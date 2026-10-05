//! Test helpers shared by the test suites of all crates.
//!
//! Postgres tests need `AMAIL_TEST_PG_URL` (a URL of a server where the user
//! may create databases, e.g. `postgres://postgres:pw@127.0.0.1:5432/postgres`).
//! Each test gets a fresh database. When the variable is unset, Postgres
//! tests are skipped with a message on stderr.

use crate::{OpenOptions, Store};
use sqlx::Connection;
use std::sync::Arc;

pub const PG_ENV: &str = "AMAIL_TEST_PG_URL";

/// A store for one test plus whatever keeps its database alive.
pub struct TestDb {
    pub store: Arc<dyn Store>,
    pub url: String,
    _dir: Option<tempfile::TempDir>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Postgres,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Sqlite => "sqlite",
            Backend::Postgres => "postgres",
        }
    }
}

/// A fresh database URL for `backend`, or `None` (after printing a skip
/// notice) when Postgres is not configured. The returned guard must outlive
/// the database's use.
pub async fn fresh_url(
    backend: Backend,
    test: &str,
) -> Option<(String, Option<tempfile::TempDir>)> {
    match backend {
        Backend::Sqlite => {
            let dir = tempfile::tempdir().expect("tempdir");
            let url = format!("sqlite://{}", dir.path().join("amail.db").display());
            Some((url, Some(dir)))
        }
        Backend::Postgres => {
            let Ok(admin) = std::env::var(PG_ENV) else {
                eprintln!("SKIPPED {test} on postgres: {PG_ENV} is not set");
                return None;
            };
            let name = format!("amail_test_{:016x}", rand::random::<u64>());
            let mut conn = sqlx::PgConnection::connect(&admin)
                .await
                .expect("connect to AMAIL_TEST_PG_URL");
            sqlx::query(&format!("CREATE DATABASE {name}"))
                .execute(&mut conn)
                .await
                .expect("create test database");
            let _ = conn.close().await;
            Some((with_database(&admin, &name), None))
        }
    }
}

/// Open a fresh store for `backend`; `None` if Postgres is not configured.
pub async fn open(backend: Backend, test: &str) -> Option<TestDb> {
    let (url, dir) = fresh_url(backend, test).await?;
    let store = crate::open(
        &url,
        &OpenOptions {
            max_connections: 16,
        },
    )
    .await
    .expect("open store");
    Some(TestDb {
        store,
        url,
        _dir: dir,
    })
}

impl TestDb {
    /// Run SQL statements directly on the database (bulk test data), then
    /// update the planner's statistics.
    pub async fn exec(&self, statements: &[String]) {
        if self.url.starts_with("sqlite:") {
            let mut c = sqlx::SqliteConnection::connect(&self.url)
                .await
                .expect("connect");
            for s in statements {
                sqlx::raw_sql(s).execute(&mut c).await.expect(s);
            }
            sqlx::raw_sql("ANALYZE")
                .execute(&mut c)
                .await
                .expect("analyze");
        } else {
            let mut c = sqlx::PgConnection::connect(&self.url)
                .await
                .expect("connect");
            for s in statements {
                sqlx::raw_sql(s).execute(&mut c).await.expect(s);
            }
            sqlx::raw_sql("ANALYZE")
                .execute(&mut c)
                .await
                .expect("analyze");
        }
    }

    /// Forget the planner statistics that [`TestDb::exec`] gathered
    /// (SQLite): the server never runs `ANALYZE`, and statistics of an
    /// unusual fill (one thread of 60,000 messages) can make SQLite's
    /// planner scan an index where it otherwise looks a row up. The schema
    /// change at the end makes every open connection read them again.
    pub async fn drop_stats(&self) {
        if !self.url.starts_with("sqlite:") {
            return;
        }
        let mut c = sqlx::SqliteConnection::connect(&self.url)
            .await
            .expect("connect");
        for s in [
            "DELETE FROM sqlite_stat1",
            "CREATE TABLE amail_test_schema_bump (x INTEGER)",
            "DROP TABLE amail_test_schema_bump",
        ] {
            sqlx::raw_sql(s).execute(&mut c).await.expect(s);
        }
    }

