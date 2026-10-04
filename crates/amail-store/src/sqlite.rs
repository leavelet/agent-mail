//! SQLite backend.
//!
//! WAL mode with `synchronous=NORMAL`. Reads use a connection pool. All
//! writes go through one writer task that owns the only write connection:
//! it drains whatever jobs are queued, runs them in a single transaction
//! (each job inside its own savepoint, so one failing job does not affect the
//! others) and commits once. Concurrent sends are thereby merged into one
//! commit, and per-seat sequence numbers are trivially serialized.

use crate::common::{join_addrs, line_count, meta_to_db};
use crate::model::*;
use crate::{OpenOptions, Result, StoreError, now_ms};
use futures::future::BoxFuture;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteSynchronous,
};
use sqlx::{Connection, Executor};
use std::any::Any;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

type AnyResult = Result<Box<dyn Any + Send>>;
type RunFn = Box<dyn for<'c> FnOnce(&'c mut SqliteConnection) -> BoxFuture<'c, AnyResult> + Send>;

struct Job {
    run: RunFn,
    reply: oneshot::Sender<AnyResult>,
}

/// Upper bound on the number of jobs merged into one transaction.
const MAX_BATCH: usize = 256;

pub struct SqliteStore {
    read: SqlitePool,
    jobs: mpsc::Sender<Job>,
    /// Send budgets. A SQLite database has one server process (`amail serve`
    /// refuses to share it), so the budget can live in memory.
    budget: crate::budget::Budget,
}

fn boxed_run<F>(f: F) -> RunFn
where
    F: for<'c> FnOnce(&'c mut SqliteConnection) -> BoxFuture<'c, AnyResult> + Send + 'static,
{
    Box::new(f)
}

impl SqliteStore {
    pub(crate) const BACKEND: &'static str = "sqlite";

