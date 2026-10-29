//! Several server instances on one Postgres database.
//!
//! The database stays the only source of truth; what the instances tell
//! each other is small and only about changes:
//!
//! - **Notifications** go through Postgres `LISTEN/NOTIFY` on one channel
//!   ([`CHANNEL`]): mailbox events (`seat`, `seq`), "presence of these seats
//!   changed" (the receiver reads the rows), and node commands (a few
//!   short fields). Each payload stays below [`MAX_PAYLOAD`] bytes; larger
//!   batches are split. They are sent in batches by one background task,
//!   after the change was committed. An instance whose listening
//!   connection drops (or stays silent past [`SILENCE`]) reconnects and
//!   ends all of its event streams with a new epoch, so clients reconnect
//!   and get a snapshot instead of a replay that may have holes.
//! - **Node streams**: each instance records in `node_streams` how many node
//!   event streams it holds per org (heartbeat every [`HEARTBEAT`]); the
//!   others read the sums, so `node_connected` and the `node_offline`
//!   answers are the same on every instance.
//! - **Conversation mirrors** carry too much data for `NOTIFY` and must not
//!   be stored: an instance with browsers watching a seat records that in
//!   `mirror_watch` (with its URL from `server_instances`); an instance that
//!   receives a node's transcript post forwards it over HTTP to the
//!   watching instances, with the node's own credentials.

use crate::AppState;
use crate::events::{EventBus, Hub, Subscription};
use amail_proto::NodeCommand;
use amail_store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// The `LISTEN/NOTIFY` channel.
pub const CHANNEL: &str = "amail_cluster";
/// Postgres limits a payload to 8000 bytes.
pub const MAX_PAYLOAD: usize = 7900;
/// How often an instance renews its row and its node stream counts.
pub const HEARTBEAT: Duration = Duration::from_secs(10);
/// An instance not heard of for this long is gone.
pub const INSTANCE_TTL: Duration = Duration::from_secs(30);
/// The listener pings itself every [`HEARTBEAT`]; hearing nothing at all for
/// this long means the connection is dead.
pub const SILENCE: Duration = Duration::from_secs(35);

/// One message between instances.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Wire {
    /// Mailbox events of one org: `(seat, seq)`.
    #[serde(rename = "m")]
    Mailbox { o: i64, e: Vec<(String, i64)> },
    /// Presence of these seats changed (read it from the database).
    #[serde(rename = "p")]
    Presence { o: i64, s: Vec<String> },
    /// A command for the org's nodes.
    #[serde(rename = "c")]
    Command { o: i64, c: NodeCommand },
    /// These tokens (ids) and browser sessions (hashes) were revoked: end
    /// their streams.
    #[serde(rename = "r")]
    Ended { k: Vec<i64>, s: Vec<String> },
    /// A seat or list of the org changed.
    #[serde(rename = "d")]
    Directory {
        o: i64,
        d: amail_proto::DirectoryEvent,
    },
    /// A task of the org changed.
    #[serde(rename = "k")]
    Task {
        o: i64,
        v: amail_proto::TaskBusEvent,
    },
    /// A document of the org changed.
    #[serde(rename = "b")]
    Doc { o: i64, v: amail_proto::DocBusEvent },
    /// A request of the org changed.
    #[serde(rename = "q")]
    Request {
        o: i64,
        v: amail_proto::RequestBusEvent,
    },
    /// The sender's own liveness ping (others ignore it).
    #[serde(rename = "h")]
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    /// The sending instance.
    i: String,
    #[serde(flatten)]
    w: Wire,
}

