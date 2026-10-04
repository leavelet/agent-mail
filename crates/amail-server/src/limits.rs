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
    /// Node notice reports per org (always on).
    pub node_notices: Buckets<i64>,
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
            node_notices: Buckets::per_hour(
                amail_proto::limits::NODE_NOTICES_PER_HOUR,
                amail_proto::limits::NODE_NOTICE_BURST,
            ),
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
        self.node_notices.prune();
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

/// The pools of query slots. Search and archive requests can look at many
/// messages (in steps, for a bounded time per request); the other requests
/// whose cost depends on the data look at a bounded amount each. Separate
/// pools keep one kind from taking every slot of the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pool {
    /// `/archive/search`, `/archive/messages`.
    Scan,
    /// Threads, delivery status, `/changes`, large `/inbox` and `/sent`
    /// pages.
    Other,
    /// Requests whose cost has a fixed upper bound: `/overview`,
    /// `/attention`, `/lists`, `/lists/{name}`. Limited per org only, so
    /// that other orgs cannot make them wait.
    Light,
}

/// Slots for requests whose database cost depends on the data, in
/// [`Pool`]s: in each, at most `per_org` of one org and (except
/// [`Pool::Light`]) `total` of all orgs run at a time. A request waits up
/// to [`QUERY_SLOT_WAIT`] for a slot.
/// When a slot frees, it goes to the waiting request of the org that holds
/// the fewest slots of the pool at that moment, then of the org that was
/// given a slot least recently, then the longest waiting: an org with one
/// request waiting is served before another org's next one, however many
/// requests that org has waiting.
pub struct QuerySlots {
    scan: FairSlots,
    other: FairSlots,
    light: FairSlots,
}

/// How long a request waits for a query slot before it is refused.
pub const QUERY_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// Which limit refused a query slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotRefused {
    Org,
    Total,
}

impl QuerySlots {
    pub fn new(per_org: Option<usize>, total: Option<usize>) -> Self {
        QuerySlots {
            scan: FairSlots::new(per_org, total),
            other: FairSlots::new(per_org, total),
            light: FairSlots::new(per_org, None),
        }
    }

    /// Take a slot of the pool for the org, waiting up to
    /// [`QUERY_SLOT_WAIT`].
    pub async fn acquire(&self, pool: Pool, org_id: i64) -> Result<QuerySlot, SlotRefused> {
        match pool {
            Pool::Scan => self.scan.acquire(org_id).await,
            Pool::Other => self.other.acquire(org_id).await,
            Pool::Light => self.light.acquire(org_id).await,
        }
    }

    /// Requests waiting for a slot, in both pools.
    pub fn waiting(&self) -> usize {
        self.scan.waiting() + self.other.waiting() + self.light.waiting()
    }
}

/// One pool of query slots (see [`QuerySlots`]).
struct FairSlots {
    per_org: usize,
    total: usize,
    state: Arc<Mutex<FairState>>,
}

#[derive(Default)]
struct FairState {
    /// Per org: slots held, and when it was last given one. Orgs holding
    /// none are dropped after [`FAIR_MEMORY`].
    orgs: HashMap<i64, (usize, Instant)>,
    in_use: usize,
    /// Requests waiting, in arrival order.
    waiters: Vec<Waiter>,
    next_id: u64,
}

/// How long a pool remembers when an org holding no slot was last given
/// one.
const FAIR_MEMORY: std::time::Duration = std::time::Duration::from_secs(10);

struct Waiter {
    id: u64,
    org: i64,
    grant: tokio::sync::oneshot::Sender<()>,
}

impl FairState {
    fn held(&self, org: i64) -> usize {
        self.orgs.get(&org).map_or(0, |o| o.0)
    }

    /// Give free slots to waiting requests: each to the waiter of the org
    /// that holds the fewest slots, then of the org that was given one
    /// least recently, then the oldest.
    fn dispatch(&mut self, per_org: usize, total: usize) {
        while self.in_use < total {
            let best = self
                .waiters
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    let (held, last) = match self.orgs.get(&w.org) {
                        Some((n, t)) => (*n, Some(*t)),
                        None => (0, None),
                    };
                    (held < per_org).then_some((held, last, w.id, i))
                })
                .min();
            let Some((_, _, _, i)) = best else { return };
            let w = self.waiters.remove(i);
            if w.grant.send(()).is_ok() {
                let o = self.orgs.entry(w.org).or_insert((0, Instant::now()));
                o.0 += 1;
                o.1 = Instant::now();
                self.in_use += 1;
            }
        }
    }

    fn release(&mut self, org: i64) {
        if let Some(o) = self.orgs.get_mut(&org) {
            o.0 = o.0.saturating_sub(1);
        }
        self.in_use -= 1;
        if self.orgs.len() > 1024 {
            self.orgs
                .retain(|_, (n, t)| *n > 0 || t.elapsed() < FAIR_MEMORY);
        }
    }
}

impl FairSlots {
    fn new(per_org: Option<usize>, total: Option<usize>) -> Self {
        FairSlots {
            per_org: per_org.unwrap_or(usize::MAX),
            total: total.unwrap_or(usize::MAX),
            state: Default::default(),
        }
    }

    fn waiting(&self) -> usize {
        self.state.lock().unwrap().waiters.len()
    }

