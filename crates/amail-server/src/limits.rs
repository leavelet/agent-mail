//! In-memory rate limiters for the protective limits (see
//! [`crate::config::Limits`]) and the per-org count of open event streams.
//! They are per server instance.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Token buckets, one per key. At most [`MAX_KEYS`] keys are tracked, so
/// that many distinct keys (client addresses, say) cannot grow the map
/// without bound. When a new key arrives while the map is full, the buckets
/// closest to full are dropped to make room (a full bucket carries no
/// information; an emptied one is what limits a client). A client that
/// fills the map with emptied buckets therefore cannot refuse service to
/// other clients: a new key always gets a bucket of its own. Buckets that
/// are full again are dropped by [`Buckets::prune`], which the server calls
/// on a timer.
pub struct Buckets<K> {
    rate: f64,
    burst: f64,
    max_keys: usize,
    map: Mutex<HashMap<K, (f64, Instant)>>,
}

/// Keys tracked per limiter.
pub const MAX_KEYS: usize = 100_000;

impl<K: Hash + Eq + Clone> Buckets<K> {
    /// `rate` tokens per second, at most `burst` saved up.
    pub fn new(rate: f64, burst: f64) -> Self {
        Self::with_max_keys(rate, burst, MAX_KEYS)
    }

    pub fn with_max_keys(rate: f64, burst: f64, max_keys: usize) -> Self {
        Buckets {
            rate,
            burst: burst.max(1.0),
            max_keys: max_keys.max(1),
            map: Mutex::new(HashMap::new()),
        }
    }

    pub fn per_hour(n: f64, burst: f64) -> Self {
        Self::new(n / 3600.0, burst)
    }

    fn level(&self, (tokens, at): (f64, Instant), now: Instant) -> f64 {
        (tokens + now.duration_since(at).as_secs_f64() * self.rate).min(self.burst)
    }

    /// Make room in a full map: drop the fullest 1/64 of the buckets (at
    /// least one), so that the cost of the scan is spread over the next
    /// inserts.
    fn evict(&self, map: &mut HashMap<K, (f64, Instant)>, now: Instant) {
        let n = (map.len() / 64).max(1);
        let mut levels: Vec<(f64, K)> = map
            .iter()
            .map(|(k, v)| (self.level(*v, now), k.clone()))
            .collect();
        if n < levels.len() {
            levels.select_nth_unstable_by(n - 1, |a, b| b.0.total_cmp(&a.0));
            levels.truncate(n);
        }
        for (_, k) in levels {
            map.remove(&k);
        }
    }

    /// Give back a token taken for something that then did not happen.
    pub fn refund(&self, key: &K) {
        let mut map = self.map.lock().unwrap();
        if let Some(slot) = map.get_mut(key) {
            slot.0 = (slot.0 + 1.0).min(self.burst);
        }
    }

    /// Whether a token is available for `key`, without taking it; if not,
    /// in how many milliseconds one is.
    pub fn check(&self, key: &K) -> Result<(), u64> {
        let now = Instant::now();
        let map = self.map.lock().unwrap();
        match map.get(key) {
            Some(v) => self.wait(self.level(*v, now)),
            None => Ok(()),
        }
    }

    fn wait(&self, have: f64) -> Result<(), u64> {
        if have >= 1.0 {
            return Ok(());
        }
        let wait = if self.rate > 0.0 {
            (1.0 - have) / self.rate
        } else {
            3600.0
        };
        Err((wait * 1000.0).ceil().max(1.0) as u64)
    }

    /// Take a token, or say in how many milliseconds one is available.
    pub fn take(&self, key: &K) -> Result<(), u64> {
        let now = Instant::now();
        let mut map = self.map.lock().unwrap();
        if !map.contains_key(key) && map.len() >= self.max_keys {
            self.evict(&mut map, now);
        }
        let slot = map.entry(key.clone()).or_insert((self.burst, now));
        let have = self.level(*slot, now);
        *slot = (have, now);
        self.wait(have)?;
        slot.0 = have - 1.0;
        Ok(())
    }