    /// Recount the pending and unread counters of the org's seats from
    /// their deliveries, and record their threads, after deliveries were
    /// inserted directly (sending keeps both; on Postgres a direct insert
    /// does not).
    pub async fn recount_seats(&self, org_id: i64) {
        self.exec(&[
            format!("UPDATE seats SET \
 pending_to = (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'to'), \
 pending_cc = (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'cc'), \
 pending_list = (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'immediate' AND d.class = 'list'), \
 pending_urgent = (SELECT COUNT(*) FROM deliveries d JOIN messages m ON m.id = d.message_id WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'immediate' AND m.priority = 'urgent'), \
 pending_digest = (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'digest'), \
 unread = (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = seats.id AND d.read_at IS NULL AND d.mode <> 'archive'), \
 pending_digest_since = (SELECT MIN(m.created_at) FROM deliveries d JOIN messages m ON m.id = d.message_id WHERE d.seat_id = seats.id AND d.notified_at IS NULL AND d.mode = 'digest') WHERE org_id = {org_id}"),
            format!(
                "INSERT INTO thread_seats (thread_id, org_id, seat_id) \
                 SELECT DISTINCT m.thread_id, d.org_id, d.seat_id FROM deliveries d \
                 JOIN messages m ON m.id = d.message_id WHERE d.org_id = {org_id} \
                 ON CONFLICT DO NOTHING"
            ),
        ])
            .await;
    }

    /// Rows of a query whose columns are all integers (`BIGINT`), in order.
    pub async fn rows_i64(&self, sql: &str) -> Vec<Vec<i64>> {
        use sqlx::Row;
        if self.url.starts_with("sqlite:") {
            let mut c = sqlx::SqliteConnection::connect(&self.url)
                .await
                .expect("connect");
            let rows = sqlx::query(sql).fetch_all(&mut c).await.expect(sql);
            rows.iter()
                .map(|r| (0..r.len()).map(|i| r.get::<i64, _>(i)).collect())
                .collect()
        } else {
            let mut c = sqlx::PgConnection::connect(&self.url)
                .await
                .expect("connect");
            let rows = sqlx::query(sql).fetch_all(&mut c).await.expect(sql);
            rows.iter()
                .map(|r| (0..r.len()).map(|i| r.get::<i64, _>(i)).collect())
                .collect()
        }
    }

    /// Store `n` messages from seat `seat_id` of org `org_id` directly in
    /// the database, each its own thread with subject `filler <i>` and the
    /// given body, without deliveries (for tests that need much mail).
    pub async fn fill_messages(&self, org_id: i64, seat_id: i64, n: i64, body: &str) {
        let sql = "WITH RECURSIVE g(i) AS (SELECT CAST(1 AS BIGINT) UNION ALL SELECT i + 1 FROM g WHERE i < $3) \
                   INSERT INTO messages (org_id, sender_seat_id, thread_id, parent_id, depth, subject, body, \
                   lines, to_addrs, cc_addrs, priority, ack_required, created_at, meta) \
                   SELECT $1, $2, 0, NULL, 0, 'filler ' || CAST(i AS TEXT), $4, 1, '', '', 'normal', FALSE, $5, '' FROM g";
        let fix = "UPDATE messages SET thread_id = id WHERE thread_id = 0";
        let now = crate::now_ms();
        if self.url.starts_with("sqlite:") {
            let mut c = sqlx::SqliteConnection::connect(&self.url)
                .await
                .expect("connect");
            sqlx::query(sql)
                .bind(org_id)
                .bind(seat_id)
                .bind(n)
                .bind(body)
                .bind(now)
                .execute(&mut c)
                .await
                .expect("fill");
            sqlx::query(fix).execute(&mut c).await.expect("fill");
        } else {
            let mut c = sqlx::PgConnection::connect(&self.url)
                .await
                .expect("connect");
            sqlx::query(sql)
                .bind(org_id)
                .bind(seat_id)
                .bind(n)
                .bind(body)
                .bind(now)
                .execute(&mut c)
                .await
                .expect("fill");
            sqlx::query(fix).execute(&mut c).await.expect("fill");
            sqlx::query("ANALYZE messages")
                .execute(&mut c)
                .await
                .expect("analyze");
        }
    }
}

impl TestDb {
    /// For every org: its `stored_bytes` and `kept_bytes`, and the same
    /// two counts computed from scratch by [`RECOUNT`], in a transaction
    /// that is then rolled back.
    pub async fn storage_counts(&self) -> Vec<StorageCount> {
        self.recounted(RECOUNT).await
    }

    /// Like [`Self::storage_counts`], but the counts computed again are
    /// those of migration `0016_kept_bytes.sql` applied to the counts of
    /// the rule before it (the kept mail added back to `stored_bytes`).
    pub async fn storage_counts_after_0016(&self) -> Vec<StorageCount> {
        const SQLITE: &str = include_str!("../migrations/sqlite/0016_kept_bytes.sql");
        const PG: &str = include_str!("../migrations/postgres/0016_kept_bytes.sql");
        let m = if self.url.starts_with("sqlite:") {
            SQLITE
        } else {
            PG
        };
        // Without its first statement, which adds the column.
        let (_, rest) = m.split_once("DEFAULT 0;").expect("0016 adds kept_bytes");
        let sql = format!(
            "UPDATE orgs SET stored_bytes = stored_bytes + kept_bytes, kept_bytes = 0;{rest}"
        );
        self.recounted(&sql).await
    }