/// Split a message into payloads of at most [`MAX_PAYLOAD`] bytes.
fn payloads(instance: &str, w: Wire) -> Vec<String> {
    let enc = |w: Wire| {
        serde_json::to_string(&Envelope {
            i: instance.to_string(),
            w,
        })
        .unwrap_or_default()
    };
    let one = enc(w.clone());
    if one.len() <= MAX_PAYLOAD {
        return vec![one];
    }
    match w {
        Wire::Mailbox { o, e } if e.len() > 1 => {
            let (a, b) = e.split_at(e.len() / 2);
            let mut out = payloads(instance, Wire::Mailbox { o, e: a.to_vec() });
            out.extend(payloads(instance, Wire::Mailbox { o, e: b.to_vec() }));
            out
        }
        Wire::Presence { o, s } if s.len() > 1 => {
            let (a, b) = s.split_at(s.len() / 2);
            let mut out = payloads(instance, Wire::Presence { o, s: a.to_vec() });
            out.extend(payloads(instance, Wire::Presence { o, s: b.to_vec() }));
            out
        }
        _ => {
            tracing::warn!("dropping a cluster message of {} bytes", one.len());
            Vec::new()
        }
    }
}

/// This instance's part in a cluster.
pub struct Cluster {
    /// Random, per process.
    pub id: String,
    /// Where other instances reach this one (for conversation mirrors).
    pub url: Option<String>,
    db_url: String,
    out: mpsc::UnboundedSender<Wire>,
    rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<Wire>>>,
    http: reqwest::Client,
    /// Set once the listener is first established.
    listening: tokio::sync::watch::Sender<bool>,
}

