//! Postgres backend.
//!
//! Every write runs in its own READ COMMITTED transaction. Two locking rules
//! keep the per-seat sequence correct and deadlock-free:
//!
//! - A seat's counter (`seats.last_seq`) is advanced with a row lock that is
//!   held until commit, so for any one seat, commits happen in sequence order
//!   and a reader never sees seq N+1 before seq N.
//! - Locks are taken in a fixed order: delivery rows before seat rows, and
//!   multiple seat rows in ascending id order. Explicit row locks use
//!   `FOR NO KEY UPDATE` so they do not conflict with the `KEY SHARE` locks
//!   that foreign-key checks take on the sender's seat.
//!
//! `claim` selects candidates with `FOR UPDATE SKIP LOCKED`, so concurrent
//! claims never return the same delivery.

use crate::common::{join_addrs, line_count, meta_to_db};
use crate::model::*;
use crate::{OpenOptions, Result, StoreError, now_ms};
use futures::future::BoxFuture;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use std::str::FromStr;
use std::time::Duration;

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub(crate) const BACKEND: &'static str = "postgres";

    pub async fn open(url: &str, opts: &OpenOptions) -> Result<Self> {
        Self::open_with(PgConnectOptions::from_str(url)?, opts).await
    }

    pub async fn open_with(conn: PgConnectOptions, opts: &OpenOptions) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(opts.max_connections.max(2))
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(conn)
            .await?;
        sqlx::migrate!("./migrations/postgres").run(&pool).await?;
        Ok(PgStore { pool })
    }

    async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T>> + Send + 'static,
    {
        let mut c = self.pool.acquire().await?;
        f(&mut c).await
    }

    /// A read whose cost depends on the data (search, archive, threads): in
    /// a transaction that Postgres cancels after
    /// [`amail_proto::limits::QUERY_TIMEOUT_MS`], planned for the actual
    /// parameters (a generic plan cannot tell a small id range from a large
    /// one).
    async fn read_query<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T>> + Send + 'static,
    {
        let mut tx = self.pool.begin().await?;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = {}",
            amail_proto::limits::QUERY_TIMEOUT_MS
        ))
        .execute(&mut *tx)
        .await?;
        sqlx::query("SET LOCAL plan_cache_mode = force_custom_plan")
            .execute(&mut *tx)
            .await?;
        let out = f(&mut tx).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T>> + Send + 'static,
    {
        let mut tx = self.pool.begin().await?;
        let out = f(&mut tx).await?;
        tx.commit().await?;
        Ok(out)
    }

    async fn send_impl(&self, m: &NewMessage) -> Result<Sent> {
        let m = m.clone();
        self.write(move |c| {
            Box::pin(async move {
                let now = now_ms();
                let meta = meta_to_db(&m.meta);
                let (to, cc) = (join_addrs(&m.to_addrs), join_addrs(&m.cc_addrs));
                // Notice mail is charged to nobody (see `message_bytes`).
                let bytes = if m.notice {
                    0
                } else {
                    message_bytes(&m.subject, &m.body, &meta, &to, &cc)
                        + amail_proto::limits::STORED_BYTES_PER_DELIVERY * m.deliveries.len() as i64
                };
                let mut seat_ids: Vec<i64> = m.deliveries.iter().map(|d| d.seat_id).collect();
                seat_ids.sort_unstable();
                seat_ids.dedup();
                if seat_ids.len() != m.deliveries.len() {
                    return Err(StoreError::Invalid("duplicate recipient seat".into()));
                }
                // Lock recipient seats in id order, then advance their counters.
                sqlx::query("SELECT id FROM seats WHERE id = ANY($1) ORDER BY id FOR NO KEY UPDATE")
                    .bind(&seat_ids)
                    .execute(&mut *c)
                    .await?;
                // Advance their sequence counters and add the new deliveries
                // to their pending and unread counters (see migration 0012).
                let urgent = m.priority == amail_proto::Priority::Urgent;
                let one = |b: bool| i32::from(b);
                let (mut ids, mut pt, mut pc, mut pl, mut pu, mut pd, mut un) =
                    (vec![], vec![], vec![], vec![], vec![], vec![], vec![]);
                for d in &m.deliveries {
                    let now_ = d.mode == amail_proto::DeliveryMode::Immediate;
                    ids.push(d.seat_id);
                    pt.push(one(now_ && d.class == amail_proto::DeliveryClass::To));
                    pc.push(one(now_ && d.class == amail_proto::DeliveryClass::Cc));
                    pl.push(one(now_ && d.class == amail_proto::DeliveryClass::List));
                    pu.push(one(now_ && urgent));
                    pd.push(one(d.mode == amail_proto::DeliveryMode::Digest));
                    un.push(one(d.mode != amail_proto::DeliveryMode::Archive));
                }
                let bumped: Vec<(i64, i64)> = sqlx::query_as(
                    "UPDATE seats s SET last_seq = s.last_seq + 1, \
                     pending_to = s.pending_to + v.pt, pending_cc = s.pending_cc + v.pc, \
                     pending_list = s.pending_list + v.pl, pending_urgent = s.pending_urgent + v.pu, \
                     pending_digest = s.pending_digest + v.pd, unread = s.unread + v.un, \
                     pending_digest_since = CASE WHEN v.pd > 0 THEN COALESCE(s.pending_digest_since, $9) \
                       ELSE s.pending_digest_since END \
                     FROM UNNEST($2::bigint[], $3::int[], $4::int[], $5::int[], $6::int[], $7::int[], $8::int[]) \
                       AS v(id, pt, pc, pl, pu, pd, un) \
                     WHERE s.id = ANY($1) AND s.id = v.id RETURNING s.id, s.last_seq",
                )
                .bind(&seat_ids)
                .bind(&ids)
                .bind(&pt)
                .bind(&pc)
                .bind(&pl)
                .bind(&pu)
                .bind(&pd)
                .bind(&un)
                .bind(now)
                .fetch_all(&mut *c)
                .await?;
                let seq_of = |seat: i64| bumped.iter().find(|(id, _)| *id == seat).map(|(_, s)| *s);

                let id: i64 = sqlx::query_scalar("SELECT nextval(pg_get_serial_sequence('messages', 'id'))")
                    .fetch_one(&mut *c)
                    .await?;
                let thread_id = m.thread_id.unwrap_or(id);
                sqlx::query(
                    "INSERT INTO messages (id, org_id, sender_seat_id, thread_id, parent_id, depth, subject, body, \
                     lines, to_addrs, cc_addrs, priority, ack_required, created_at, meta, ack_due, notice) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
                )
                .bind(id)
                .bind(m.org_id)
                .bind(m.sender_seat_id)
                .bind(thread_id)
                .bind(m.parent_id)
                .bind(m.depth)
                .bind(&m.subject)
                .bind(&m.body)
                .bind(line_count(&m.body))
                .bind(&to)
                .bind(&cc)
                .bind(m.priority.as_str())
                .bind(m.ack_required)
                .bind(now)
                .bind(&meta)
                .bind(m.ack_due)
                .bind(m.notice)
                .execute(&mut *c)
                .await?;

                let mut seqs = Vec::with_capacity(m.deliveries.len());
                let (mut orgs, mut seats, mut classes, mut modes, mut seqv) = (vec![], vec![], vec![], vec![], vec![]);
                for d in &m.deliveries {
                    let seq = seq_of(d.seat_id).ok_or_else(|| StoreError::NotFound(format!("seat {}", d.seat_id)))?;
                    orgs.push(d.org_id);
                    seats.push(d.seat_id);
                    classes.push(d.class.as_str());
                    modes.push(d.mode.as_str());
                    seqv.push(seq);
                    seqs.push(SeqBump { org_id: d.org_id, seat_id: d.seat_id, seq });
                }
                sqlx::query(
                    "INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq) \
                     SELECT o, $1, s, c, md, q, q \
                     FROM UNNEST($2::bigint[], $3::bigint[], $4::text[], $5::text[], $6::bigint[]) AS t(o, s, c, md, q)",
                )
                .bind(id)
                .bind(&orgs)
                .bind(&seats)
                .bind(&classes)
                .bind(&modes)
                .bind(&seqv)
                .execute(&mut *c)
                .await?;
                // The recipients take part in the thread (see migration 0012).
                sqlx::query(
                    "INSERT INTO thread_seats (thread_id, org_id, seat_id) \
                     SELECT $1, o, s FROM UNNEST($2::bigint[], $3::bigint[]) AS t(o, s) \
                     ORDER BY 2, 3 ON CONFLICT DO NOTHING",
                )
                .bind(thread_id)
                .bind(&orgs)
                .bind(&seats)
                .execute(&mut *c)
                .await?;
                // A message that reaches another org: every org on it (the
                // sender's and the recipients') knows all its seats as
                // participants of the thread (see migration 0019).
                if orgs.iter().any(|o| *o != m.org_id) {
                    let mut viewers: Vec<i64> = orgs.clone();
                    viewers.push(m.org_id);
                    viewers.sort_unstable();
                    viewers.dedup();
                    let on_seats = [vec![m.sender_seat_id], seats.clone()].concat();
                    let on_orgs = [vec![m.org_id], orgs.clone()].concat();
                    let (mut vv, mut ss, mut oo) = (vec![], vec![], vec![]);
                    for v in &viewers {
                        for (s, o) in on_seats.iter().zip(&on_orgs) {
                            vv.push(*v);
                            ss.push(*s);
                            oo.push(*o);
                        }
                    }
                    sqlx::query(
                        "INSERT INTO thread_views (thread_id, viewer_org_id, seat_id, org_id) \
                         SELECT $1, v, s, o FROM UNNEST($2::bigint[], $3::bigint[], $4::bigint[]) AS t(v, s, o) \
                         ORDER BY 2, 3 ON CONFLICT DO NOTHING",
                    )
                    .bind(thread_id)
                    .bind(&vv)
                    .bind(&ss)
                    .bind(&oo)
                    .execute(&mut *c)
                    .await?;
                }
                // Last, so the org's row stays locked only until the commit.
                // The message and every delivery are charged (see
                // `message_bytes`); refused over the quota, and when the org
                // was deleted meanwhile.
                let added: Option<i64> = sqlx::query_scalar(
                    "UPDATE orgs SET stored_bytes = stored_bytes + $1 \
                     WHERE id = $2 AND deleted_at IS NULL \
                     AND ($3::bigint IS NULL OR stored_bytes + $1 <= $3) RETURNING id",
                )
                .bind(bytes)
                .bind(m.org_id)
                .bind(m.storage_quota)
                .fetch_optional(&mut *c)
                .await?;
                if added.is_none() {
                    let stored: i64 = sqlx::query_scalar("SELECT stored_bytes FROM orgs WHERE id = $1")
                        .bind(m.org_id)
                        .fetch_one(&mut *c)
                        .await?;
                    return Err(StoreError::StorageQuota(stored));
                }
                Ok(Sent { message_id: id, thread_id, created_at: now, seqs })
            })
        })
        .await
    }

    async fn claim_impl(
        &self,
        seat_id: i64,
        f: &ClaimFilter,
    ) -> Result<(Vec<Entry>, Option<SeqBump>)> {
        let f = f.clone();
        self.write(move |c| {
            Box::pin(async move {
                // A keyed claim first takes its key: a concurrent claim with
                // the same key waits here until this one commits, then
                // returns what this one took.
                if let Some(key) = &f.key {
                    let fresh: Option<i64> = sqlx::query_scalar(
                        "INSERT INTO claim_keys (seat_id, key, ids, created_at) VALUES ($1, $2, '', $3) \
                         ON CONFLICT DO NOTHING RETURNING seat_id",
                    )
                    .bind(seat_id)
                    .bind(key)
                    .bind(now_ms())
                    .fetch_optional(&mut *c)
                    .await?;
                    if fresh.is_none() {
                        let ids = q::claimed_with_key(c, seat_id, key).await?.unwrap_or_default();
                        return Ok((q::entries_by_ids(c, seat_id, &ids).await?, None));
                    }
                }
                let ids =
                    q::claim_candidates(c, concat!(claim_select!(), " FOR NO KEY UPDATE OF d SKIP LOCKED"), seat_id, &f, None).await?;
                if let Some(key) = &f.key {
                    let list: Vec<String> = ids.iter().map(i64::to_string).collect();
                    sqlx::query("UPDATE claim_keys SET ids = $3 WHERE seat_id = $1 AND key = $2")
                        .bind(seat_id)
                        .bind(key)
                        .bind(list.join(","))
                        .execute(&mut *c)
                        .await?;
                }
                if ids.is_empty() {
                    return Ok((Vec::new(), None));
                }
                let n = ids.len() as i64;
                let hi = q::bump_seq(c, seat_id, n).await?;
                let lo = hi - n;
                sqlx::query(
                    "UPDATE deliveries d SET notified_at = $1, notified_via = $2, mod_seq = $3 + x.ord, \
                     read_at = CASE WHEN $5 >= 0 AND \
                       (SELECT length(m.body) FROM messages m WHERE m.id = d.message_id) <= $5 \
                       THEN COALESCE(d.read_at, $1) ELSE d.read_at END \
                     FROM UNNEST($4::bigint[]) WITH ORDINALITY AS x(id, ord) WHERE d.id = x.id",
                )
                .bind(now_ms())
                .bind(&f.via)
                .bind(lo)
                .bind(&ids)
                .bind(f.read_body_max.unwrap_or(-1))
                .execute(&mut *c)
                .await?;
                let entries = q::entries_in_range(c, seat_id, lo, hi).await?;
                let bump = entries.first().map(|e| SeqBump { org_id: e.delivery.org_id, seat_id, seq: hi });
                Ok((entries, bump))
            })
        })
        .await
    }
}