    async fn recounted(&self, recount: &str) -> Vec<StorageCount> {
        const READ: &str = "SELECT id, stored_bytes, kept_bytes FROM orgs ORDER BY id";
        // Concurrent sends wait until the counts are read: SQLite takes the
        // write lock at once, Postgres locks `orgs` (a send updates its
        // org's row last, so its rows are either all visible or none).
        macro_rules! run {
            ($c:expr, $begin:expr) => {{
                sqlx::raw_sql($begin).execute(&mut $c).await.expect("begin");
                let now: Vec<(i64, i64, i64)> =
                    sqlx::query_as(READ).fetch_all(&mut $c).await.expect("read");
                sqlx::raw_sql(recount)
                    .execute(&mut $c)
                    .await
                    .expect("recount");
                let new: Vec<(i64, i64, i64)> =
                    sqlx::query_as(READ).fetch_all(&mut $c).await.expect("read");
                sqlx::raw_sql("ROLLBACK")
                    .execute(&mut $c)
                    .await
                    .expect("rollback");
                now.iter()
                    .zip(&new)
                    .map(|(a, b)| StorageCount {
                        org_id: a.0,
                        stored_bytes: a.1,
                        kept_bytes: a.2,
                        stored_recount: b.1,
                        kept_recount: b.2,
                    })
                    .collect()
            }};
        }
        if self.url.starts_with("sqlite:") {
            let mut c = sqlx::SqliteConnection::connect(&self.url)
                .await
                .expect("connect");
            run!(c, "BEGIN IMMEDIATE")
        } else {
            let mut c = sqlx::PgConnection::connect(&self.url)
                .await
                .expect("connect");
            run!(c, "BEGIN; LOCK TABLE orgs IN SHARE ROW EXCLUSIVE MODE")
        }
    }
}

/// One org's storage counts and the same counted from scratch (see
/// [`TestDb::storage_counts`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageCount {
    pub org_id: i64,
    pub stored_bytes: i64,
    pub kept_bytes: i64,
    pub stored_recount: i64,
    pub kept_recount: i64,
}

/// The storage counts of every org from scratch, under the rule of
/// `amail_store::message_bytes`: `stored_bytes` is the mail the org sent
/// with all its deliveries; `kept_bytes` is, for each message of a deleted
/// org that the org keeps deliveries of, the message's full bytes and the
/// org's own deliveries of it. Notice mail (`messages.notice`) counts for
/// nobody, and deleted orgs count nothing. Portable
/// between SQLite and Postgres.
pub const RECOUNT: &str = "
UPDATE orgs SET stored_bytes = 0, kept_bytes = 0;
UPDATE orgs SET stored_bytes = COALESCE((
    SELECT SUM(512 + octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta)
               + octet_length(m.to_addrs) + octet_length(m.cc_addrs)
               + 384 * (SELECT COUNT(*) FROM deliveries d WHERE d.message_id = m.id))
      FROM messages m WHERE m.org_id = orgs.id AND NOT m.notice
), 0), kept_bytes = COALESCE((
    SELECT SUM(512 + octet_length(m.subject) + octet_length(m.body) + octet_length(m.meta)
               + octet_length(m.to_addrs) + octet_length(m.cc_addrs)
               + 384 * (SELECT COUNT(*) FROM deliveries d WHERE d.message_id = m.id AND d.org_id = orgs.id))
      FROM messages m
     WHERE m.org_id IN (SELECT o.id FROM orgs o WHERE o.deleted_at IS NOT NULL) AND NOT m.notice
       AND EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.org_id = orgs.id)
), 0) WHERE deleted_at IS NULL;
";

/// Replace the database name in a Postgres URL.
pub fn with_database(url: &str, db: &str) -> String {
    let (base, query) = match url.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (url, None),
    };
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let host_part = match base[scheme_end..].find('/') {
        Some(i) => &base[..scheme_end + i],
        None => base,
    };
    match query {
        Some(q) => format!("{host_part}/{db}?{q}"),
        None => format!("{host_part}/{db}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn with_database() {
        assert_eq!(
            super::with_database("postgres://u:p@h:1/postgres", "x"),
            "postgres://u:p@h:1/x"
        );
        assert_eq!(
            super::with_database("postgres://u:p@h:1", "x"),
            "postgres://u:p@h:1/x"
        );
        assert_eq!(
            super::with_database("postgres://h/db?sslmode=disable", "x"),
            "postgres://h/x?sslmode=disable"
        );
    }
}