impl Cluster {
    pub fn new(db_url: &str, url: Option<String>) -> Arc<Cluster> {
        let (out, rx) = mpsc::unbounded_channel();
        Arc::new(Cluster {
            id: hex::encode(rand::random::<[u8; 8]>()),
            url: url.map(|u| u.trim_end_matches('/').to_string()),
            db_url: db_url.to_string(),
            out,
            rx: std::sync::Mutex::new(Some(rx)),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(10))
                .no_proxy()
                .build()
                .expect("building the HTTP client"),
            listening: tokio::sync::watch::channel(false).0,
        })
    }

    /// Wait until this instance receives the other instances' notifications
    /// (or `max` has passed). An instance that serves requests before that
    /// would miss events and then cut its event streams to recover.
    pub async fn wait_listening(&self, max: Duration) -> bool {
        let mut rx = self.listening.subscribe();
        let ready = tokio::time::timeout(max, rx.wait_for(|up| *up)).await;
        matches!(ready, Ok(Ok(_)))
    }

    /// Tell the other instances (after the change was committed).
    pub fn notify(&self, w: Wire) {
        let _ = self.out.send(w);
    }

    /// Start the background tasks of this instance.
    pub fn start(self: &Arc<Self>, st: &AppState) -> Vec<tokio::task::JoinHandle<()>> {
        let c = self.clone();
        st.nodes.set_forward(Box::new(move |o, cmd| {
            c.notify(Wire::Command { o, c: cmd.clone() })
        }));
        let rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .expect("cluster started twice");
        vec![
            tokio::spawn(self.clone().publisher(rx)),
            tokio::spawn(self.clone().listener(st.clone())),
            tokio::spawn(self.clone().membership(st.clone())),
        ]
    }

    /// Send queued messages, a batch per round trip.
    async fn publisher(self: Arc<Self>, mut rx: mpsc::UnboundedReceiver<Wire>) {
        use sqlx::Connection;
        let mut conn: Option<sqlx::PgConnection> = None;
        let mut batch = Vec::new();
        while rx.recv_many(&mut batch, 512).await > 0 {
            let texts: Vec<String> = batch
                .drain(..)
                .flat_map(|w| payloads(&self.id, w))
                .collect();
            for attempt in 0..3 {
                if conn.is_none() {
                    match sqlx::PgConnection::connect(&self.db_url).await {
                        Ok(c) => conn = Some(c),
                        Err(e) => {
                            tracing::warn!("cluster: connecting to send notifications: {e}");
                            tokio::time::sleep(Duration::from_millis(200 << attempt)).await;
                            continue;
                        }
                    }
                }
                let c = conn.as_mut().expect("connected");
                let r = sqlx::query("SELECT pg_notify($1, p) FROM unnest($2::text[]) AS p")
                    .bind(CHANNEL)
                    .bind(&texts)
                    .execute(&mut *c)
                    .await;
                match r {
                    Ok(_) => break,
                    Err(e) => {
                        tracing::warn!("cluster: sending notifications: {e}");
                        conn = None;
                        if attempt == 2 {
                            tracing::warn!(
                                "cluster: dropped {} notifications; nodes resynchronize \
                                 within their resync interval",
                                texts.len()
                            );
                        }
                    }
                }
            }
        }
    }

    /// Receive the other instances' messages.
    async fn listener(self: Arc<Self>, st: AppState) {
        let mut backoff = Duration::from_millis(200);
        loop {
            let mut l = match sqlx::postgres::PgListener::connect(&self.db_url).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("cluster: connecting the listener: {e}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                    continue;
                }
            };
            if let Err(e) = l.listen(CHANNEL).await {
                tracing::warn!("cluster: LISTEN failed: {e}");
                tokio::time::sleep(backoff).await;
                continue;
            }
            backoff = Duration::from_millis(200);
            // Anything may have happened while not listening.
            st.bus.lost_events();
            self.listening.send_replace(true);
            tracing::info!("cluster: listening as instance {}", self.id);
            loop {
                let next = tokio::time::timeout(SILENCE, l.try_recv()).await;
                match next {
                    Ok(Ok(Some(n))) => self.dispatch(&st, n.payload()).await,
                    Ok(Ok(None)) => {
                        tracing::warn!("cluster: listener connection lost; reconnecting");
                        st.bus.lost_events();
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("cluster: listener: {e}; reconnecting");
                        break;
                    }
                    Err(_) => {
                        tracing::warn!("cluster: listener silent for {SILENCE:?}; reconnecting");
                        break;
                    }
                }
            }
            st.bus.lost_events();
            tokio::time::sleep(backoff).await;
        }
    }

    async fn dispatch(&self, st: &AppState, payload: &str) {
        let Ok(env) = serde_json::from_str::<Envelope>(payload) else {
            tracing::debug!("cluster: ignoring {payload:?}");
            return;
        };
        if env.i == self.id {
            return;
        }
        match env.w {
            Wire::Mailbox { o, e } => st.bus.deliver(o, &e),
            Wire::Presence { o, s } => st.presence.changed_elsewhere(o, &s).await,
            Wire::Command { o, c } => {
                st.nodes.deliver(o, c);
            }
            Wire::Ended { k, s } => st.revocations.end(
                k.into_iter()
                    .map(crate::revoke::Cred::Token)
                    .chain(s.into_iter().map(crate::revoke::Cred::Session)),
            ),
            Wire::Directory { o, d } => st.directory.deliver(o, d),
            Wire::Task { o, v } => st.tasks.deliver(o, v),
            Wire::Doc { o, v } => st.docs.deliver(o, v),
            Wire::Request { o, v } => st.requests.deliver(o, v),
            Wire::Ping => {}
        }
    }

    /// Heartbeat, node stream counts, and cleanup.
    async fn membership(self: Arc<Self>, st: AppState) {
        let mut last_full = Instant::now() - HEARTBEAT;
        let mut last_counts: Vec<(i64, i64)> = Vec::new();
        loop {
            let full = last_full.elapsed() >= HEARTBEAT;
            let mut counts = st.nodes.local_counts();
            counts.sort_unstable();
            if full || counts != last_counts {
                let r = async {
                    st.store
                        .heartbeat_instance(&self.id, self.url.as_deref())
                        .await?;
                    st.store.set_node_streams(&self.id, &counts).await
                }
                .await;
                match r {
                    Ok(()) => {
                        last_counts = counts;
                        if full {
                            last_full = Instant::now();
                            self.notify(Wire::Ping);
                        }
                    }
                    Err(e) => tracing::warn!("cluster: heartbeat: {e}"),
                }
            }
            let since = amail_store::now_ms() - INSTANCE_TTL.as_millis() as i64;
            match st.store.node_streams(&self.id, since).await {
                Ok(rows) => st
                    .nodes
                    .set_remote(rows.into_iter().collect::<HashMap<_, _>>()),
                Err(e) => tracing::debug!("cluster: reading node streams: {e}"),
            }
            tokio::select! {
                _ = st.nodes.changed.notified() => {
                    // Let changes that come together settle.
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    }

    /// Leave the cluster (on shutdown).
    pub async fn leave(&self, store: &dyn Store) {
        if let Err(e) = store.remove_instance(&self.id).await {
            tracing::debug!("cluster: leaving: {e}");
        }
    }

    /// Forward a node's transcript post to the other instances watching the
    /// seat; returns their watchers.
    pub async fn relay_transcript(
        &self,
        store: &dyn Store,
        org_id: i64,
        seat: &str,
        authorization: &str,
        body: &amail_proto::TranscriptPost,
    ) -> u32 {
        let targets = match store.mirror_watchers(org_id, seat).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("cluster: reading mirror watchers: {e}");
                return 0;
            }
        };
        let calls = targets
            .into_iter()
            .filter(|(id, _)| *id != self.id)
            .filter_map(|(id, url)| match url {
                Some(u) => Some(u),
                None => {
                    tracing::warn!(
                        "cluster: instance {id} watches {seat} but has no URL \
                         (start it with --advertise-url)"
                    );
                    None
                }
            })
            .map(|url| {
                let req = self
                    .http
                    .post(format!(
                        "{}/v1/seats/{}/transcript",
                        url.trim_end_matches('/'),
                        seat
                    ))
                    .header(reqwest::header::AUTHORIZATION, authorization)
                    .header(RELAY_HEADER, &self.id)
                    .json(body);
                async move {
                    match req.send().await {
                        Ok(r) if r.status().is_success() => r
                            .json::<amail_proto::TranscriptPostResponse>()
                            .await
                            .map(|x| x.subscribers)
                            .unwrap_or(0),
                        Ok(r) => {
                            tracing::warn!(
                                "cluster: relaying a transcript to {url}: HTTP {}",
                                r.status()
                            );
                            0
                        }
                        Err(e) => {
                            tracing::warn!("cluster: relaying a transcript to {url}: {e}");
                            0
                        }
                    }
                }
            });
        futures::future::join_all(calls).await.into_iter().sum()
    }
}