    /// Forget keys whose bucket is full again.
    pub fn prune(&self) {
        let now = Instant::now();
        let (rate, burst) = (self.rate, self.burst);
        self.map
            .lock()
            .unwrap()
            .retain(|_, (t, at)| *t + now.duration_since(*at).as_secs_f64() * rate < burst);
    }

    /// Keys tracked.
    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The limiters of one server instance.
pub struct Limiters {
    pub global_send: Option<Buckets<()>>,
    pub org_send: Option<Buckets<i64>>,
    /// Keyed by `(is_session, id)`: a token id or a browser session's token.
    pub token_requests: Option<Buckets<(u8, i64)>>,
    /// Requests of each org, all its tokens and sessions together.
    pub org_requests: Option<Buckets<i64>>,
    /// The org of each token or session (by hash) seen lately, so that a
    /// request of an org over its request limit is refused before the
    /// database is asked who sent it.
    pub credential_orgs: CredentialOrgs,
    pub link_requests: Option<Buckets<i64>>,
    pub registrations: Option<Buckets<String>>,
    /// Registrations per IPv6 /48 network, checked before the per-address
    /// limit (see [`amail_proto::limits::REGISTRATION_NET48_FACTOR`]).
    pub registration_nets: Option<Buckets<String>>,
    pub streams: Arc<StreamCount>,
    /// Webhook notices in flight per receiving org for its own mail.
    pub webhooks: Arc<StreamCount>,
    /// Webhook notices in flight per receiving org for mail from other
    /// orgs, all of them together.
    pub webhooks_external: Arc<StreamCount>,
    /// Webhook notices in flight per (receiving org, sending org), for mail
    /// from another org.
    pub webhooks_from: Arc<StreamCount<(i64, i64)>>,
    /// Requests whose database cost depends on the data (search, archive,
    /// threads, ...) in flight, per org and in all.
    pub queries: QuerySlots,
}

impl Limiters {
    pub fn new(l: &crate::config::Limits) -> Self {
        Limiters {
            global_send: l.global_send_per_sec.map(|r| Buckets::new(r, r)),
            org_send: l.org_send_per_sec.map(|r| Buckets::new(r, r)),
            token_requests: l.token_requests_per_sec.map(|r| Buckets::new(r, r * 2.0)),
            org_requests: l.org_requests_per_sec.map(|r| Buckets::new(r, r * 2.0)),
            credential_orgs: CredentialOrgs::default(),
            link_requests: l
                .link_requests_per_hour
                .map(|n| Buckets::per_hour(n, (n / 4.0).max(1.0))),
            registrations: l
                .registrations_per_hour
                .map(|n| Buckets::per_hour(n, (n / 2.0).max(1.0))),
            registration_nets: l.registrations_per_hour.map(|n| {
                let f = amail_proto::limits::REGISTRATION_NET48_FACTOR;
                Buckets::per_hour(n * f, (n / 2.0).max(1.0) * f)
            }),
            streams: Arc::new(StreamCount::new(l.org_streams)),
            webhooks: Arc::new(StreamCount::new(l.org_webhooks_in_flight)),
            webhooks_external: Arc::new(StreamCount::new(l.org_webhooks_in_flight)),
            webhooks_from: Arc::new(StreamCount::new(
                l.org_webhooks_in_flight.map(|n| (n / 4).max(1)),
            )),
            queries: QuerySlots::new(l.org_queries, l.queries),
        }
    }

