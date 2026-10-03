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
