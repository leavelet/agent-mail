//! Storage for agent-mail.
//!
//! [`Store`] is the single interface the server uses. It has two
//! implementations with identical schemas and semantics: [`SqliteStore`]
//! (default, single file, one writer task with group commit) and [`PgStore`].
//!
//! The store enforces data invariants (unique names, per-seat sequence
//! numbers, atomic sends, exclusive claims). Policy (who may send to whom,
//! rate limits, address resolution) lives in the server.

mod budget;
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
    /// The message would take the org's stored bytes over its quota; the
    /// value is what the org has stored.
    #[error("storage quota exceeded ({0} bytes stored)")]
    StorageQuota(i64),
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
    /// Links and pending requests, including requests to names that are
    /// not an org (`peer_org_id` 0, state `requested`).
    async fn links(&self, org_id: i64) -> Result<Vec<Link>>;
    /// Record a request to a name that is not an (active) org, so that it
    /// is listed like any other request.
    async fn request_unknown_link(&self, org_id: i64, name: &str) -> Result<()>;
    /// Remove such a request. Returns false if there was none.
    async fn revoke_unknown_link(&self, org_id: i64, name: &str) -> Result<bool>;
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
    /// Messages of a thread visible to `org_id`, with bodies: the newest
    /// `limit`, oldest first.
    async fn thread(&self, thread_id: i64, org_id: i64, limit: i64) -> Result<Vec<MessageRec>>;
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
    ///
    /// With `f.key`: if the seat made a claim with that key before (within
    /// about ten minutes), nothing new is claimed and that claim's
    /// deliveries are returned again (with their current state), also when
    /// the two claims run concurrently. A retry after a lost response thus
    /// gets the same batch.
    async fn claim(&self, seat_id: i64, f: &ClaimFilter) -> Result<(Vec<Entry>, Option<SeqBump>)>;
    /// Deliveries created or changed after `since` (by `mod_seq`), ascending.
    async fn changes(&self, seat_id: i64, since: i64, limit: i64) -> Result<Vec<Entry>>;
    /// All deliveries of a message, across orgs.
    async fn recipients(&self, message_id: i64) -> Result<Vec<DeliveryRec>>;
    /// Messages visible to the org, newest first, without bodies.
    async fn archive(&self, org_id: i64, q: &ArchiveQuery) -> Result<Vec<MessageRec>>;
    /// `(seat_id, name, last_seq)` of every live seat in the org.
    async fn seat_seqs(&self, org_id: i64) -> Result<Vec<(i64, String, i64)>>;
    /// Live (unrevoked) tokens of the org.
    async fn tokens(&self, org_id: i64) -> Result<Vec<TokenRec>>;
    /// Revoke a token of the org and end its browser sessions. Returns
    /// false if there was no live token with that id.
    async fn revoke_token(&self, org_id: i64, id: i64) -> Result<bool>;
    /// Pending-notification counts of every live seat of the org (or only
    /// `seat_id`), including seats with nothing pending.
    async fn pending(&self, org_id: i64, seat_id: Option<i64>) -> Result<Vec<PendingRec>>;

    // --- web interface ------------------------------------------------------

    /// The token behind a live (unexpired) browser session, if its token is
    /// still live.
    async fn session_auth(&self, hash: &str) -> Result<Option<TokenAuth>>;
    /// Store a browser session; the token's oldest sessions beyond
    /// `amail_proto::limits::MAX_SESSIONS_PER_TOKEN` end.
    async fn create_session(&self, hash: &str, token_id: i64, expires_at: i64) -> Result<()>;
    async fn delete_session(&self, hash: &str) -> Result<bool>;
    /// The org's user seat.
    async fn user_seat(&self, org_id: i64) -> Result<Option<Seat>>;
    /// Unread and pending counts of every live seat (or only `seat_id`).
    async fn counts(&self, org_id: i64, seat_id: Option<i64>) -> Result<Vec<CountRec>>;
    /// Deliveries to the user seat that wait for the user (see the server's
    /// `/v1/attention`), oldest first.
    async fn attention(&self, user_seat_id: i64, limit: i64) -> Result<Vec<Entry>>;
    /// Messages visible to the org created at or after `since`.
    async fn message_count(&self, org_id: i64, since: i64) -> Result<i64>;
    /// Full-text search over messages visible to the org, newest first, with
    /// bodies.
    async fn search(&self, org_id: i64, q: &SearchQuery) -> Result<SearchResult>;

    // --- presence and server instances ------------------------------------

    /// Presence rows of the org's live seats (or of one seat), stale ones
    /// included.
    async fn presence(&self, org_id: i64, seat: Option<&str>) -> Result<Vec<PresenceRec>>;
    /// Store a node's report (seats that are not live seats of the org are
    /// ignored); returns the entries that changed, as stored. An entry
    /// older than `ttl_ms` counts as absent.
    async fn put_presence(
        &self,
        org_id: i64,
        recs: &[PresenceRec],
        ttl_ms: i64,
    ) -> Result<Vec<PresenceRec>>;
    /// Take one token from the seat's send budget, or say how many
    /// milliseconds until one is available. Exact across server instances.
    async fn take_send_token(
        &self,
        seat_id: i64,
        per_min: i64,
        burst: i64,
    ) -> Result<Result<(), u64>>;
    /// Give back a token taken for a send that then failed.
    async fn refund_send_token(&self, seat_id: i64, burst: i64) -> Result<()>;
    async fn heartbeat_instance(&self, id: &str, url: Option<&str>) -> Result<()>;
    async fn remove_instance(&self, id: &str) -> Result<()>;
    /// Replace the instance's per-org counts of open node event streams.
    async fn set_node_streams(&self, instance: &str, counts: &[(i64, i64)]) -> Result<()>;
    /// Open node event streams per org on the other live instances.
    async fn node_streams(&self, except: &str, fresh_since: i64) -> Result<Vec<(i64, i64)>>;
    /// Record (`Some(expiry)`) or drop (`None`) that `instance` has browsers
    /// watching the seat's conversation.
    async fn watch_mirror(
        &self,
        org_id: i64,
        seat: &str,
        instance: &str,
        expires_at: Option<i64>,
    ) -> Result<()>;
    /// `(instance id, url)` of the instances watching the seat.
    async fn mirror_watchers(
        &self,
        org_id: i64,
        seat: &str,
    ) -> Result<Vec<(String, Option<String>)>>;
    /// Drop expired claim keys, watches, server instances and browser
    /// sessions, and revoked tokens (with their sessions).
    async fn cleanup(&self) -> Result<()>;
    /// Which of these token ids are live, and which of these browser
    /// sessions (by hash) are live and belong to a live token.
    async fn live_credentials(
        &self,
        tokens: &[i64],
        sessions: &[String],
    ) -> Result<(Vec<i64>, Vec<String>)>;
    /// Row counts of some tables (tests and diagnostics).
    async fn row_counts(&self) -> Result<RowCounts>;

    // --- attention -------------------------------------------------------------

    /// Set (or with `None` clear) the seat's focus; the seat's counter moves.
    async fn set_focus(&self, seat_id: i64, until: Option<i64>) -> Result<SeqBump>;
    /// The org's hidden `system` seat (created on first use).
    async fn system_seat(&self, org_id: i64) -> Result<Seat>;
    /// Kind and focus of the given seats.
    async fn recipient_states(&self, seat_ids: &[i64]) -> Result<Vec<RecipientState>>;
    /// Acknowledgement-requiring messages whose next escalation step is due.
    async fn due_acks(&self, now: i64, limit: i64) -> Result<Vec<AckDue>>;
    /// Move a message from escalation step `from` to `to`, with the next
    /// due time; false if another instance did it first.
    async fn advance_ack(
        &self,
        message_id: i64,
        from: i64,
        to: i64,
        next_due: Option<i64>,
    ) -> Result<bool>;
    /// Recipients that have not acknowledged the message (not the sender,
    /// not `archive` members).
    async fn laggards(&self, message_id: i64) -> Result<Vec<Laggard>>;

    // --- public mode ----------------------------------------------------------

    /// Store a registration invite (by the hash of its code).
    async fn create_invite(
        &self,
        hash: &str,
        note: Option<&str>,
        expires_at: Option<i64>,
    ) -> Result<i64>;
    /// Every invite, oldest first.
    async fn invites(&self) -> Result<Vec<InviteRec>>;
    /// Revoke an unused invite; false if there was none.
    async fn revoke_invite(&self, id: i64) -> Result<bool>;
    /// Use the invite with this hash and create the org (see
    /// [`Store::create_org`]), atomically: an invite is used at most once,
    /// and only if the org could be created.
    async fn register(
        &self,
        invite_hash: &str,
        org: &str,
        user_seat: &str,
        token_hash: &str,
    ) -> Result<std::result::Result<(Org, Seat), InviteError>>;
    async fn create_operator_token(&self, hash: &str, label: Option<&str>) -> Result<i64>;
    /// Live operator tokens.
    async fn operator_tokens(&self) -> Result<Vec<OperatorTokenRec>>;
    async fn revoke_operator_token(&self, id: i64) -> Result<bool>;
    /// The id of the live operator token with this hash.
    async fn operator_auth(&self, hash: &str) -> Result<Option<i64>>;
    /// The operator token behind a live browser session.
    async fn operator_session_auth(&self, hash: &str) -> Result<Option<i64>>;
    async fn create_operator_session(
        &self,
        hash: &str,
        token_id: i64,
        expires_at: i64,
    ) -> Result<()>;
    async fn delete_operator_session(&self, hash: &str) -> Result<bool>;
    /// Every org, by name.
    async fn orgs(&self) -> Result<Vec<Org>>;
    /// Suspend (`Some(time)`) or resume (`None`) an org; false if none.
    async fn set_org_suspended(&self, org_id: i64, at: Option<i64>) -> Result<bool>;
    /// Delete an org with all its data (see the backends); false if none.
    async fn delete_org(&self, org_id: i64) -> Result<bool>;
    /// Clear the bodies of up to `limit` messages created before `cutoff`;
    /// returns how many were cleared.
    async fn expire_bodies(&self, cutoff: i64, limit: i64) -> Result<u64>;
    /// Load and activity per org, for the operator.
    async fn operator_stats(&self) -> Result<OperatorStats>;
    /// Replace a user token: issue `new`, revoke `old` and end its browser
    /// sessions. Returns the new token's id.
    async fn rotate_token(&self, org_id: i64, old: i64, new: &NewToken) -> Result<i64>;
    /// Revoke all user tokens of the org and issue one with this hash.
    async fn reset_user_tokens(&self, org_id: i64, hash: &str) -> Result<()>;
}