impl PgStore {
    /// Token bucket in `send_budget`, on the database's clock, so that every
    /// server instance draws from the same bucket.
    async fn take_send_token_impl(
        &self,
        seat_id: i64,
        per_min: i64,
        burst: i64,
    ) -> Result<std::result::Result<(), u64>> {
        self.take_bucket(
            "send_budget",
            seat_id,
            per_min.max(0) as f64 / 60_000.0,
            burst,
        )
        .await
    }

    async fn refund_send_token_impl(&self, seat_id: i64, burst: i64) -> Result<()> {
        self.refund_bucket("send_budget", seat_id, burst).await
    }

    async fn take_thread_token_impl(
        &self,
        seat_id: i64,
        per_hour: i64,
    ) -> Result<std::result::Result<(), u64>> {
        self.take_bucket(
            "thread_budget",
            seat_id,
            per_hour.max(0) as f64 / 3_600_000.0,
            per_hour,
        )
        .await
    }

    async fn refund_thread_token_impl(&self, seat_id: i64, per_hour: i64) -> Result<()> {
        self.refund_bucket("thread_budget", seat_id, per_hour).await
    }

    /// Take a token from the seat's bucket in `table` (`send_budget` or
    /// `thread_budget`), refilled with `per_ms` tokens a millisecond and
    /// holding at most `burst`.
    async fn take_bucket(
        &self,
        table: &'static str,
        seat_id: i64,
        per_ms: f64,
        burst: i64,
    ) -> Result<std::result::Result<(), u64>> {
        let burst = burst.max(1) as f64;
        let mut c = self.pool.acquire().await?;
        let left: Option<f64> = sqlx::query_scalar(&format!(
            "INSERT INTO {table} AS b (seat_id, tokens, updated_at) \
             SELECT $1, $2::float8 - 1, (extract(epoch FROM clock_timestamp()) * 1000)::bigint \
             ON CONFLICT (seat_id) DO UPDATE SET \
               tokens = LEAST($2::float8, b.tokens + GREATEST(0, excluded.updated_at - b.updated_at) * $3::float8) - 1, \
               updated_at = excluded.updated_at \
             WHERE LEAST($2::float8, b.tokens + GREATEST(0, excluded.updated_at - b.updated_at) * $3::float8) >= 1 \
             RETURNING tokens"
        ))
        .bind(seat_id)
        .bind(burst)
        .bind(per_ms)
        .fetch_optional(&mut *c)
        .await?;
        if left.is_some() {
            return Ok(Ok(()));
        }
        if per_ms == 0.0 {
            return Ok(Err(3_600_000));
        }
        let have: f64 = sqlx::query_scalar(&format!(
            "SELECT LEAST($2::float8, tokens + GREATEST(0, (extract(epoch FROM clock_timestamp()) * 1000)::bigint \
             - updated_at) * $3::float8) FROM {table} WHERE seat_id = $1"
        ))
        .bind(seat_id)
        .bind(burst)
        .bind(per_ms)
        .fetch_optional(&mut *c)
        .await?
        .unwrap_or(0.0);
        Ok(Err((((1.0 - have) / per_ms).ceil() as u64).max(1)))
    }

    async fn refund_bucket(&self, table: &'static str, seat_id: i64, burst: i64) -> Result<()> {
        sqlx::query(&format!(
            "UPDATE {table} SET tokens = LEAST($2::float8, tokens + 1) WHERE seat_id = $1"
        ))
        .bind(seat_id)
        .bind(burst.max(1) as f64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

impl_store_common!(PgStore, sqlx::Postgres, Dialect::Postgres);
