//! In-memory rate limiters for the protective limits (see
//! [`crate::config::Limits`]) and the per-org count of open event streams.
//! They are per server instance.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Token buckets, one per key. At most [`MAX_KEYS`] keys are tracked; while
/// that many are, new keys share one overflow bucket, so that many distinct
/// keys (client addresses, say) cannot grow the map without bound. Buckets
/// that are full again carry no information and are dropped by
/// [`Buckets::prune`], which the server calls on a timer.
pub struct Buckets<K> {
    rate: f64,
    burst: f64,
    max_keys: usize,
    inner: Mutex<Inner<K>>,
}

struct Inner<K> {
    map: HashMap<K, (f64, Instant)>,
    overflow: (f64, Instant),
}

/// Keys tracked per limiter.
pub const MAX_KEYS: usize = 100_000;

impl<K: Hash + Eq + Clone> Buckets<K> {
    /// `rate` tokens per second, at most `burst` saved up.
    pub fn new(rate: f64, burst: f64) -> Self {
        Self::with_max_keys(rate, burst, MAX_KEYS)
    }

    pub fn with_max_keys(rate: f64, burst: f64, max_keys: usize) -> Self {
        let burst = burst.max(1.0);
        Buckets {
            rate,
            burst,
            max_keys,
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                overflow: (burst, Instant::now()),
            }),
        }
    }

    pub fn per_hour(n: f64, burst: f64) -> Self {
        Self::new(n / 3600.0, burst)
    }

    /// Give back a token taken for something that then did not happen.
    pub fn refund(&self, key: &K) {
        let mut inner = self.inner.lock().unwrap();
        let burst = self.burst;
        let slot = match inner.map.get_mut(key) {
            Some(slot) => slot,
            None => &mut inner.overflow,
        };
        slot.0 = (slot.0 + 1.0).min(burst);
    }

    /// Take a token, or say in how many milliseconds one is available.
    pub fn take(&self, key: &K) -> Result<(), u64> {
        let now = Instant::now();
        let mut inner = self.inner.lock().unwrap();
        let inner = &mut *inner;
        let (tokens, at) = if inner.map.contains_key(key) || inner.map.len() < self.max_keys {
            inner.map.entry(key.clone()).or_insert((self.burst, now))
        } else {
            &mut inner.overflow
        };
        let have = (*tokens + now.duration_since(*at).as_secs_f64() * self.rate).min(self.burst);
        *at = now;
        if have >= 1.0 {
            *tokens = have - 1.0;
            Ok(())
        } else {
            *tokens = have;
            let wait = if self.rate > 0.0 {
                (1.0 - have) / self.rate
            } else {
                3600.0
            };
            Err((wait * 1000.0).ceil().max(1.0) as u64)
        }
    }

    /// Forget keys whose bucket is full again.
    pub fn prune(&self) {
        let now = Instant::now();
        let (rate, burst) = (self.rate, self.burst);
        self.inner
            .lock()
            .unwrap()
            .map
            .retain(|_, (t, at)| *t + now.duration_since(*at).as_secs_f64() * rate < burst);
    }

    /// Keys tracked.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
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
    pub link_requests: Option<Buckets<i64>>,
    pub registrations: Option<Buckets<String>>,
    pub streams: Arc<StreamCount>,
}

impl Limiters {
    pub fn new(l: &crate::config::Limits) -> Self {
        Limiters {
            global_send: l.global_send_per_sec.map(|r| Buckets::new(r, r)),
            org_send: l.org_send_per_sec.map(|r| Buckets::new(r, r)),
            token_requests: l.token_requests_per_sec.map(|r| Buckets::new(r, r * 2.0)),
            link_requests: l
                .link_requests_per_hour
                .map(|n| Buckets::per_hour(n, (n / 4.0).max(1.0))),
            registrations: l
                .registrations_per_hour
                .map(|n| Buckets::per_hour(n, (n / 2.0).max(1.0))),
            streams: Arc::new(StreamCount::new(l.org_streams)),
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
        if let Some(b) = &self.link_requests {
            b.prune();
        }
        if let Some(b) = &self.registrations {
            b.prune();
        }
    }
}

/// Open event streams per org, with an optional cap.
pub struct StreamCount {
    max: Option<usize>,
    open: Mutex<HashMap<i64, usize>>,
}

/// One open stream; dropping it ends the count.
pub struct StreamGuard {
    count: Arc<StreamCount>,
    org_id: i64,
}

impl Drop for StreamGuard {
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

impl StreamCount {
    pub fn new(max: Option<usize>) -> Self {
        StreamCount {
            max,
            open: Mutex::new(HashMap::new()),
        }
    }

    /// Count a new stream of the org, or `None` when the org is at its cap.
    pub fn open(self: &Arc<Self>, org_id: i64) -> Option<StreamGuard> {
        let mut open = self.open.lock().unwrap();
        let n = open.entry(org_id).or_default();
        if self.max.is_some_and(|m| *n >= m) {
            return None;
        }
        *n += 1;
        Some(StreamGuard {
            count: self.clone(),
            org_id,
        })
    }

    /// Orgs with open streams.
    pub fn orgs(&self) -> Vec<i64> {
        self.open.lock().unwrap().keys().copied().collect()
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

    /// Many keys cannot grow the map without bound: past the cap they share
    /// one bucket, and keys whose bucket is full again are pruned.
    #[test]
    fn bounded_keys() {
        let b = Buckets::with_max_keys(0.0, 1.0, 3);
        for k in 0..3 {
            assert!(b.take(&k).is_ok());
        }
        assert_eq!(b.len(), 3);
        // New keys share the overflow bucket (one token here).
        assert!(b.take(&10).is_ok());
        assert!(b.take(&11).is_err());
        assert_eq!(b.len(), 3);
        // Known keys keep their own bucket.
        assert!(b.take(&0).is_err());
        let r = Buckets::with_max_keys(1000.0, 1.0, 3);
        for k in 0..3 {
            assert!(r.take(&k).is_ok());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        r.prune();
        assert!(r.is_empty());
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