    /// Drop the state of keys that are not limited at the moment.
    pub fn prune(&self) {
        if let Some(b) = &self.global_send {
            b.prune();
        }
        if let Some(b) = &self.org_send {
            b.prune();
        }
        if let Some(b) = &self.token_requests {
            b.prune();
        }
        if let Some(b) = &self.org_requests {
            b.prune();
        }
        if let Some(b) = &self.link_requests {
            b.prune();
        }
        if let Some(b) = &self.registrations {
            b.prune();
        }
        if let Some(b) = &self.registration_nets {
            b.prune();
        }
        self.queries.prune();
    }
}

/// Credential hash -> org, for at most [`MAX_KEYS`] credentials (when full,
/// it starts over). Only used to refuse early: a request is accepted only
/// after the database has checked its credential.
#[derive(Default)]
pub struct CredentialOrgs {
    map: Mutex<HashMap<String, i64>>,
}

impl CredentialOrgs {
    pub fn get(&self, hash: &str) -> Option<i64> {
        self.map.lock().unwrap().get(hash).copied()
    }

    pub fn insert(&self, hash: &str, org_id: i64) {
        let mut m = self.map.lock().unwrap();
        if m.len() >= MAX_KEYS && !m.contains_key(hash) {
            m.clear();
        }
        m.insert(hash.to_string(), org_id);
    }
}

/// Slots for requests whose database cost depends on the data: at most
/// `per_org` of one org and `total` of all orgs run at a time. A request
/// waits up to [`QUERY_SLOT_WAIT`] for a slot.
pub struct QuerySlots {
    per_org: Option<usize>,
    orgs: Mutex<HashMap<i64, Arc<tokio::sync::Semaphore>>>,
    total: Option<Arc<tokio::sync::Semaphore>>,
}

/// How long a request waits for a query slot before it is refused.
pub const QUERY_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// A query slot; dropping it frees the slot.
pub struct QuerySlot {
    _org: Option<tokio::sync::OwnedSemaphorePermit>,
    _total: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// Which limit refused a query slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRefused {
    Org,
    Total,
}

impl QuerySlots {
    pub fn new(per_org: Option<usize>, total: Option<usize>) -> Self {
        QuerySlots {
            per_org,
            orgs: Mutex::new(HashMap::new()),
            total: total.map(|n| Arc::new(tokio::sync::Semaphore::new(n))),
        }
    }

    /// Take a slot for the org, waiting up to [`QUERY_SLOT_WAIT`] in all.
    pub async fn acquire(&self, org_id: i64) -> Result<QuerySlot, SlotRefused> {
        let deadline = tokio::time::Instant::now() + QUERY_SLOT_WAIT;
        let org = match self.per_org {
            None => None,
            Some(n) => {
                let sem = self
                    .orgs
                    .lock()
                    .unwrap()
                    .entry(org_id)
                    .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(n)))
                    .clone();
                match tokio::time::timeout_at(deadline, sem.acquire_owned()).await {
                    Ok(Ok(p)) => Some(p),
                    _ => return Err(SlotRefused::Org),
                }
            }
        };
        let total = match &self.total {
            None => None,
            Some(sem) => match tokio::time::timeout_at(deadline, sem.clone().acquire_owned()).await
            {
                Ok(Ok(p)) => Some(p),
                _ => return Err(SlotRefused::Total),
            },
        };
        Ok(QuerySlot {
            _org: org,
            _total: total,
        })
    }

    /// Forget orgs with no slot in use.
    pub fn prune(&self) {
        let Some(n) = self.per_org else { return };
        self.orgs
            .lock()
            .unwrap()
            .retain(|_, s| Arc::strong_count(s) > 1 || s.available_permits() < n);
    }
}

/// Things in progress per key (an org, usually), with an optional cap.
pub struct StreamCount<K = i64> {
    max: Option<usize>,
    open: Mutex<HashMap<K, usize>>,
}

/// One counted thing in progress; dropping it ends the count.
pub struct StreamGuard<K: Hash + Eq + Clone = i64> {
    count: Arc<StreamCount<K>>,
    org_id: K,
}

