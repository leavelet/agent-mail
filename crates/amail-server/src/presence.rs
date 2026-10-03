//! Seat presence: the session state of each seat as reported by nodes
//! (`POST /v1/presence`). Stored in the database (table `presence`), so
//! every server instance sees what a node reported to any of them, and a
//! restarted server still knows it. An entry that has not been refreshed
//! within [`TTL`] counts as absent (offline). Changes are broadcast to
//! `GET /v1/events` subscribers as `presence` events: by the instance that
//! received the report, and through [`crate::cluster`] by the others.

use amail_proto::{Presence, PresenceEvent, PresenceUpdate};
use amail_store::{PresenceRec, Store};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast;

/// How long a report stays valid. Nodes re-report every seat well within it.
pub const TTL: Duration = Duration::from_secs(90);

const CHANNEL: usize = 1024;

pub struct PresenceBoard {
    store: Arc<dyn Store>,
    orgs: Mutex<HashMap<i64, broadcast::Sender<PresenceEvent>>>,
}

fn ttl_ms() -> i64 {
    TTL.as_millis() as i64
}

/// The wire form of a stored entry (`node_connected` is filled in later).
pub fn wire(r: &PresenceRec) -> Presence {
    Presence {
        state: r.state,
        wake_failed: r.wake_failed,
        harness: r.harness.clone(),
        node: r.node.clone(),
        since: crate::render::ts(r.since),
        updated_at: crate::render::ts(r.updated_at),
        attention: r.attention.clone(),
        attention_hint: r.attention_hint.clone(),
        terminal_url: r.terminal_url.clone(),
        node_connected: false,
    }
}

fn fresh(r: &PresenceRec, now: i64) -> bool {
    now - r.updated_at <= ttl_ms()
}

impl PresenceBoard {
    pub fn new(store: Arc<dyn Store>) -> Self {
        PresenceBoard {
            store,
            orgs: Mutex::new(HashMap::new()),
        }
    }

    fn sender(&self, org_id: i64) -> broadcast::Sender<PresenceEvent> {
        self.orgs
            .lock()
            .unwrap()
            .entry(org_id)
            .or_insert_with(|| broadcast::channel(CHANNEL).0)
            .clone()
    }

    /// Apply a node's report (seats that are not live seats of the org are
    /// dropped). Returns the seats whose presence changed; they have been
    /// announced to this instance's subscribers.
    pub async fn update(
        &self,
        org_id: i64,
        node: Option<&str>,
        updates: &[PresenceUpdate],
    ) -> amail_store::Result<Vec<String>> {
        let now = amail_store::now_ms();
        let recs: Vec<PresenceRec> = updates
            .iter()
            .map(|u| PresenceRec {
                seat: u.seat.clone(),
                state: u.state,
                wake_failed: u.wake_failed,
                harness: u.harness.clone(),
                node: node.map(str::to_string),
                since: u
                    .since
                    .as_deref()
                    .and_then(crate::render::parse_ts)
                    .unwrap_or(now)
                    .min(now),
                updated_at: now,
                attention: u.attention.clone(),
                attention_hint: u.attention_hint.clone(),
                terminal_url: u.terminal_url.clone(),
            })
            .collect();
        let changed = self.store.put_presence(org_id, &recs, ttl_ms()).await?;
        self.announce(org_id, &changed);
        Ok(changed.into_iter().map(|r| r.seat).collect())
    }

    fn announce(&self, org_id: i64, recs: &[PresenceRec]) {
        if recs.is_empty() {
            return;
        }
        let tx = self.sender(org_id);
        for r in recs {
            let _ = tx.send(PresenceEvent {
                seat: r.seat.clone(),
                presence: wire(r),
            });
        }
    }

    /// Another instance stored new presence for these seats: announce it
    /// here too.
    pub async fn changed_elsewhere(&self, org_id: i64, seats: &[String]) {
        if self
            .orgs
            .lock()
            .unwrap()
            .get(&org_id)
            .is_none_or(|tx| tx.receiver_count() == 0)
        {
            return;
        }
        match self.store.presence(org_id, None).await {
            Ok(rows) => {
                let rows: Vec<PresenceRec> = rows
                    .into_iter()
                    .filter(|r| seats.contains(&r.seat))
                    .collect();
                self.announce(org_id, &rows);
            }
            Err(e) => tracing::warn!("loading presence: {e}"),
        }
    }

    /// The seat's current presence, if a node reported it recently.
    pub async fn get(&self, org_id: i64, seat: &str) -> amail_store::Result<Option<Presence>> {
        let now = amail_store::now_ms();
        Ok(self
            .store
            .presence(org_id, Some(seat))
            .await?
            .iter()
            .find(|r| r.seat == seat && fresh(r, now))
            .map(wire))
    }

    /// Every seat of the org with a recent report.
    pub async fn snapshot(&self, org_id: i64) -> amail_store::Result<Vec<(String, Presence)>> {
        let now = amail_store::now_ms();
        Ok(self
            .store
            .presence(org_id, None)
            .await?
            .iter()
            .filter(|r| fresh(r, now))
            .map(|r| (r.seat.clone(), wire(r)))
            .collect())
    }

    pub fn subscribe(&self, org_id: i64) -> broadcast::Receiver<PresenceEvent> {
        self.sender(org_id).subscribe()
    }
}
