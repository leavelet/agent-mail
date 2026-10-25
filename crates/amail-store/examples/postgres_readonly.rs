//! Check the application's PostgreSQL TLS connection without running migrations.
//! AMAIL_DB is read only from the environment; no DSN or credentials are logged.
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::{Connection, PgConnection};
use std::str::FromStr;
use std::time::Duration;

async fn check() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("AMAIL_DB")?;
    let options = PgConnectOptions::from_str(&url)?;
    if !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull) {
        return Err("verify-full is required".into());
    }
    let options = options.options([
        ("default_transaction_read_only", "on"),
        ("statement_timeout", "5000"),
    ]);
    let mut conn = tokio::time::timeout(
        Duration::from_secs(15),
        PgConnection::connect_with(&options),
    )
    .await??;
    let (ssl, tls, server, read_only, extension, can_create, schema_create): (
        bool,
        String,
        String,
        String,
        bool,
        bool,
        bool,
    ) = sqlx::query_as(
        "SELECT ssl, version, current_setting('server_version'),
                current_setting('transaction_read_only'),
                EXISTS(SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm'),
                has_database_privilege(current_user, current_database(), 'CREATE'),
                has_schema_privilege(current_user, 'public', 'CREATE')
         FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
    )
    .fetch_one(&mut conn)
    .await?;
    if !ssl || read_only != "on" {
        return Err("TLS or read-only session check failed".into());
    }
    let amail_tables: (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name IN ('orgs', 'seats', 'messages', '_sqlx_migrations')",
    )
    .fetch_one(&mut conn)
    .await?;
    conn.close().await?;
    println!(
        "{}",
        serde_json::json!({
            "connection": "sqlx",
            "sslmode": "verify-full",
            "ssl": ssl,
            "tls_version": tls,
            "server_version": server,
            "transaction_read_only": true,
            "pg_trgm_installed": extension,
            "database_create": can_create,
            "public_schema_create": schema_create,
            "amail_tables_present": amail_tables.0,
            "migrations_executed": false,
        })
    );
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if check().await.is_err() {
        // A database or URL error may contain credentials. Never print it.
        eprintln!("PostgreSQL read-only check failed; check TLS, connection and read permissions.");
        std::process::exit(1);
    }
}
