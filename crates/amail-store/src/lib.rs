//! Storage for agent-mail.
//!
//! [`Store`] is the single interface the server uses. It has two
//! implementations with identical schemas and semantics: [`SqliteStore`]
//! (default, single file, one writer task with group commit) and [`PgStore`].
//!
//! The store enforces data invariants (unique names, per-seat sequence
//! numbers, atomic sends, exclusive claims). Policy (who may send to whom,
//! rate limits, address resolution) lives in the server.

#[macro_use]
mod common;
mod model;
mod postgres;
mod sqlite;
pub mod testutil;

pub use model::*;
pub use postgres::PgStore;
pub use sqlite::SqliteStore;

use async_trait::async_trait;
use std::sync::Arc;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("database error: {0}")]
    Db(sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("commit failed: {0}")]
    Commit(String),
    #[error("store is shut down")]
    Closed,
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        match e.as_database_error() {
            Some(d) if d.is_unique_violation() => StoreError::Conflict(d.message().to_string()),
            _ => StoreError::Db(e),
        }
    }
}

/// Connection settings for [`open`].
#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Postgres pool size, or SQLite read pool size.
    pub max_connections: u32,
}

impl Default for OpenOptions {
    fn default() -> Self {
        OpenOptions {
            max_connections: 16,
        }
    }
}

/// Open a store from a URL (`sqlite://path` or `postgres://...`) and run
/// migrations.
pub async fn open(url: &str, opts: &OpenOptions) -> Result<Arc<dyn Store>> {
    if url.starts_with("sqlite:") {
        Ok(Arc::new(SqliteStore::open(url, opts).await?))
    } else if url.starts_with("postgres:") || url.starts_with("postgresql:") {
        Ok(Arc::new(PgStore::open(url, opts).await?))
    } else {
        Err(StoreError::Invalid(format!(
            "unsupported database url {url:?} (expected sqlite:// or postgres://)"
        )))
    }
}

/// Current time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// `"sqlite"` or `"postgres"`.
    fn backend(&self) -> &'static str;

    // --- orgs and tokens -------------------------------------------------

    /// Create an org with its user seat and a user-scope token.
    async fn create_org(
        &self,
        name: &str,
        user_seat: &str,
        token_hash: &str,
    ) -> Result<(Org, Seat)>;
    async fn org_by_name(&self, name: &str) -> Result<Option<Org>>;
    async fn update_org(&self, org_id: i64, patch: &OrgSettings) -> Result<Org>;
    async fn create_token(&self, token: &NewToken) -> Result<()>;
    /// Look up a live token by hash, with its org and (for user and seat
    /// tokens) its seat.
    async fn token_auth(&self, hash: &str) -> Result<Option<TokenAuth>>;

    // --- seats -------------------------------------------------------------

    async fn create_seat(&self, org_id: i64, seat: &NewSeat) -> Result<Seat>;
    async fn seat_by_name(&self, org_id: i64, name: &str) -> Result<Option<Seat>>;
    /// Live seats of the org with any of the given names.
    async fn seats_by_names(&self, org_id: i64, names: &[String]) -> Result<Vec<Seat>>;
    /// All live seats of an org, ordered by name.
    async fn seats(&self, org_id: i64) -> Result<Vec<Seat>>;
    async fn update_seat(&self, org_id: i64, name: &str, patch: &SeatUpdate) -> Result<Seat>;
    /// Soft delete: the seat disappears from the directory and lists, its seat
    /// tokens are revoked, its children move to its parent. Mail is kept.
    async fn delete_seat(&self, org_id: i64, name: &str) -> Result<()>;

    // --- lists -------------------------------------------------------------

    async fn create_list(&self, org_id: i64, list: &NewList) -> Result<List>;
    async fn list_by_name(&self, org_id: i64, name: &str) -> Result<Option<List>>;
    async fn lists(&self, org_id: i64) -> Result<Vec<List>>;
    async fn delete_list(&self, org_id: i64, name: &str) -> Result<()>;
    async fn list_members(&self, list_id: i64) -> Result<Vec<Member>>;
    /// Insert or update a member (keyed by address).
    async fn put_member(&self, org_id: i64, list_id: i64, member: &Member) -> Result<()>;
    async fn remove_member(&self, list_id: i64, address: &str) -> Result<bool>;

    // --- links ---------------------------------------------------------------

    /// Request a link to `peer`. If `peer` already asked us, this accepts.
    async fn request_link(&self, org_id: i64, peer_id: i64) -> Result<amail_proto::LinkState>;
    /// Accept a pending request from `peer`.
    async fn accept_link(&self, org_id: i64, peer_id: i64) -> Result<()>;
    /// Remove the link in both directions. Returns false if there was none.
    async fn revoke_link(&self, org_id: i64, peer_id: i64) -> Result<bool>;
    async fn links(&self, org_id: i64) -> Result<Vec<Link>>;
    async fn link_active(&self, org_id: i64, peer_id: i64) -> Result<bool>;

    // --- mail ----------------------------------------------------------------

    /// Store a message and its deliveries in one transaction, assigning each
    /// recipient seat the next value of its sequence counter.
    async fn send(&self, msg: &NewMessage) -> Result<Sent>;
    /// A message with its body.
    async fn message(&self, id: i64) -> Result<Option<MessageRec>>;
    /// True if the message was sent by `org_id` or delivered to one of its seats.
    async fn org_sees_message(&self, org_id: i64, message_id: i64) -> Result<bool>;
    /// True if the seat sent or received any message in the thread.
    async fn seat_in_thread(&self, seat_id: i64, thread_id: i64) -> Result<bool>;
    async fn delivery(&self, message_id: i64, seat_id: i64) -> Result<Option<DeliveryRec>>;
    /// Messages of a thread visible to `org_id`, oldest first, with bodies.
    async fn thread(&self, thread_id: i64, org_id: i64) -> Result<Vec<MessageRec>>;
    async fn inbox(&self, seat_id: i64, q: &InboxQuery) -> Result<Vec<Entry>>;
    /// Mark the seat's delivery read. `None` if the seat has no delivery.
    async fn mark_read(
        &self,
        message_id: i64,
        seat_id: i64,
    ) -> Result<Option<(DeliveryRec, Option<SeqBump>)>>;
    /// Mark the seat's delivery acknowledged (and read).
    async fn ack(
        &self,
        message_id: i64,
        seat_id: i64,
    ) -> Result<Option<(DeliveryRec, Option<SeqBump>)>>;
    /// Atomically mark matching unnotified deliveries as notified and return
    /// them with bodies. Concurrent claims never return the same delivery.
    async fn claim(&self, seat_id: i64, f: &ClaimFilter) -> Result<(Vec<Entry>, Option<SeqBump>)>;
    /// Deliveries created or changed after `since` (by `mod_seq`), ascending.
    async fn changes(&self, seat_id: i64, since: i64, limit: i64) -> Result<Vec<Entry>>;
    /// All deliveries of a message, across orgs.
    async fn recipients(&self, message_id: i64) -> Result<Vec<DeliveryRec>>;
    /// Messages visible to the org, newest first, without bodies.
    async fn archive(&self, org_id: i64, q: &ArchiveQuery) -> Result<Vec<MessageRec>>;
    /// `(seat_id, name, last_seq)` of every live seat in the org.
    async fn seat_seqs(&self, org_id: i64) -> Result<Vec<(i64, String, i64)>>;
}
