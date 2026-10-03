//! Mailbox change notifications for `GET /v1/events`.
//!
//! [`EventBus`] is the seam for multi-instance deployments: the in-process
//! [`Hub`] is enough for a single server; a Postgres `LISTEN/NOTIFY`
//! implementation can replace it without touching the handlers.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use tokio::sync::broadcast;

/// One mailbox change: `seat`'s sequence counter reached `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusEvent {
    /// Opaque, per-org, increasing; used as the SSE event id.
    pub id: String,
    pub seat: String,
    pub seq: i64,
}

pub struct Subscription {
    pub rx: broadcast::Receiver<BusEvent>,
    /// Events after the requested `Last-Event-ID`, or `None` if they are no
    /// longer (or were never) available, in which case the caller must send
    /// a full snapshot of the org's seat counters.
    pub replay: Option<Vec<BusEvent>>,
    /// Id of the latest event published before subscribing, if any.
    pub latest_id: Option<String>,
}

pub trait EventBus: Send + Sync + 'static {
    /// Publish `(seat name, seq)` changes for one org.
    fn publish(&self, org_id: i64, events: &[(String, i64)]);
    /// Subscribe to an org's events, replaying from `last_event_id`.
    fn subscribe(&self, org_id: i64, last_event_id: Option<&str>) -> Subscription;
}

const RECENT: usize = 4096;
const CHANNEL: usize = 1024;

/// In-process event bus: one broadcast channel per org plus a ring buffer of
/// recent events for `Last-Event-ID` replay. Event ids are
/// `<epoch>-<counter>`; an id from another server run triggers a snapshot.
pub struct Hub {
    epoch: String,
    orgs: Mutex<HashMap<i64, OrgChannel>>,
}

struct OrgChannel {
    tx: broadcast::Sender<BusEvent>,
    next: u64,
    recent: VecDeque<(u64, BusEvent)>,
}

impl OrgChannel {
    fn new() -> Self {
        OrgChannel {
            tx: broadcast::channel(CHANNEL).0,
            next: 1,
            recent: VecDeque::new(),
        }
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        let epoch = format!("{:x}", amail_store::now_ms());
        Hub {
            epoch,
            orgs: Mutex::new(HashMap::new()),
        }
    }

    fn parse_id(&self, id: &str) -> Option<u64> {
        let (epoch, n) = id.rsplit_once('-')?;
        if epoch != self.epoch {
            return None;
        }
        n.parse().ok()
    }
}

impl EventBus for Hub {
    fn publish(&self, org_id: i64, events: &[(String, i64)]) {
        if events.is_empty() {
            return;
        }
        let mut orgs = self.orgs.lock().unwrap();
        let ch = orgs.entry(org_id).or_insert_with(OrgChannel::new);
        for (seat, seq) in events {
            let n = ch.next;
            ch.next += 1;
            let ev = BusEvent {
                id: format!("{}-{n}", self.epoch),
                seat: seat.clone(),
                seq: *seq,
            };
            if ch.recent.len() == RECENT {
                ch.recent.pop_front();
            }
            ch.recent.push_back((n, ev.clone()));
            let _ = ch.tx.send(ev);
        }
    }

    fn subscribe(&self, org_id: i64, last_event_id: Option<&str>) -> Subscription {
        let mut orgs = self.orgs.lock().unwrap();
        let ch = orgs.entry(org_id).or_insert_with(OrgChannel::new);
        let rx = ch.tx.subscribe();
        let latest_id = ch.recent.back().map(|(_, e)| e.id.clone());
        let replay = match last_event_id {
            None => Some(Vec::new()),
            Some(id) => match self.parse_id(id) {
                // Replayable if nothing after `n` has been evicted.
                Some(n)
                    if n < ch.next
                        && ch.recent.front().is_none_or(|(first, _)| *first <= n + 1) =>
                {
                    Some(
                        ch.recent
                            .iter()
                            .filter(|(k, _)| *k > n)
                            .map(|(_, e)| e.clone())
                            .collect(),
                    )
                }
                _ => None,
            },
        };
        Subscription {
            rx,
            replay,
            latest_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_and_snapshot() {
        let hub = Hub::new();
        hub.publish(1, &[("a".into(), 1), ("b".into(), 1)]);
        let s = hub.subscribe(1, None);
        assert_eq!(s.replay, Some(vec![]));
        let first = s.latest_id.clone().unwrap();
        hub.publish(1, &[("a".into(), 2)]);
        let s = hub.subscribe(1, Some(&first));
        let r = s.replay.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].seat.as_str(), r[0].seq), ("a", 2));
        assert!(hub.subscribe(1, Some("bogus-1")).replay.is_none());
        assert!(
            hub.subscribe(1, Some(&format!("{}-999", hub.epoch)))
                .replay
                .is_none()
        );
        // Other orgs are separate.
        assert!(hub.subscribe(2, None).latest_id.is_none());
        // Eviction makes old ids unreplayable.
        for i in 0..RECENT as i64 + 1 {
            hub.publish(1, &[("a".into(), 10 + i)]);
        }
        assert!(hub.subscribe(1, Some(&first)).replay.is_none());
    }
}
