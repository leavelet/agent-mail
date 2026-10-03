//! The conversation mirror: browsers watch a seat's transcript
//! (`GET /v1/seats/{name}/transcript`, SSE); the node hosting the seat reads
//! the harness's session file and posts what it finds
//! (`POST /v1/seats/{name}/transcript`).
//!
//! Nothing is stored. While a seat has watchers the server keeps its most
//! recent events in memory (at most [`BUFFER`]) so that a second browser
//! starts with context, and renews the node's lease (a `transcript` command)
//! every third of the lease. When the last watcher leaves the buffer is
//! dropped and renewals stop; the node stops when its lease runs out.
//!
//! With several instances, the browser and the node may talk to different
//! ones: the watching instance records the watch in the database (renewed
//! with the lease), and an instance that receives a node's post forwards it
//! to the watching instances over HTTP (see [`crate::cluster`]).

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path};
use amail_proto::{
    NodeCommand, SeatKind, TokenScope, TranscriptEvent, TranscriptPost, TranscriptPostResponse,
    TranscriptState, TranscriptStatus,
};
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt, stream};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Events kept per watched seat.
pub const BUFFER: usize = 1000;
/// Events accepted in one post.
const MAX_POST: usize = 1000;
/// Longest text kept per event, in characters (nodes cut earlier).
const MAX_TEXT: usize = 16_000;

/// Lease and backfill settings.
#[derive(Debug, Clone)]
pub struct MirrorConfig {
    pub lease: Duration,
    pub backfill: u32,
    /// How long to wait for any node to answer before saying so.
    pub answer_timeout: Duration,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        MirrorConfig {
            lease: Duration::from_secs(30),
            backfill: 200,
            answer_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
enum Msg {
    Status(TranscriptStatus),
    Reset,
    Events(Vec<TranscriptEvent>),
}

struct Chan {
    watchers: usize,
    buffer: VecDeque<TranscriptEvent>,
    status: TranscriptStatus,
    tx: broadcast::Sender<Msg>,
    /// The node that answers for the seat (the first to post).
    node: Option<String>,
    answered: bool,
    renewer: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Default)]
pub struct MirrorHub {
    seats: Mutex<HashMap<(i64, String), Chan>>,
}

/// One watcher; dropping it may end the channel.
struct Watch {
    hub: Arc<MirrorHub>,
    key: (i64, String),
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut seats = self.hub.seats.lock().unwrap();
        let last = match seats.get_mut(&self.key) {
            Some(c) => {
                c.watchers = c.watchers.saturating_sub(1);
                c.watchers == 0
            }
            None => false,
        };
        if last && let Some(t) = seats.remove(&self.key).and_then(|c| c.renewer) {
            t.abort();
        }
    }
}

fn status(state: TranscriptState, message: &str) -> TranscriptStatus {
    TranscriptStatus {
        message: Some(message.to_string()),
        ..TranscriptStatus::new(state)
    }
}

impl MirrorHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seats being watched in the org (for tests and status).
    pub fn watched(&self, org_id: i64) -> Vec<String> {
        let seats = self.seats.lock().unwrap();
        let mut v: Vec<String> = seats
            .keys()
            .filter(|(o, _)| *o == org_id)
            .map(|(_, s)| s.clone())
            .collect();
        v.sort();
        v
    }

    fn set_status(&self, key: &(i64, String), s: TranscriptStatus) {
        let mut seats = self.seats.lock().unwrap();
        if let Some(c) = seats.get_mut(key) {
            c.status = s.clone();
            let _ = c.tx.send(Msg::Status(s));
        }
    }

    /// A node's post. Returns the number of watchers (0: nobody watches,
    /// the data is dropped).
    fn post(&self, key: &(i64, String), p: TranscriptPost) -> u32 {
        let mut seats = self.seats.lock().unwrap();
        let Some(c) = seats.get_mut(key) else {
            return 0;
        };
        match (&c.node, &p.node) {
            (Some(a), Some(b)) if a != b => return c.watchers as u32,
            (None, Some(b)) => c.node = Some(b.clone()),
            _ => {}
        }
        c.answered = true;
        if p.reset {
            c.buffer.clear();
            let _ = c.tx.send(Msg::Reset);
        }
        if let Some(s) = p.status
            && s != c.status
        {
            c.status = s.clone();
            let _ = c.tx.send(Msg::Status(s));
        }
        if !p.events.is_empty() {
            let mut events = p.events;
            if events.len() > MAX_POST {
                events.drain(..events.len() - MAX_POST);
            }
            for e in &mut events {
                if e.text.chars().count() > MAX_TEXT {
                    e.text = e.text.chars().take(MAX_TEXT).collect();
                    e.truncated = true;
                }
            }
            for e in &events {
                if c.buffer.len() == BUFFER {
                    c.buffer.pop_front();
                }
                c.buffer.push_back(e.clone());
            }
            let _ = c.tx.send(Msg::Events(events));
        }
        c.watchers as u32
    }
}

