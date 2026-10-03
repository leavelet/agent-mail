//! Seat presence: the session state of each seat as reported by nodes
//! (`POST /v1/presence`). Kept in memory only; an entry that has not been
//! refreshed within [`TTL`] counts as absent (offline). Changes are broadcast
//! to `GET /v1/events` subscribers as `presence` events.

use amail_proto::{Presence, PresenceEvent, PresenceUpdate, SeatState};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::broadcast;

/// How long a report stays valid. Nodes re-report every seat well within it.
pub const TTL: Duration = Duration::from_secs(90);

const CHANNEL: usize = 1024;

#[derive(Default)]
pub struct PresenceBoard {
    orgs: Mutex<HashMap<i64, OrgPresence>>,
}

struct OrgPresence {
    seats: HashMap<String, Entry>,
    tx: broadcast::Sender<PresenceEvent>,
}

impl OrgPresence {
    fn new() -> Self {
        OrgPresence {
            seats: HashMap::new(),
            tx: broadcast::channel(CHANNEL).0,
        }
    }
}

#[derive(Clone)]
struct Entry {
    state: SeatState,
    wake_failed: bool,
    harness: Option<String>,
    node: Option<String>,
    since: i64,
    updated: i64,
    attention: Option<String>,
    attention_hint: Option<String>,
    terminal_url: Option<String>,
}

impl Entry {
    fn presence(&self) -> Presence {
        Presence {
            state: self.state,
            wake_failed: self.wake_failed,
            harness: self.harness.clone(),
            node: self.node.clone(),
            since: crate::render::ts(self.since),
            updated_at: crate::render::ts(self.updated),
            attention: self.attention.clone(),
            attention_hint: self.attention_hint.clone(),
            terminal_url: self.terminal_url.clone(),
            node_connected: false,
        }
    }

    fn fresh(&self, now: i64) -> bool {
        now - self.updated <= TTL.as_millis() as i64
    }
}

impl PresenceBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a node's report. `known` filters seat names (only live seats of
    /// the org are accepted). Returns the number of entries applied.
    pub fn update(
        &self,
        org_id: i64,
        node: Option<&str>,
        updates: &[PresenceUpdate],
        known: impl Fn(&str) -> bool,
    ) -> usize {
        let now = amail_store::now_ms();
        let mut orgs = self.orgs.lock().unwrap();
        let org = orgs.entry(org_id).or_insert_with(OrgPresence::new);
        let mut applied = 0;
        for u in updates {
            if !known(&u.seat) {
                continue;
            }
            applied += 1;
            let since = u
                .since
                .as_deref()
                .and_then(crate::render::parse_ts)
                .unwrap_or(now)
                .min(now);
            let prev = org.seats.get(&u.seat).filter(|e| e.fresh(now)).cloned();
            let changed = prev.as_ref().is_none_or(|p| {
                p.state != u.state
                    || p.wake_failed != u.wake_failed
                    || p.harness != u.harness
                    || p.attention != u.attention
                    || p.attention_hint != u.attention_hint
                    || p.terminal_url != u.terminal_url
            });
            let entry = Entry {
                state: u.state,
                wake_failed: u.wake_failed,
                harness: u.harness.clone(),
                node: node.map(str::to_string),
                since: match &prev {
                    Some(p) if p.state == u.state => p.since,
                    _ => since,
                },
                updated: now,
                attention: u.attention.clone(),
                attention_hint: u.attention_hint.clone(),
                terminal_url: u.terminal_url.clone(),
            };
            if changed {
                let _ = org.tx.send(PresenceEvent {
                    seat: u.seat.clone(),
                    presence: entry.presence(),
                });
            }
            org.seats.insert(u.seat.clone(), entry);
        }
        applied
    }

    /// The seat's current presence, if a node reported it recently.
    pub fn get(&self, org_id: i64, seat: &str) -> Option<Presence> {
        let now = amail_store::now_ms();
        let orgs = self.orgs.lock().unwrap();
        orgs.get(&org_id)?
            .seats
            .get(seat)
            .filter(|e| e.fresh(now))
            .map(Entry::presence)
    }

    /// Every seat of the org with a recent report.
    pub fn snapshot(&self, org_id: i64) -> Vec<(String, Presence)> {
        let now = amail_store::now_ms();
        let orgs = self.orgs.lock().unwrap();
        orgs.get(&org_id)
            .map(|o| {
                o.seats
                    .iter()
                    .filter(|(_, e)| e.fresh(now))
                    .map(|(n, e)| (n.clone(), e.presence()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn subscribe(&self, org_id: i64) -> broadcast::Receiver<PresenceEvent> {
        let mut orgs = self.orgs.lock().unwrap();
        orgs.entry(org_id)
            .or_insert_with(OrgPresence::new)
            .tx
            .subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(seat: &str, state: SeatState) -> PresenceUpdate {
        PresenceUpdate {
            seat: seat.into(),
            state,
            wake_failed: false,
            harness: Some("claude".into()),
            since: None,
            attention: None,
            attention_hint: None,
            terminal_url: None,
        }
    }

    #[test]
    fn update_and_events() {
        let b = PresenceBoard::new();
        let mut rx = b.subscribe(1);
        assert_eq!(
            b.update(1, Some("h"), &[up("a", SeatState::Busy)], |_| true),
            1
        );
        assert_eq!(rx.try_recv().unwrap().presence.state, SeatState::Busy);
        // Same state again: no event, `since` kept.
        let since = b.get(1, "a").unwrap().since;
        b.update(1, Some("h"), &[up("a", SeatState::Busy)], |_| true);
        assert!(rx.try_recv().is_err());
        assert_eq!(b.get(1, "a").unwrap().since, since);
        b.update(1, None, &[up("a", SeatState::Idle)], |_| true);
        assert_eq!(rx.try_recv().unwrap().presence.state, SeatState::Idle);
        // Unknown seats are dropped; other orgs are separate.
        assert_eq!(
            b.update(1, None, &[up("zz", SeatState::Idle)], |s| s != "zz"),
            0
        );
        assert!(b.get(1, "zz").is_none());
        assert!(b.get(2, "a").is_none());
    }
}