    async fn acquire(&self, org_id: i64) -> Result<QuerySlot, SlotRefused> {
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let mut pending = {
            let mut st = self.state.lock().unwrap();
            let id = st.next_id;
            st.next_id += 1;
            st.waiters.push(Waiter {
                id,
                org: org_id,
                grant: tx,
            });
            st.dispatch(self.per_org, self.total);
            Pending {
                slots: self,
                id,
                org: org_id,
                done: false,
            }
        };
        let granted = tokio::time::timeout(QUERY_SLOT_WAIT, &mut rx).await.is_ok() || {
            let mut st = self.state.lock().unwrap();
            match st.waiters.iter().position(|w| w.id == pending.id) {
                Some(i) => {
                    st.waiters.remove(i);
                    false
                }
                // Granted while the wait ran out (grants are sent under the
                // lock, so it has been sent).
                None => true,
            }
        };
        pending.done = true;
        if !granted {
            let held = self.state.lock().unwrap().held(org_id);
            return Err(if held >= self.per_org {
                SlotRefused::Org
            } else {
                SlotRefused::Total
            });
        }
        Ok(QuerySlot {
            state: self.state.clone(),
            org: org_id,
            per_org: self.per_org,
            total: self.total,
        })
    }
}

/// A request waiting for a slot. If the request is dropped while it waits
/// (the client went away), this takes it out of the queue, or gives back
/// the slot it was granted meanwhile.
struct Pending<'a> {
    slots: &'a FairSlots,
    id: u64,
    org: i64,
    done: bool,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut st = self.slots.state.lock().unwrap();
        match st.waiters.iter().position(|w| w.id == self.id) {
            Some(i) => {
                st.waiters.remove(i);
            }
            None => {
                st.release(self.org);
                st.dispatch(self.slots.per_org, self.slots.total);
            }
        }
    }
}

/// A query slot; dropping it frees the slot and gives it to the next
/// waiting request.
pub struct QuerySlot {
    state: Arc<Mutex<FairState>>,
    org: i64,
    per_org: usize,
    total: usize,
}

impl Drop for QuerySlot {
    fn drop(&mut self) {
        let mut st = self.state.lock().unwrap();
        st.release(self.org);
        st.dispatch(self.per_org, self.total);
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
        let a = q.acquire(Pool::Scan, 1).await.unwrap();
        let _b = q.acquire(Pool::Scan, 1).await.unwrap();
        assert_eq!(q.acquire(Pool::Scan, 1).await.err(), Some(SlotRefused::Org));
        let _c = q.acquire(Pool::Scan, 2).await.unwrap();
        assert_eq!(
            q.acquire(Pool::Scan, 3).await.err(),
            Some(SlotRefused::Total)
        );
        // The other pools are separate, and the light one has no total.
        let _d = q.acquire(Pool::Other, 3).await.unwrap();
        let _e: Vec<_> = futures::future::join_all((10..20).map(|o| q.acquire(Pool::Light, o)))
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
        drop(a);
        assert!(q.acquire(Pool::Scan, 1).await.is_ok());
        // A slot freed while a request waits goes to that request.
        let held = q.acquire(Pool::Scan, 1).await.unwrap();
        let wait = q.acquire(Pool::Scan, 1);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            drop(held);
        });
        assert!(wait.await.is_ok());
        assert_eq!(q.waiting(), 0);
    }

    /// A freed slot goes to the org holding the fewest slots: an org with
    /// one request waiting is served before an org's tenth, whatever the
    /// order of arrival.
    #[tokio::test]
    async fn query_slots_are_fair_between_orgs() {
        let q = Arc::new(QuerySlots::new(Some(10), Some(2)));
        let first = q.acquire(Pool::Scan, 1).await.unwrap();
        let second = q.acquire(Pool::Scan, 1).await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        // Nine more requests of org 1, then one of org 2.
        for org in [1, 1, 1, 1, 1, 1, 1, 1, 1, 2] {
            let (q, order) = (q.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let slot = q.acquire(Pool::Scan, org).await;
                order.lock().unwrap().push(org);
                if let Ok(s) = slot {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    drop(s);
                }
            }));
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(q.waiting(), 10);
        drop(first);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        assert_eq!(order.lock().unwrap().as_slice(), &[2]);
        drop(second);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(order.lock().unwrap().len(), 10);

        // One slot per org: org 1's next request waited longer, but org 2
        // has not had a slot lately, so it goes first.
        let q = Arc::new(QuerySlots::new(Some(1), Some(1)));
        let order = Arc::new(Mutex::new(Vec::new()));
        let held = q.acquire(Pool::Scan, 1).await.unwrap();
        let mut tasks = Vec::new();
        for org in [1, 2] {
            let (q, order) = (q.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let s = q.acquire(Pool::Scan, org).await.unwrap();
                order.lock().unwrap().push(org);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                drop(s);
            }));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(held);
        for t in tasks {
            t.await.unwrap();
        }
        assert_eq!(order.lock().unwrap().as_slice(), &[2, 1]);
    }

    /// A request dropped while it waits leaves the queue, and one dropped
    /// after it was granted gives its slot back.
    #[tokio::test]
    async fn dropped_waiters_free_their_place() {
        let q = Arc::new(QuerySlots::new(Some(1), Some(1)));
        let held = q.acquire(Pool::Other, 1).await.unwrap();
        let q2 = q.clone();
        let t = tokio::spawn(async move { q2.acquire(Pool::Other, 2).await.map(|_| ()) });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(q.waiting(), 1);
        t.abort();
        let _ = t.await;
        assert_eq!(q.waiting(), 0);
        drop(held);
        // Granted, then dropped before it was used: the slot comes back.
        let held = q.acquire(Pool::Other, 1).await.unwrap();
        let mut fut = Box::pin(q.acquire(Pool::Other, 2));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut fut)
                .await
                .is_err()
        );
        drop(held);
        drop(fut);
        let _again = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            q.acquire(Pool::Other, 3),
        )
        .await
        .expect("slot given back")
        .unwrap();
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
