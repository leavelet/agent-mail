//! Per-org counts of recent mail (`mail_last_hour` and `mail_last_day` of
//! `GET /v1/overview`), kept per minute in the store's `org_activity`.
//!
//! A send adds one to the current minute of the sending org and of every
//! org that received the message, in memory; a background task adds the
//! counts to the store about once a second, so sending costs no extra
//! database write. A count is the store's rows from the window's first
//! minute on plus what this instance has not written yet (counts of other
//! instances appear within about a second). Reading it is one index range of
//! at most one row per minute.

use amail_store::Store;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MINUTE_MS: i64 = 60_000;

#[derive(Default)]
pub struct Activity {
    /// `(org, minute) -> messages` not yet in the store.
    pending: Mutex<HashMap<(i64, i64), i64>>,
    /// Held for writing while counts move from `pending` to the store, so
    /// that a reader sees them in exactly one of the two.
    moving: tokio::sync::RwLock<()>,
}

impl Activity {
    /// One message, sent or received by each of `orgs`, now.
    pub fn record(&self, orgs: impl IntoIterator<Item = i64>) {
        let minute = amail_store::now_ms() / MINUTE_MS;
        let mut orgs: Vec<i64> = orgs.into_iter().collect();
        orgs.sort_unstable();
        orgs.dedup();
        let mut p = self.pending.lock().unwrap();
        for org in orgs {
            *p.entry((org, minute)).or_default() += 1;
        }
    }

    /// Messages the org sent or received from the minute of `since_ms` on.
    pub async fn count(
        &self,
        store: &dyn Store,
        org_id: i64,
        since_ms: i64,
    ) -> amail_store::Result<i64> {
        let since = since_ms / MINUTE_MS;
        let _r = self.moving.read().await;
        let stored = store.activity(org_id, since).await?;
        let local: i64 = self
            .pending
            .lock()
            .unwrap()
            .iter()
            .filter(|((o, m), _)| *o == org_id && *m >= since)
            .map(|(_, n)| n)
            .sum();
        Ok(stored + local)
    }

    /// Add the counts this instance holds to the store.
    pub async fn flush(&self, store: &dyn Store) -> amail_store::Result<()> {
        let _w = self.moving.write().await;
        let batch: Vec<(i64, i64, i64)> = std::mem::take(&mut *self.pending.lock().unwrap())
            .into_iter()
            .map(|((o, m), n)| (o, m, n))
            .collect();
        if batch.is_empty() {
            return Ok(());
        }
        if let Err(e) = store.add_activity(&batch).await {
            let mut p = self.pending.lock().unwrap();
            for (o, m, n) in batch {
                *p.entry((o, m)).or_default() += n;
            }
            return Err(e);
        }
        Ok(())
    }
}

/// Write the counts about once a second.
pub async fn run(activity: Arc<Activity>, store: Arc<dyn Store>) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = activity.flush(&*store).await {
            tracing::warn!("writing mail counts: {e}");
        }
    }
}
