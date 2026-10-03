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

use crate::common::{join_addrs, like_pattern, line_count, meta_to_db, search_sql};
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
                let bumped: Vec<(i64, i64)> = sqlx::query_as(
                    "UPDATE seats SET last_seq = last_seq + 1 WHERE id = ANY($1) RETURNING id, last_seq",
                )
                .bind(&seat_ids)
                .fetch_all(&mut *c)
                .await?;
                let seq_of = |seat: i64| bumped.iter().find(|(id, _)| *id == seat).map(|(_, s)| *s);

                let id: i64 = sqlx::query_scalar("SELECT nextval(pg_get_serial_sequence('messages', 'id'))")
                    .fetch_one(&mut *c)
                    .await?;
                let thread_id = m.thread_id.unwrap_or(id);
                sqlx::query(
                    "INSERT INTO messages (id, org_id, sender_seat_id, thread_id, parent_id, depth, subject, body, \
                     lines, to_addrs, cc_addrs, priority, ack_required, created_at, meta) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
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
                .bind(join_addrs(&m.to_addrs))
                .bind(join_addrs(&m.cc_addrs))
                .bind(m.priority.as_str())
                .bind(m.ack_required)
                .bind(now)
                .bind(meta_to_db(&m.meta))
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
                Ok(Sent { message_id: id, thread_id, created_at: now, seqs })
            })
        })
        .await
    }

    /// `ILIKE` on `subject || ' ' || body`, the expression of the trigram
    /// index; in scan mode the same condition limited to the window.
    async fn search_impl(
        &self,
        org_id: i64,
        q: &SearchQuery,
        lower: Option<i64>,
    ) -> Result<Vec<MessageRec>> {
        let conds: Vec<String> = (0..q.terms.len())
            .map(|i| format!("(m.subject || ' ' || m.body) ILIKE ${} ESCAPE '\\'", i + 5))
            .collect();
        let sql = search_sql(&conds);
        let binds: Vec<String> = q.terms.iter().map(|t| like_pattern(t)).collect();
        let (before, limit) = (q.before_id, q.limit);
        let rows: Vec<crate::common::MsgRow> = self
            .read(move |c| {
                Box::pin(async move {
                    let mut query = sqlx::query_as(&sql)
                        .bind(org_id)
                        .bind(before)
                        .bind(lower)
                        .bind(limit);
                    for b in &binds {
                        query = query.bind(b);
                    }
                    Ok(query.fetch_all(&mut *c).await?)
                })
            })
            .await?;
        crate::common::collect(rows, crate::common::MsgRow::into_message)
    }

    async fn claim_impl(
        &self,
        seat_id: i64,
        f: &ClaimFilter,
    ) -> Result<(Vec<Entry>, Option<SeqBump>)> {
        let f = f.clone();
        self.write(move |c| {
            Box::pin(async move {
                let ids =
                    q::claim_candidates(c, concat!(claim_select!(), " FOR NO KEY UPDATE OF d SKIP LOCKED"), seat_id, &f).await?;
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

impl_store_common!(PgStore, sqlx::Postgres);