    /// Open (creating if needed) the database at `url` (`sqlite://path`) and
    /// run migrations. In-memory databases are not supported because readers
    /// and the writer use separate connections.
    pub async fn open(url: &str, opts: &OpenOptions) -> Result<Self> {
        if url.contains(":memory:") || url.contains("mode=memory") {
            return Err(StoreError::Invalid(
                "in-memory SQLite is not supported; use a file".into(),
            ));
        }
        let base = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(10))
            .foreign_keys(true);
        let mut writer = SqliteConnection::connect_with(&base).await?;
        sqlx::migrate!("./migrations/sqlite")
            .run(&mut writer)
            .await?;
        writer
            .execute("CREATE TEMP TABLE claim_ids (ord INTEGER PRIMARY KEY, id INTEGER NOT NULL)")
            .await?;
        // `SEND_SQL` stops at its first statement, with a CHECK violation,
        // when the message does not fit in the org's storage quota.
        writer
            .execute(
                "CREATE TEMP TABLE send_quota (fits BOOLEAN CHECK (fits)); \
                 INSERT INTO temp.send_quota VALUES (TRUE)",
            )
            .await?;
        let read = SqlitePoolOptions::new()
            .max_connections(opts.max_connections.max(1))
            .test_before_acquire(false)
            .connect_with(base)
            .await?;
        // Checkpointing: a background connection copies WAL frames into the
        // database (and fsyncs it) off the write path. Once a second the
        // writer checkpoints whatever is left in RESTART mode so the WAL is
        // reused from the start instead of growing without bound.
        writer.execute("PRAGMA wal_autocheckpoint = 0").await?;
        writer
            .execute("PRAGMA journal_size_limit = 67108864")
            .await?;
        let (jobs, rx) = mpsc::channel(4096);
        tokio::spawn(writer_loop(writer, rx));
        tokio::spawn(checkpoint_loop(read.clone(), jobs.downgrade()));
        Ok(SqliteStore {
            read,
            jobs,
            budget: Default::default(),
        })
    }

    async fn read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut SqliteConnection) -> BoxFuture<'c, Result<T>> + Send + 'static,
    {
        let mut c = self.read.acquire().await?;
        f(&mut c).await
    }

    /// A read whose cost depends on the data (search, archive, threads):
    /// interrupted after [`amail_proto::limits::QUERY_TIMEOUT_MS`].
    async fn read_query<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut SqliteConnection) -> BoxFuture<'c, Result<T>> + Send + 'static,
    {
        let mut c = self.read.acquire().await?;
        let deadline = std::time::Instant::now()
            + Duration::from_millis(amail_proto::limits::QUERY_TIMEOUT_MS);
        c.lock_handle()
            .await?
            .set_progress_handler(10_000, move || std::time::Instant::now() < deadline);
        let out = f(&mut c).await;
        c.lock_handle().await?.remove_progress_handler();
        out
    }

    async fn write<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut SqliteConnection) -> BoxFuture<'c, Result<T>> + Send + 'static,
    {
        let (reply, rx) = oneshot::channel();
        let run = boxed_run(move |c| {
            Box::pin(async move { f(c).await.map(|v| Box::new(v) as Box<dyn Any + Send>) })
        });
        self.jobs
            .send(Job { run, reply })
            .await
            .map_err(|_| StoreError::Closed)?;
        let out = rx.await.map_err(|_| StoreError::Closed)??;
        Ok(*out
            .downcast::<T>()
            .expect("writer job returned an unexpected type"))
    }

    async fn send_impl(&self, m: &NewMessage) -> Result<Sent> {
        // `[{"s":seat,"o":org,"c":class,"m":mode},...]`; all values are ids or
        // fixed ASCII words, so no escaping is needed.
        let deliveries = format!(
            "[{}]",
            m.deliveries
                .iter()
                .map(|d| format!(
                    r#"{{"s":{},"o":{},"c":"{}","m":"{}"}}"#,
                    d.seat_id, d.org_id, d.class, d.mode
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        let m = m.clone();
        let now = now_ms();
        let meta = meta_to_db(&m.meta);
        let (to, cc) = (join_addrs(&m.to_addrs), join_addrs(&m.cc_addrs));
        let bytes = message_bytes(&m.subject, &m.body, &meta, &to, &cc);
        let rows: Vec<(i64, i64, i64, i64, i64)> = self
            .write(move |c| {
                Box::pin(async move {
                    let rows = sqlx::query_as(SEND_SQL)
                        .bind(m.org_id)
                        .bind(m.sender_seat_id)
                        .bind(m.thread_id)
                        .bind(m.parent_id)
                        .bind(m.depth)
                        .bind(&m.subject)
                        .bind(&m.body)
                        .bind(line_count(&m.body))
                        .bind(to)
                        .bind(cc)
                        .bind(m.priority.as_str())
                        .bind(m.ack_required)
                        .bind(now)
                        .bind(deliveries)
                        .bind(meta)
                        .bind(m.ack_due)
                        .bind(bytes)
                        .bind(m.storage_quota)
                        .bind(amail_proto::limits::STORED_BYTES_PER_DELIVERY)
                        .fetch_all(&mut *c)
                        .await;
                    match rows {
                        Err(sqlx::Error::Database(e))
                            if e.kind() == sqlx::error::ErrorKind::CheckViolation =>
                        {
                            let stored: i64 =
                                sqlx::query_scalar("SELECT stored_bytes FROM orgs WHERE id = $1")
                                    .bind(m.org_id)
                                    .fetch_one(&mut *c)
                                    .await?;
                            Err(StoreError::StorageQuota(stored))
                        }
                        r => Ok(r?),
                    }
                })
            })
            .await?;
        let Some(&(message_id, thread_id, ..)) = rows.first() else {
            return Err(StoreError::Invalid("message has no deliveries".into()));
        };
        let seqs = rows
            .iter()
            .map(|&(_, _, seat_id, org_id, seq)| SeqBump {
                org_id,
                seat_id,
                seq,
            })
            .collect();
        Ok(Sent {
            message_id,
            thread_id,
            created_at: now,
            seqs,
        })
    }

    async fn claim_impl(
        &self,
        seat_id: i64,
        f: &ClaimFilter,
    ) -> Result<(Vec<Entry>, Option<SeqBump>)> {
        use amail_proto::DeliveryClass as C;
        let f = f.clone();
        let rows: Vec<crate::common::EntryRow> = self
            .write(move |c| {
                Box::pin(async move {
                    Ok(sqlx::query_as(CLAIM_SQL)
                        .bind(seat_id)
                        .bind(crate::common::mode_mask(&f))
                        .bind(f.has(C::To))
                        .bind(f.has(C::Cc))
                        .bind(f.has(C::List))
                        .bind(f.urgent_only)
                        .bind(f.limit)
                        .bind(f.key.clone())
                        .bind(now_ms())
                        .bind(&f.via)
                        .bind(f.read_body_max.unwrap_or(-1))
                        .fetch_all(&mut *c)
                        .await?)
                })
            })
            .await?;
        let entries = crate::common::collect(rows, crate::common::EntryRow::into_entry)?;
        let bump = entries
            .iter()
            .map(|e| e.delivery.mod_seq)
            .max()
            .map(|seq| SeqBump {
                org_id: entries[0].delivery.org_id,
                seat_id,
                seq,
            });
        Ok((entries, bump))
    }
}

// The two hot write paths run as one multi-statement query each, so the
// writer makes a single round trip to SQLite's connection thread per job.
// They rely on this connection being the only writer: the message just
// inserted is `MAX(messages.id)`, and seats' `last_seq` values read back are
// the ones just assigned.

/// Binds: $1 org, $2 sender, $3 thread (NULL for a new thread), $4 parent,
/// $5 depth, $6 subject, $7 body, $8 lines, $9 to, $10 cc, $11 priority,
/// $12 ack_required, $13 now, $14 deliveries JSON, $15 meta, $16 ack_due,
/// $17 the message's bytes without its deliveries (see `message_bytes`),
/// $18 the org's storage quota (NULL: none), $19 the bytes per delivery.
/// The org's `stored_bytes` grows by $17 plus $19 per delivery created; the
/// message is refused when that would exceed $18, and also when the org was
/// deleted meanwhile.
/// Returns `(message_id, thread_id, seat_id, org_id, seq)` per delivery.
const SEND_SQL: &str = "
UPDATE temp.send_quota SET fits = COALESCE((SELECT deleted_at IS NULL
       AND ($18 IS NULL OR stored_bytes + $17 + $19 * json_array_length($14) <= $18) FROM orgs WHERE id = $1), FALSE);
INSERT INTO messages (org_id, sender_seat_id, thread_id, parent_id, depth, subject, body, lines,
                      to_addrs, cc_addrs, priority, ack_required, created_at, meta, ack_due)
VALUES ($1, $2, COALESCE($3, 0), $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $15, $16);
UPDATE messages SET thread_id = id WHERE id = last_insert_rowid() AND $3 IS NULL;
UPDATE seats SET last_seq = last_seq + 1
 WHERE id IN (SELECT json_extract(value, '$.s') FROM json_each($14));
INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq)
SELECT json_extract(j.value, '$.o'), (SELECT MAX(id) FROM messages), s.id,
       json_extract(j.value, '$.c'), json_extract(j.value, '$.m'), s.last_seq, s.last_seq
  FROM json_each($14) j JOIN seats s ON s.id = json_extract(j.value, '$.s');
UPDATE orgs SET stored_bytes = stored_bytes + $17
       + $19 * (SELECT COUNT(*) FROM deliveries WHERE message_id = (SELECT MAX(id) FROM messages))
 WHERE id = $1;
SELECT m.id, m.thread_id, d.seat_id, d.org_id, d.seq
  FROM deliveries d JOIN messages m ON m.id = d.message_id
 WHERE d.message_id = (SELECT MAX(id) FROM messages)
 ORDER BY d.seat_id";

/// Binds: $1..$8 as `claim_select!` ($8: the idempotency key or NULL), $9
/// now, $10 via, $11 read_body_max (-1: none). The candidates are numbered
/// 1..n in `temp.claim_ids` (seq order) and get mod_seq lo+1..lo+n. With a
/// key the seat used before, nothing is claimed and that claim's deliveries
/// are returned (the writer is the only one, so the check cannot race).
const CLAIM_SQL: &str = concat!(
    "DELETE FROM temp.claim_ids;
INSERT INTO temp.claim_ids (id) ",
    claim_select!(),
    ";
UPDATE seats SET last_seq = last_seq + (SELECT COUNT(*) FROM temp.claim_ids) WHERE id = $1;
UPDATE deliveries SET notified_at = $9, notified_via = $10,
       read_at = CASE WHEN $11 >= 0
                       AND (SELECT length(m.body) FROM messages m WHERE m.id = deliveries.message_id) <= $11
                      THEN COALESCE(read_at, $9) ELSE read_at END,
       mod_seq = (SELECT last_seq FROM seats WHERE id = $1) - (SELECT COUNT(*) FROM temp.claim_ids) + c.ord
  FROM temp.claim_ids c WHERE deliveries.id = c.id;
INSERT INTO claim_keys (seat_id, key, ids, created_at)
SELECT $1, $8, COALESCE((SELECT group_concat(id) FROM temp.claim_ids), ''), $9
 WHERE $8 IS NOT NULL AND NOT EXISTS (SELECT 1 FROM claim_keys WHERE seat_id = $1 AND key = $8);
",
    entry_select_body!(),
    " WHERE d.seat_id = $1 AND (d.id IN (SELECT id FROM temp.claim_ids) \
      OR d.id IN (SELECT j.value FROM claim_keys k, json_each('[' || k.ids || ']') j \
                  WHERE k.seat_id = $1 AND k.key = $8)) ORDER BY d.seq"
);

const WRITER_CHECKPOINT_EVERY: Duration = Duration::from_secs(1);

/// Run a passive checkpoint four times a second while the store is alive.
async fn checkpoint_loop(pool: SqlitePool, alive: mpsc::WeakSender<Job>) {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tick.tick().await;
        if alive.upgrade().is_none() {
            return;
        }
        if let Err(e) = sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(&pool)
            .await
        {
            tracing::warn!("sqlite checkpoint failed: {e}");
        }
    }
}