/// Header that marks a transcript post relayed from another instance (it
/// is applied there and not relayed again).
pub const RELAY_HEADER: &str = "x-amail-relay";

/// The event bus of an instance in a cluster: its own [`Hub`], plus every
/// published event sent to the other instances.
pub struct ClusterBus {
    pub hub: Hub,
    pub cluster: Arc<Cluster>,
}

impl EventBus for ClusterBus {
    fn publish(&self, org_id: i64, events: &[(String, i64)]) {
        if events.is_empty() {
            return;
        }
        self.hub.publish(org_id, events);
        self.cluster.notify(Wire::Mailbox {
            o: org_id,
            e: events.to_vec(),
        });
    }

    fn subscribe(&self, org_id: i64, last_event_id: Option<&str>) -> Subscription {
        self.hub.subscribe(org_id, last_event_id)
    }

    fn deliver(&self, org_id: i64, events: &[(String, i64)]) {
        self.hub.publish(org_id, events);
    }

    fn lost_events(&self) {
        self.hub.reset();
    }

    fn close_org(&self, org_id: i64) {
        self.hub.close_org(org_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_split() {
        let e: Vec<(String, i64)> = (0..2000).map(|i| (format!("seat-{i:05}"), i)).collect();
        let parts = payloads("abc", Wire::Mailbox { o: 7, e: e.clone() });
        assert!(parts.len() > 1);
        let mut back = Vec::new();
        for p in &parts {
            assert!(p.len() <= MAX_PAYLOAD);
            let env: Envelope = serde_json::from_str(p).unwrap();
            assert_eq!(env.i, "abc");
            match env.w {
                Wire::Mailbox { o: 7, e } => back.extend(e),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(back, e);
        let one = payloads("x", Wire::Ping);
        assert_eq!(one, vec![r#"{"i":"x","t":"h"}"#.to_string()]);
    }
}