impl<K: Hash + Eq + Clone> Drop for StreamGuard<K> {
    fn drop(&mut self) {
        let mut open = self.count.open.lock().unwrap();
        if let Some(n) = open.get_mut(&self.org_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                open.remove(&self.org_id);
            }
        }
    }
}

impl<K: Hash + Eq + Clone> StreamCount<K> {
    pub fn new(max: Option<usize>) -> Self {
        StreamCount {
            max,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// Count a new stream of the org, or `None` when the org is at its cap.
    pub fn open(self: &Arc<Self>, org_id: K) -> Option<StreamGuard<K>> {
        let mut open = self.open.lock().unwrap();
        let n = open.entry(org_id.clone()).or_default();
        if self.max.is_some_and(|m| *n >= m) {
            return None;
        }
        *n += 1;
        Some(StreamGuard {
            count: self.clone(),
            org_id,
        })
    }

    /// Keys (orgs) with something in progress.
    pub fn orgs(&self) -> Vec<K> {
        self.open.lock().unwrap().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets() {
        let b = Buckets::new(10.0, 2.0);
        assert!(b.take(&1).is_ok());
        assert!(b.take(&1).is_ok());
        let wait = b.take(&1).unwrap_err();
        assert!((1..=100).contains(&wait), "{wait}");
        assert!(b.take(&2).is_ok());
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(b.take(&1).is_ok());
    }

    /// Many keys cannot grow the map without bound, and keys whose bucket is
    /// full again are pruned.
    #[test]
    fn bounded_keys() {
        let b = Buckets::with_max_keys(0.0, 1.0, 3);
        for k in 0..10 {
            assert!(b.take(&k).is_ok());
            assert!(b.len() <= 3);
        }
        let r = Buckets::with_max_keys(1000.0, 1.0, 3);
        for k in 0..3 {
            assert!(r.take(&k).is_ok());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        r.prune();
        assert!(r.is_empty());
    }

    /// A client that fills the map with emptied buckets cannot refuse other
    /// clients: every new key gets a bucket of its own, and the buckets that
    /// are dropped to make room are the fullest ones.
    #[test]
    fn full_map_does_not_refuse_new_keys() {
        let b = Buckets::with_max_keys(0.0, 2.0, 1000);
        // One key with a full bucket, then 999 emptied ones.
        b.take(&-1).unwrap();
        b.refund(&-1);
        for k in 0..999 {
            b.take(&k).unwrap();
            b.take(&k).unwrap();
            assert!(b.take(&k).is_err());
        }
        assert_eq!(b.len(), 1000);
        for k in 10_000..10_100 {
            assert!(b.take(&k).is_ok(), "new key {k} refused");
        }
        assert!(b.len() <= 1000);
        // Fuller buckets are dropped before emptied ones: most of the
        // emptied keys are still limited.
        let limited = (0..999).filter(|k| b.check(k).is_err()).count();
        assert!(limited >= 900, "{limited}");
    }

    #[tokio::test]
    async fn query_slots() {
        let q = QuerySlots::new(Some(2), Some(3));
        let a = q.acquire(1).await.unwrap();
        let _b = q.acquire(1).await.unwrap();
        assert_eq!(q.acquire(1).await.err(), Some(SlotRefused::Org));
        let _c = q.acquire(2).await.unwrap();
        assert_eq!(q.acquire(3).await.err(), Some(SlotRefused::Total));
        drop(a);
        assert!(q.acquire(1).await.is_ok());
        // A slot freed while a request waits goes to that request.
        let held = q.acquire(1).await.unwrap();
        let wait = q.acquire(1);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(held);
        });
        assert!(wait.await.is_ok());
        q.prune();
    }

    #[test]
    fn stream_cap() {
        let c = Arc::new(StreamCount::new(Some(2)));
        let a = c.open(1).unwrap();
        let _b = c.open(1).unwrap();
        assert!(c.open(1).is_none());
        assert!(c.open(2).is_some());
        drop(a);
        assert!(c.open(1).is_some());
    }
}