/// Keep the hosting node forwarding while the seat is watched.
async fn renew(st: AppState, key: (i64, String)) {
    let cfg = st.mirror_config.clone();
    // The first command that reaches a node asks for a backfill (a node
    // still forwarding from an earlier watch sends it again); renewals don't.
    let mut need_backfill = true;
    loop {
        if let Some(c) = &st.cluster {
            let expires = amail_store::now_ms() + cfg.lease.as_millis() as i64;
            if let Err(e) = st
                .store
                .watch_mirror(key.0, &key.1, &c.id, Some(expires))
                .await
            {
                tracing::warn!("recording a mirror watch: {e}");
            }
        }
        let node = match st.presence.get(key.0, &key.1).await {
            Ok(p) => p.and_then(|p| p.node),
            Err(e) => {
                tracing::debug!("reading presence: {e}");
                None
            }
        };
        let cmd = NodeCommand {
            op: "transcript".into(),
            seat: key.1.clone(),
            lease_ms: Some(cfg.lease.as_millis() as u64),
            backfill: need_backfill.then_some(cfg.backfill),
            node,
        };
        if st.nodes.send(key.0, cmd) == 0 {
            need_backfill = true;
            st.mirror.set_status(
                &key,
                status(
                    TranscriptState::NodeOffline,
                    "No node of this org is connected, so the session cannot be read.",
                ),
            );
        } else if need_backfill {
            need_backfill = false;
            let (st2, key2) = (st.clone(), key.clone());
            tokio::spawn(async move {
                tokio::time::sleep(cfg.answer_timeout).await;
                let answered = st2
                    .mirror
                    .seats
                    .lock()
                    .unwrap()
                    .get(&key2)
                    .is_none_or(|c| c.answered);
                if !answered {
                    st2.mirror.set_status(
                        &key2,
                        status(
                            TranscriptState::Unknown,
                            "No connected node has a session for this seat.",
                        ),
                    );
                }
            });
        }
        tokio::time::sleep(cfg.lease / 3).await;
    }
}

/// `GET /v1/seats/{name}/transcript` (user tokens): server-sent `status`,
/// `reset` and `events` events.
pub async fn watch(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    auth.require_user()?;
    let seat = st
        .store
        .seat_by_name(auth.org.id, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats have sessions",
        ));
    }
    let key = (auth.org.id, seat.name.clone());
    let (initial, rx, guard) = {
        let mut seats = st.mirror.seats.lock().unwrap();
        let c = seats.entry(key.clone()).or_insert_with(|| Chan {
            watchers: 0,
            buffer: VecDeque::new(),
            status: status(
                TranscriptState::Waiting,
                "Asking the node that hosts this seat for its session.",
            ),
            tx: broadcast::channel(256).0,
            node: None,
            answered: false,
            renewer: None,
        });
        c.watchers += 1;
        if c.renewer.is_none() {
            c.renewer = Some(tokio::spawn(renew(st.clone(), key.clone())));
        }
        let mut initial = vec![Msg::Status(c.status.clone())];
        if !c.buffer.is_empty() {
            initial.push(Msg::Events(c.buffer.iter().cloned().collect()));
        }
        let guard = Watch {
            hub: st.mirror.clone(),
            key,
        };
        (initial, c.tx.subscribe(), guard)
    };
    // A watcher that falls behind is disconnected; the browser reconnects
    // and starts again from the buffer.
    let live = BroadcastStream::new(rx).take_while(|r| std::future::ready(r.is_ok()));
    let stream = stream::iter(initial.into_iter().map(Ok))
        .chain(live)
        .filter_map(move |m| {
            let _keep = &guard;
            std::future::ready(m.ok().map(|m| Ok(sse(m))))
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn sse(m: Msg) -> Event {
    match m {
        Msg::Status(s) => Event::default()
            .event("status")
            .data(serde_json::to_string(&s).unwrap_or_default()),
        Msg::Reset => Event::default().event("reset").data("{}"),
        Msg::Events(events) => Event::default().event("events").data(
            serde_json::to_string(&serde_json::json!({ "events": events })).unwrap_or_default(),
        ),
    }
}

/// `POST /v1/seats/{name}/transcript` (node tokens). Forwarded to the other
/// instances whose browsers watch the seat, unless it was relayed here.
pub async fn post(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TranscriptPost>,
) -> ApiResult<Json<TranscriptPostResponse>> {
    if auth.scope != TokenScope::Node {
        return Err(ApiError::forbidden("requires a node token"));
    }
    let relayed = headers.contains_key(crate::cluster::RELAY_HEADER);
    let remote = match (&st.cluster, relayed) {
        (Some(c), false) => {
            let authorization = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            c.relay_transcript(&*st.store, auth.org.id, &name, &authorization, &body)
                .await
        }
        _ => 0,
    };
    let subscribers = st.mirror.post(&(auth.org.id, name), body) + remote;
    Ok(Json(TranscriptPostResponse { subscribers }))
}