async fn writer_loop(mut conn: SqliteConnection, mut rx: mpsc::Receiver<Job>) {
    let mut batch = Vec::with_capacity(MAX_BATCH);
    let mut last_checkpoint = std::time::Instant::now();
    while rx.recv_many(&mut batch, MAX_BATCH).await > 0 {
        run_batch(&mut conn, &mut batch).await;
        if last_checkpoint.elapsed() >= WRITER_CHECKPOINT_EVERY {
            // RESTART waits for in-flight readers, then lets the next write
            // start the WAL from the beginning.
            if let Err(e) = conn.execute("PRAGMA wal_checkpoint(RESTART)").await {
                tracing::warn!("sqlite checkpoint failed: {e}");
            }
            last_checkpoint = std::time::Instant::now();
        }
    }
    let _ = conn.close().await;
}

async fn run_batch(conn: &mut SqliteConnection, batch: &mut Vec<Job>) {
    if let Err(e) = conn.execute("BEGIN IMMEDIATE").await {
        for job in batch.drain(..) {
            let _ = job
                .reply
                .send(Err(StoreError::Commit(format!("begin: {e}"))));
        }
        return;
    }
    // A lone job needs no savepoint: on error the whole transaction rolls back.
    let single = batch.len() == 1;
    let mut results = Vec::with_capacity(batch.len());
    for job in batch.drain(..) {
        if !single && let Err(e) = conn.execute("SAVEPOINT job").await {
            results.push((job.reply, Err(StoreError::from(e))));
            continue;
        }
        let r = (job.run)(&mut *conn).await;
        if !single {
            if r.is_err() {
                let _ = conn.execute("ROLLBACK TO job").await;
            }
            let _ = conn.execute("RELEASE job").await;
        }
        results.push((job.reply, r));
    }
    let all_failed = results.iter().all(|(_, r)| r.is_err());
    let end = if all_failed {
        conn.execute("ROLLBACK").await
    } else {
        conn.execute("COMMIT").await
    };
    match end {
        Ok(_) => {
            for (reply, r) in results {
                let _ = reply.send(r);
            }
        }
        Err(e) => {
            tracing::error!("sqlite commit failed: {e}");
            let _ = conn.execute("ROLLBACK").await;
            let msg = e.to_string();
            for (reply, r) in results {
                let _ = reply.send(r.and(Err(StoreError::Commit(msg.clone()))));
            }
        }
    }
}

impl SqliteStore {
    async fn take_send_token_impl(
        &self,
        seat_id: i64,
        per_min: i64,
        burst: i64,
    ) -> Result<std::result::Result<(), u64>> {
        Ok(self.budget.take(seat_id, per_min, burst))
    }

    async fn refund_send_token_impl(&self, seat_id: i64, burst: i64) -> Result<()> {
        self.budget.refund(seat_id, burst);
        Ok(())
    }
}

impl_store_common!(SqliteStore, sqlx::Sqlite, Dialect::Sqlite);
