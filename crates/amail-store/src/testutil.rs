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
    /// For every org: `(id, stored_bytes, the same count computed from
    /// scratch)`. The second count comes from running the storage migration
    /// (`0013_storage_deliveries.sql`) again in a transaction that is then
    /// rolled back.
    pub async fn storage_counts(&self) -> Vec<(i64, i64, i64)> {
        const SQLITE: &str = include_str!("../migrations/sqlite/0013_storage_deliveries.sql");
        const PG: &str = include_str!("../migrations/postgres/0013_storage_deliveries.sql");
        const READ: &str = "SELECT id, stored_bytes FROM orgs ORDER BY id";
        // Concurrent sends wait until the counts are read: SQLite takes the
        // write lock at once, Postgres locks `orgs` (a send updates its
        // org's row last, so its rows are either all visible or none).
        macro_rules! run {
            ($c:expr, $begin:expr, $recount:expr) => {{
                sqlx::raw_sql($begin).execute(&mut $c).await.expect("begin");
                let now: Vec<(i64, i64)> =
                    sqlx::query_as(READ).fetch_all(&mut $c).await.expect("read");
                sqlx::raw_sql($recount)
                    .execute(&mut $c)
                    .await
                    .expect("recount");
                let new: Vec<(i64, i64)> =
                    sqlx::query_as(READ).fetch_all(&mut $c).await.expect("read");
                sqlx::raw_sql("ROLLBACK")
                    .execute(&mut $c)
                    .await
                    .expect("rollback");
                now.iter().zip(&new).map(|(a, b)| (a.0, a.1, b.1)).collect()
            }};
        }
        if self.url.starts_with("sqlite:") {
            let mut c = sqlx::SqliteConnection::connect(&self.url)
                .await
                .expect("connect");
            run!(c, "BEGIN IMMEDIATE", SQLITE)
        } else {
            let mut c = sqlx::PgConnection::connect(&self.url)
                .await
                .expect("connect");
            run!(c, "BEGIN; LOCK TABLE orgs IN SHARE ROW EXCLUSIVE MODE", PG)
        }
    }
}

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
