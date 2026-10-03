//! Seat presence: the session state of each seat as reported by nodes
//! (`POST /v1/presence`). An entry that has not been refreshed within
//! [`TTL`] counts as absent (offline). Changes are broadcast to
//! `GET /v1/events` subscribers as `presence` events.
//!
//! A single server keeps presence in memory: it changes with every turn of
//! every seat, and is worth nothing after the nodes' next report (nodes
//! report every seat when they connect and every 30 s). Several instances
//! keep it in the database (table `presence`), so every instance sees what
//! a node reported to any of them; the instance that received a report
//! announces the change, and through [`crate::cluster`] so do the others.

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
    /// Kept in the database (several instances) or here.
    shared: bool,
    mem: Mutex<HashMap<i64, HashMap<String, PresenceRec>>>,
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

/// Merge reports into `prev` (seat -> entry); returns the entries that
/// changed or were absent, as stored. The same rule as the store's.
fn merge(
    prev: &mut HashMap<String, PresenceRec>,
    recs: Vec<PresenceRec>,
    now: i64,
) -> Vec<PresenceRec> {
    let mut changed = Vec::new();
    for mut rec in recs {
        let old = prev.get(&rec.seat).filter(|p| fresh(p, now));
        rec.updated_at = now;
        rec.since = match old {
            Some(o) if o.state == rec.state => o.since,
            _ => rec.since.min(now),
        };
        if old.is_none_or(|o| !o.same_as(&rec)) {
            changed.push(rec.clone());
        }
        prev.insert(rec.seat.clone(), rec);
    }
    changed
}

impl PresenceBoard {
    /// Presence of a single instance (in memory).
    pub fn new(store: Arc<dyn Store>) -> Self {
        PresenceBoard {
            store,
            shared: false,
            mem: Mutex::new(HashMap::new()),
            orgs: Mutex::new(HashMap::new()),
        }
    }

    /// Presence shared by several instances (in the database).
    pub fn shared(store: Arc<dyn Store>) -> Self {
        PresenceBoard {
            shared: true,
            ..PresenceBoard::new(store)
        }
    }

    async fn rows(&self, org_id: i64, seat: Option<&str>) -> amail_store::Result<Vec<PresenceRec>> {
        if self.shared {
            return self.store.presence(org_id, seat).await;
        }
        let mem = self.mem.lock().unwrap();
        Ok(mem
            .get(&org_id)
            .map(|m| {
                m.values()
                    .filter(|r| seat.is_none_or(|s| s == r.seat))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
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
        let changed = if self.shared {
            self.store.put_presence(org_id, &recs, ttl_ms()).await?
        } else {
            merge(
                self.mem.lock().unwrap().entry(org_id).or_default(),
                recs,
                now,
            )
        };
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
        match self.rows(org_id, None).await {
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
            .rows(org_id, Some(seat))
            .await?
            .iter()
            .find(|r| r.seat == seat && fresh(r, now))
            .map(wire))
    }

    /// Every seat of the org with a recent report.
    pub async fn snapshot(&self, org_id: i64) -> amail_store::Result<Vec<(String, Presence)>> {
        let now = amail_store::now_ms();
        Ok(self
            .rows(org_id, None)
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
