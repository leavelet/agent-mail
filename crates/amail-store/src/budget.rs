//! Per-seat send budget in memory (SQLite: one server process per
//! database): a token bucket refilled at the org's `send_rate_per_min`,
//! holding at most `send_burst` tokens. Postgres keeps the same bucket in
//! the `send_budget` table, shared by all server instances.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Budget {
    buckets: Mutex<HashMap<i64, Bucket>>,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Budget {
    /// Take one token for `seat_id`, or return how many milliseconds to wait.
    pub fn take(&self, seat_id: i64, per_min: i64, burst: i64) -> Result<(), u64> {
        self.try_acquire_at(seat_id, per_min, burst, Instant::now())
            .map_err(|d| (d.as_millis() as u64).max(1))
    }

    fn try_acquire_at(
        &self,
        seat_id: i64,
        per_min: i64,
        burst: i64,
        now: Instant,
    ) -> Result<(), Duration> {
        let burst = burst.max(1) as f64;
        let rate = per_min.max(0) as f64 / 60.0; // tokens per second
        let mut buckets = self.buckets.lock().unwrap();
        let b = buckets.entry(seat_id).or_insert(Bucket {
            tokens: burst,
            last: now,
        });
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * rate).min(burst);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            return Ok(());
        }
        if rate == 0.0 {
            return Err(Duration::from_secs(3600));
        }
        Err(Duration::from_secs_f64((1.0 - b.tokens) / rate))
    }

    /// Give back a token taken for a send that then failed.
    pub fn refund(&self, seat_id: i64, burst: i64) {
        if let Some(b) = self.buckets.lock().unwrap().get_mut(&seat_id) {
            b.tokens = (b.tokens + 1.0).min(burst.max(1) as f64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket() {
        let l = Budget::default();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(l.try_acquire_at(1, 60, 3, t0).is_ok());
        }
        let wait = l.try_acquire_at(1, 60, 3, t0).unwrap_err();
        assert!((wait.as_secs_f64() - 1.0).abs() < 1e-6);
        // Another seat has its own bucket.
        assert!(l.try_acquire_at(2, 60, 3, t0).is_ok());
        // One second later, one token is back.
        let t1 = t0 + Duration::from_secs(1);
        assert!(l.try_acquire_at(1, 60, 3, t1).is_ok());
        assert!(l.try_acquire_at(1, 60, 3, t1).is_err());
        l.refund(1, 3);
        assert!(l.try_acquire_at(1, 60, 3, t1).is_ok());
    }
}
