//! Mailbox change notifications for `GET /v1/events`.
//!
//! [`EventBus`] is the seam for multi-instance deployments: the in-process
//! [`Hub`] is enough for a single server; with several instances on one
//! Postgres database, [`crate::cluster::ClusterBus`] also forwards every
//! event to the other instances (`LISTEN/NOTIFY`), which feed it into their
//! own hub.

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
    /// Publish `(seat name, seq)` changes for one org (to every instance).
    fn publish(&self, org_id: i64, events: &[(String, i64)]);
    /// Subscribe to an org's events, replaying from `last_event_id`.
    fn subscribe(&self, org_id: i64, last_event_id: Option<&str>) -> Subscription;
    /// Hand events that another instance published to this instance's
    /// subscribers.
    fn deliver(&self, org_id: i64, events: &[(String, i64)]) {
        self.publish(org_id, events)
    }
    /// Events may have been missed: make subscribers reconnect and resync.
    fn lost_events(&self) {}
    /// End this instance's subscriptions of one org (it was suspended or
    /// deleted).
    fn close_org(&self, org_id: i64);
}

const RECENT: usize = 4096;
const CHANNEL: usize = 1024;

/// In-process event bus: one broadcast channel per org plus a ring buffer of
/// recent events for `Last-Event-ID` replay. Event ids are
/// `<epoch>-<counter>`; an id from another server run triggers a snapshot.
pub struct Hub {
    inner: Mutex<Inner>,
}

struct Inner {
    epoch: String,
    orgs: HashMap<i64, OrgChannel>,
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

fn new_epoch() -> String {
    format!("{:x}{:04x}", amail_store::now_ms(), rand::random::<u16>())
}

impl Hub {
    pub fn new() -> Self {
        Hub {
            inner: Mutex::new(Inner {
                epoch: new_epoch(),
                orgs: HashMap::new(),
            }),
        }
    }

    /// Events may have been missed (the instance lost its connection to the
    /// other instances): end every subscription and start a new epoch, so
    /// that reconnecting clients get a snapshot instead of a replay.
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.epoch = new_epoch();
        inner.orgs.clear();
    }
}

fn parse_id(epoch: &str, id: &str) -> Option<u64> {
    let (e, n) = id.rsplit_once('-')?;
    if e != epoch {
        return None;
    }
    n.parse().ok()
}

impl EventBus for Hub {
    fn publish(&self, org_id: i64, events: &[(String, i64)]) {
        if events.is_empty() {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let Inner { epoch, orgs } = &mut *inner;
        let ch = orgs.entry(org_id).or_insert_with(OrgChannel::new);
        for (seat, seq) in events {
            let n = ch.next;
            ch.next += 1;
            let ev = BusEvent {
                id: format!("{epoch}-{n}"),
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

    fn close_org(&self, org_id: i64) {
        self.inner.lock().unwrap().orgs.remove(&org_id);
    }

    fn subscribe(&self, org_id: i64, last_event_id: Option<&str>) -> Subscription {
        let mut inner = self.inner.lock().unwrap();
        let Inner { epoch, orgs } = &mut *inner;
        let ch = orgs.entry(org_id).or_insert_with(OrgChannel::new);
        let rx = ch.tx.subscribe();
        let latest_id = ch.recent.back().map(|(_, e)| e.id.clone());
        let replay = match last_event_id {
            None => Some(Vec::new()),
            Some(id) => match parse_id(epoch, id) {
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
        let epoch = hub.inner.lock().unwrap().epoch.clone();
        assert!(
            hub.subscribe(1, Some(&format!("{epoch}-999")))
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

        // A reset ends subscriptions and makes every old id unreplayable.
        let mut s = hub.subscribe(1, None);
        let last = hub.subscribe(1, None).latest_id.unwrap();
        hub.reset();
        assert!(matches!(
            s.rx.try_recv(),
            Err(broadcast::error::TryRecvError::Closed)
        ));
        assert!(hub.subscribe(1, Some(&last)).replay.is_none());
    }
}
