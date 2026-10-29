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
pub mod docmd;
#[macro_use]
mod common;
#[macro_use]
mod tasks;
#[macro_use]
mod docs;
#[macro_use]
mod handover;
#[macro_use]
mod requests;
mod model;
mod postgres;
mod sqlite;
pub mod testutil;

pub use handover::{
    HandoverSeatRec, NewNoteRec, NewWorklogEntry, NoteRec, WorklogQuery, WorklogRec,
    WorklogSummaryRec,
};
pub use model::*;
pub use requests::{
    NewRequestRec, RequestAnswerRec, RequestBounds, RequestEventRec, RequestQuery, RequestRec,
    RequestsDue,
};

/// How long a claim's idempotency key is kept: a retried claim with the
/// key returns what the first one took, and the claim can be released
/// (see [`Store::release_claim`]) for this long. It covers the longest
/// harness turn a released claim is expected to span.
pub const CLAIM_KEY_TTL_MS: i64 = 6 * 3_600_000;
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
    /// The query ran longer than
    /// [`amail_proto::limits::QUERY_TIMEOUT_MS`] and was cancelled.
    #[error("query cancelled: it took too long")]
    Timeout,
    /// A task write that breaks a rule (a bound, a cycle, a transition):
    /// a code for the API's error and a message.
    #[error("{1}")]
    Refused(&'static str, String),
    /// Internal: a dry run of an import ends with this error so that its
    /// transaction is rolled back; [`Store::import_tasks`] answers the
    /// result.
    #[error("dry run")]
    ImportDryRun(Box<TaskImportRes>),
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        match e.as_database_error() {
            Some(d) if d.is_unique_violation() => StoreError::Conflict(d.message().to_string()),
            // Postgres `query_canceled` (statement_timeout).
            Some(d) if d.code().as_deref() == Some("57014") => StoreError::Timeout,
            // SQLite `SQLITE_INTERRUPT`: only the query time limit interrupts.
            Some(d) if d.code().as_deref() == Some("9") => StoreError::Timeout,
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
    async fn org_by_id(&self, id: i64) -> Result<Option<Org>>;
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

    /// Request a link to `peer`, with a note for its user. If `peer`
    /// already asked us, this accepts.
    async fn request_link(
        &self,
        org_id: i64,
        peer_id: i64,
        note: Option<&str>,
    ) -> Result<amail_proto::LinkState>;
    /// Accept a pending request from `peer`.
    async fn accept_link(&self, org_id: i64, peer_id: i64) -> Result<()>;
    /// Remove the link in both directions. Returns false if there was none.
    async fn revoke_link(&self, org_id: i64, peer_id: i64) -> Result<bool>;
    /// Links and pending requests, including requests to names that are
    /// not an org (`peer_org_id` 0, state `requested`).
    async fn links(&self, org_id: i64) -> Result<Vec<Link>>;
    /// Record a request to a name that is not an (active) org, so that it
    /// is listed like any other request.
    async fn request_unknown_link(&self, org_id: i64, name: &str, note: Option<&str>)
    -> Result<()>;
    /// Remove such a request. Returns false if there was none.
    async fn revoke_unknown_link(&self, org_id: i64, name: &str) -> Result<bool>;
    async fn link_active(&self, org_id: i64, peer_id: i64) -> Result<bool>;

    // --- teams and openings ----------------------------------------------------

    /// Teams of the org, by name.
    async fn teams(&self, org_id: i64) -> Result<Vec<Team>>;
    /// Create the team (empty) or change it.
    async fn put_team(&self, org_id: i64, name: &str, change: &TeamChange) -> Result<Team>;
    /// Delete a team: its members leave it, its sub-teams move to its
    /// parent, its grants go. False if there was none.
    async fn delete_team(&self, org_id: i64, name: &str) -> Result<bool>;
    /// Grants by `filter` (see [`GrantFilter`]), ordered by id.
    async fn grants(&self, filter: &GrantFilter) -> Result<Vec<GrantRec>>;
    /// Add a grant on a seat or a team (`audience` `None`: every linked
    /// org); if the same grant exists it is kept. Returns its id.
    async fn add_grant(
        &self,
        org_id: i64,
        seat_id: Option<i64>,
        team_id: Option<i64>,
        audience: Option<&str>,
        managed: bool,
    ) -> Result<i64>;
    async fn remove_grant(&self, org_id: i64, id: i64) -> Result<bool>;
    /// Participants of these threads as org `viewer` knows them: every seat
    /// (with its org) of the thread's messages that reached more than one
    /// org and that a seat of `viewer` sent or received. Nothing of the
    /// messages `viewer` did not see.
    async fn thread_parts(&self, viewer: i64, thread_ids: &[i64]) -> Result<Vec<ThreadPeer>>;
    /// The org's primary contacts: `(peer org id, team id, seat id)`; team
    /// `None` is the org's own for that peer.
    async fn primaries(&self, org_id: i64) -> Result<Vec<(i64, Option<i64>, i64)>>;
    /// Set or clear (`None`) the org's (`team_id` `None`) or a team's
    /// primary contact for `peer_id`. The org's needs a link row.
    async fn set_primary(
        &self,
        org_id: i64,
        peer_id: i64,
        team_id: Option<i64>,
        seat_id: Option<i64>,
    ) -> Result<()>;
    /// Clear the org's one-time notice.
    async fn clear_org_notice(&self, org_id: i64) -> Result<()>;
    /// Take one token from the seat's budget of new threads to other orgs
    /// (`per_hour` tokens an hour, as many at most), or say how many
    /// milliseconds until one is available.
    async fn take_thread_token(&self, seat_id: i64, per_hour: i64) -> Result<Result<(), u64>>;
    /// Give back a token taken for a send that then failed.
    async fn refund_thread_token(&self, seat_id: i64, per_hour: i64) -> Result<()>;

    // --- mail ----------------------------------------------------------------

    /// Store a message and its deliveries in one transaction, assigning each
    /// recipient seat the next value of its sequence counter.
    async fn send(&self, msg: &NewMessage) -> Result<Sent>;
    /// A message with its body.
    async fn message(&self, id: i64) -> Result<Option<MessageRec>>;
    /// True if the message was sent by `org_id` or delivered to one of its seats.
    async fn org_sees_message(&self, org_id: i64, message_id: i64) -> Result<bool>;
    /// True if the seat sent or received any message in the thread (index
    /// lookups only, whatever the thread's size).
    async fn seat_in_thread(&self, seat_id: i64, thread_id: i64) -> Result<bool>;
    /// True if the org sent or received any message in the thread (index
    /// lookups only, whatever the thread's size).
    async fn org_sees_thread(&self, org_id: i64, thread_id: i64) -> Result<bool>;
    async fn delivery(&self, message_id: i64, seat_id: i64) -> Result<Option<DeliveryRec>>;
    /// Messages of a thread visible to `org_id`: the newest `limit`, oldest
    /// first; nothing (after one index lookup) when the org sees no
    /// message of the thread. Bodies are loaded newest first while their total stays within
    /// `body_bytes`; the others are left out. Only the newest
    /// [`amail_proto::limits::MAX_SCAN_MESSAGES`] messages of the thread
    /// (of all orgs) are looked at.
    async fn thread(
        &self,
        thread_id: i64,
        org_id: i64,
        limit: i64,
        body_bytes: usize,
    ) -> Result<Vec<ThreadMessage>>;
    async fn inbox(&self, seat_id: i64, q: &InboxQuery) -> Result<Vec<Entry>>;
    /// Mark the seat's delivery read. `None` if the seat has no delivery.
    async fn mark_read(
        &self,
        message_id: i64,
        seat_id: i64,
    ) -> Result<Option<(DeliveryRec, Option<SeqBump>)>>;
    /// Mark several of the seat's deliveries read in one transaction, each
    /// as `mark_read` does (read counts as notified). Only unread
    /// deliveries that were not withdrawn change; each gets its own new
    /// `mod_seq`. Returns how many changed and the seat's new seq (`None`
    /// when nothing changed).
    async fn mark_read_many(
        &self,
        seat_id: i64,
        selection: &ReadSelection,
    ) -> Result<(i64, Option<SeqBump>)>;
    /// Mark the seat's delivery not read again (an acknowledged delivery
    /// stays read). `None` if the seat has no delivery.
    async fn mark_unread(
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
    /// Withdraw the seat's delivery of a message while it has not been
    /// notified, read or acknowledged: mode `archive` and `withdrawn_at`
    /// (pending and unread counts drop it; it is never claimed). Decided
    /// atomically against a concurrent claim. Returns the delivery as it is
    /// afterwards, with a bump only when it changed; `None` when the seat
    /// has no delivery of the message.
    async fn withdraw(
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
    /// Undo part of a keyed claim (the claim's key is kept for
    /// [`CLAIM_KEY_TTL_MS`]): the seat's deliveries of `message_ids` that
    /// the claim with `key` took become "not notified, not read" again,
    /// unless acknowledged meanwhile. Returns the message ids released.
    /// Used when the harness turn that showed them failed.
    async fn release_claim(
        &self,
        seat_id: i64,
        key: &str,
        message_ids: &[i64],
    ) -> Result<(Vec<i64>, Option<SeqBump>)>;
    /// Deliveries created or changed after `since` (by `mod_seq`), ascending.
    async fn changes(&self, seat_id: i64, since: i64, limit: i64) -> Result<Vec<Entry>>;
    /// All deliveries of a message, across orgs.
    async fn recipients(&self, message_id: i64) -> Result<Vec<DeliveryRec>>;
    /// For each of `message_ids` that has deliveries (across orgs): how many
    /// are in each state, counted in the most advanced one. The caller
    /// bounds the number of ids.
    async fn delivery_counts(
        &self,
        message_ids: &[i64],
    ) -> Result<Vec<amail_proto::DeliveryCounts>>;
    /// Messages visible to the org, newest first, without bodies. The query
    /// starts from the org's own messages (or one of its seats'), never from
    /// the whole table.
    async fn archive(&self, org_id: i64, q: &ArchiveQuery) -> Result<ArchivePage>;
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
    /// `/v1/attention`), oldest first, among the seat's newest
    /// [`amail_proto::limits::MAX_ATTENTION_SCAN`] deliveries. Bodies hold
    /// only their first 400 characters (for a preview).
    async fn attention(&self, user_seat_id: i64, limit: i64) -> Result<Vec<Entry>>;
    /// Messages the org sent or received from minute `since_minute` (minutes
    /// since the Unix epoch) on, as added with [`Store::add_activity`].
    async fn activity(&self, org_id: i64, since_minute: i64) -> Result<i64>;
    /// Add `(org_id, minute, messages)` to the per-minute message counts.
    async fn add_activity(&self, counts: &[(i64, i64, i64)]) -> Result<()>;
    /// Substring search over the most recent messages visible to the org
    /// (`q.scan` of them before `q.before_id`), newest first.
    async fn search(&self, org_id: i64, q: &SearchQuery) -> Result<SearchResult>;

    // --- presence and server instances ------------------------------------

    /// Presence rows of the org's live seats (or of one seat), stale ones
    /// included.
    async fn presence(&self, org_id: i64, seat: Option<&str>) -> Result<Vec<PresenceRec>>;
    /// Store a node's report (seats that are not live seats of the org are
    /// ignored); returns the entries that changed, as stored, and the seats
    /// whose turn failure the report cleared. An entry older than `ttl_ms`
    /// counts as absent (for `changed`).
    async fn put_presence(
        &self,
        org_id: i64,
        recs: &[PresenceRec],
        ttl_ms: i64,
    ) -> Result<PresenceStored>;
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
    /// Drop expired claim keys, watches, server instances, browser sessions
    /// and link requests, and revoked tokens (with their sessions).
    async fn cleanup(&self) -> Result<()>;
    /// Notice mail the org received since `since` (ms): `(mails, bytes of
    /// subjects and bodies)`.
    async fn notice_usage(&self, org_id: i64, since: i64) -> Result<(i64, i64)>;
    /// Delete up to `limit` notice mails created before `cutoff`, with
    /// their deliveries; returns how many.
    async fn delete_old_notices(&self, cutoff: i64, limit: i64) -> Result<u64>;
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
    /// Delete, for the org, the messages of the thread it may delete: mail
    /// of deleted orgs it keeps (its deliveries of it; the message once no
    /// org keeps a delivery of it), and mail it sent that no other org has
    /// a delivery of. At most `limit` messages; `None` when the org sees no
    /// message of the thread.
    async fn delete_thread(
        &self,
        org_id: i64,
        thread_id: i64,
        limit: i64,
    ) -> Result<Option<MailDeleted>>;
    /// Delete, for the org, up to `limit` messages of deleted orgs it keeps,
    /// looking below message id `before` and starting no step of the walk
    /// after `time_limit` (`MailDeleted::next_before_id` continues).
    async fn purge_deleted_org_mail(
        &self,
        org_id: i64,
        limit: i64,
        before: Option<i64>,
        time_limit: Option<std::time::Duration>,
    ) -> Result<MailDeleted>;
    /// Retention: delete up to `limit` messages of deleted orgs created
    /// before `cutoff`, with all their deliveries; returns how many.
    async fn expire_deleted_org_mail(&self, cutoff: i64, limit: i64) -> Result<u64>;
    /// Load and activity per org, for the operator.
    async fn operator_stats(&self) -> Result<OperatorStats>;
    /// Replace a user token: issue `new`, revoke `old` and end its browser
    /// sessions. Returns the new token's id.
    async fn rotate_token(&self, org_id: i64, old: i64, new: &NewToken) -> Result<i64>;
    /// Revoke all user tokens of the org and issue one with this hash.
    async fn reset_user_tokens(&self, org_id: i64, hash: &str) -> Result<()>;

    // --- tasks -------------------------------------------------------------------
    //
    // Everything is scoped to one org. Writes record their events (one per
    // changed field), charge the org's `task_bytes` and are refused with
    // [`StoreError::StorageQuota`] over the quota they are given, and with
    // [`StoreError::Refused`] when they break a rule (bounds, cycles,
    // transitions).

    /// Create a task with the next number of the org (and its `created`
    /// event, `assigned` when it has an assignee, `linked` for
    /// `link_thread`).
    async fn create_task(&self, task: &NewTaskRec) -> Result<TaskRec> {
        Ok(self.create_task_once(task).await?.0)
    }
    /// [`Store::create_task`], except that with a `client_key` that created
    /// a task of the org in the last
    /// [`amail_proto::limits::TASK_CLIENT_KEY_HOURS`] hours it creates
    /// nothing and answers that task (true: it existed).
    async fn create_task_once(&self, task: &NewTaskRec) -> Result<(TaskRec, bool)>;
    /// A task by number, with its body.
    async fn task(&self, org_id: i64, num: i64) -> Result<Option<TaskRec>>;
    /// A task with its blockers, the tasks it blocks, its children, its
    /// links and its newest `events` events.
    async fn task_detail(
        &self,
        org_id: i64,
        num: i64,
        events: i64,
    ) -> Result<Option<TaskDetailRec>>;
    /// Change fields. A closed task can only be reopened (`open` or
    /// `doing`); `waiting_on` and `blocked_reason` go with `blocked` and
    /// are cleared when the task leaves it. Closing a task writes an
    /// `unblocked` event on each task whose last unsatisfied dependency it
    /// was (also when an ok result satisfies it). Closing a parent whose
    /// children are not all closed is refused (`children_open`) unless
    /// `force`; a result needs the state `done` (`not_done`); reopening
    /// clears the result.
    async fn change_task(&self, w: &TaskWrite, change: &TaskChange) -> Result<TaskChanged>;
    /// The actor asks to take over a task assigned to another seat and not
    /// started (stored `open`): one pending request per task. Refused with
    /// `not_assigned` (unassigned: `take` it), `own_task`, `started`
    /// (another state) and `steal_pending` (another seat asked first);
    /// asking again changes nothing.
    async fn steal_task(&self, w: &TaskWrite) -> Result<TaskChanged>;
    /// Answer the pending steal request: `give` assigns the task to the
    /// requester (`assigned` event noted `yielded to <seat> on request`),
    /// otherwise the request is cleared (`steal_declined`). Only the
    /// assignee answers, or the org's user (`as_user`): `not_assignee`.
    async fn answer_task_steal(
        &self,
        w: &TaskWrite,
        give: bool,
        as_user: bool,
    ) -> Result<TaskChanged>;
    /// End the steal requests made at `cutoff` or before (`steal_expired`),
    /// at most `limit`; each once.
    async fn expire_task_steals(&self, cutoff: i64, limit: i64) -> Result<Vec<StealExpiredRec>>;
    /// Assign an unassigned task to the actor, atomically (`taken` when
    /// someone has it; nothing changes when the actor has it).
    async fn take_task(&self, w: &TaskWrite) -> Result<TaskChanged>;
    /// The task depends on task `blocker` (by number). Refused when it
    /// would make a cycle, when the chain of dependencies the check follows
    /// is longer than [`amail_proto::limits::MAX_TASK_DEP_DEPTH`], or over
    /// the fan-out bounds.
    async fn add_task_dep(&self, w: &TaskWrite, blocker: i64) -> Result<TaskChanged> {
        self.set_task_dep(w, blocker, None).await
    }
    /// [`Store::add_task_dep`] with `needs_ok`: `Some` sets whether only an
    /// ok result of the blocker satisfies the dependency, also for an
    /// existing dependency; `None` leaves an existing one as it is (a new
    /// one: any close). A change that makes the task startable writes an
    /// `unblocked` event.
    async fn set_task_dep(
        &self,
        w: &TaskWrite,
        blocker: i64,
        needs_ok: Option<bool>,
    ) -> Result<TaskChanged>;
    /// Remove a dependency; the task gets an `unblocked` event when it was
    /// its last open blocker.
    async fn remove_task_dep(&self, w: &TaskWrite, blocker: i64) -> Result<TaskChanged>;
    /// Link a thread; false when it was linked. With `auto` (a `[tNN]`
    /// subject) a missing task, the task's own discussion thread and a
    /// task at its link bound are no error: nothing happens.
    async fn add_task_link(
        &self,
        w: &TaskWrite,
        thread_id: i64,
        auto: bool,
        note: Option<&str>,
    ) -> Result<bool>;
    /// Unlink a thread; false when it was not linked.
    async fn remove_task_link(&self, w: &TaskWrite, thread_id: i64) -> Result<bool>;
    /// Record a comment (a message in the discussion thread); the first
    /// makes `thread_id` the task's discussion thread.
    async fn record_task_comment(
        &self,
        w: &TaskWrite,
        message_id: i64,
        thread_id: i64,
    ) -> Result<TaskRec>;
    /// Record a reminder mail about a task (`reminder_sent`, no actor).
    async fn record_task_reminder(
        &self,
        org_id: i64,
        task_id: i64,
        kind: &str,
        to: &str,
        message_id: i64,
    ) -> Result<()>;
    /// Events after `after_seq`, oldest first, and how many earlier builds
    /// deleted (`events_compacted`); `None` when there is no such task.
    async fn task_events(
        &self,
        org_id: i64,
        num: i64,
        after_seq: i64,
        limit: i64,
    ) -> Result<Option<(Vec<TaskEventRec>, i64)>>;
    /// The seats that commented on a task, in the order of their first
    /// comment.
    async fn task_commenters(&self, org_id: i64, num: i64) -> Result<Vec<String>>;
    /// Tasks by [`TaskQuery`] (without bodies).
    async fn tasks(&self, q: &TaskQuery) -> Result<TaskPage>;
    /// The seat's waiting list: its tasks `blocked` with `waiting_on`
    /// (seat: that seat), and the unfinished blockers of its unfinished
    /// tasks (seat: the blocker's assignee, else reporter; other: the
    /// blocker). True when cut at `limit`.
    async fn tasks_waiting(
        &self,
        org_id: i64,
        seat_id: i64,
        limit: i64,
    ) -> Result<(Vec<TaskWaitRec>, bool)>;
    /// Who waits for the seat: tasks `blocked` with `waiting_on` it (seat:
    /// their assignee, else reporter), and its unfinished tasks (assigned,
    /// or unassigned and reported by it) that are the last open blocker of
    /// another seat's task (other: that task).
    async fn tasks_owed(
        &self,
        org_id: i64,
        seat_id: i64,
        limit: i64,
    ) -> Result<(Vec<TaskWaitRec>, bool)>;
    /// The board: counts and first tasks per shown state, load per
    /// assignee, stale (period `stale_hours`; 0: none) and overdue tasks,
    /// and tasks waiting on unfinished blockers.
    async fn task_board(
        &self,
        org_id: i64,
        team_id: Option<i64>,
        stale_hours: i64,
        now: i64,
    ) -> Result<TaskBoardRec>;
    /// Tasks of the org whose discussion thread this is or that link it.
    async fn thread_tasks(&self, org_id: i64, thread_id: i64) -> Result<Vec<TaskRec>>;
    /// Import records in one transaction: tasks first (a record whose key
    /// names a task updates the fields that differ), then parents and
    /// dependencies. Unknown parents or blockers and refused parents or
    /// dependencies are warnings. With `dry_run` nothing is kept.
    async fn import_tasks(&self, imp: &TaskImportRec) -> Result<TaskImportRes>;
    /// The lead's report (see `docs/reference.md`): `not_started`,
    /// `in_progress`, `review`, `rejected`, `approved`, `parents`,
    /// `blocked`, each with its count and first `limit` tasks.
    async fn task_report(
        &self,
        org_id: i64,
        team_id: Option<i64>,
        limit: i64,
    ) -> Result<TaskReportRec>;
    /// Unfinished tasks whose due time passed and that were not reminded
    /// of it, marked reminded (at most `limit`; each once across
    /// instances).
    async fn claim_due_tasks(&self, now: i64, limit: i64) -> Result<Vec<TaskRec>>;
    /// The stale pass of every org that had none for an hour: `doing` and
    /// `review` tasks not changed and not reminded for the org's stale
    /// period, marked reminded (at most `limit` per org).
    async fn claim_stale_tasks(&self, now: i64, limit: i64) -> Result<Vec<TaskRec>>;

    // --- documents ---------------------------------------------------------------
    //
    // Writes take the org's row, record events, charge the org's
    // `doc_bytes` and are refused with `StoreError::Refused` (a code and a
    // message: `not_owner`, `frozen`, `stale`, `sections_missing`,
    // `body_too_large`, `nothing_to_split`, ...) when they break a rule.

    /// Create a document (`draft`) with its first revision. A `client_key`
    /// that created a document of the org in the last 24 hours answers
    /// that document (`existing`).
    async fn create_doc(&self, doc: &NewDocRec) -> Result<DocChanged>;
    /// A document by number, with its current body.
    async fn doc(&self, org_id: i64, num: i64) -> Result<Option<DocRec>>;
    /// The number of the document with this slug.
    async fn doc_num_by_slug(&self, org_id: i64, slug: &str) -> Result<Option<i64>>;
    /// A document with its sections, relatives, links, threads, tasks,
    /// comments and newest `events` events.
    async fn doc_detail(&self, org_id: i64, num: i64, events: i64) -> Result<Option<DocDetailRec>>;
    /// One revision with its body.
    async fn doc_revision(&self, org_id: i64, num: i64, rev: i64) -> Result<Option<DocRevRec>>;
    /// Every revision, without bodies, oldest first.
    async fn doc_revisions(&self, org_id: i64, num: i64) -> Result<Vec<DocRevRec>>;
    /// Events after `after_seq` (at most `limit`); `None`: no such document.
    async fn doc_events(
        &self,
        org_id: i64,
        num: i64,
        after_seq: i64,
        limit: i64,
    ) -> Result<Option<Vec<DocEventRec>>>;
    /// The events after the one that wrote revision `rev`.
    async fn doc_events_since_rev(
        &self,
        org_id: i64,
        num: i64,
        rev: i64,
        limit: i64,
    ) -> Result<Vec<DocEventRec>>;
    /// Replace the body or one section (refused for an active decision,
    /// `frozen`); a body equal to the current one writes nothing
    /// (`unchanged`).
    async fn write_doc_body(&self, w: &DocWrite, body: &DocBodyRec) -> Result<DocChanged>;
    /// Change fields; title and description changes write a revision.
    async fn change_doc(&self, w: &DocWrite, change: &DocChange) -> Result<DocChanged>;
    /// Document `by` replaces `w.num`, which becomes `superseded`.
    async fn supersede_doc(&self, w: &DocWrite, by: i64) -> Result<DocChanged>;
    /// Confirm the content now, valid for `valid_ms` (`None`: the kind's
    /// interval; `Some(None)`: no expiry).
    async fn verify_doc(&self, w: &DocWrite, valid_ms: Option<Option<i64>>) -> Result<DocChanged>;
    /// Each heading of `level` becomes a child document; links to the moved
    /// sections are rewritten in other documents and in task bodies.
    async fn split_doc(&self, w: &DocWrite, level: i64) -> Result<DocMovedRec>;
    /// Move one section into a new document that supplements this one.
    async fn extract_doc(
        &self,
        w: &DocWrite,
        anchor: &str,
        title: &str,
        kind: Option<&str>,
    ) -> Result<DocMovedRec>;
    /// Append `w.num` to `into` as a section; `w.num` becomes superseded.
    async fn merge_doc(&self, w: &DocWrite, into: i64) -> Result<DocMovedRec>;
    /// Documents by [`DocQuery`] (without bodies), newest first.
    async fn docs(&self, q: &DocQuery) -> Result<Vec<DocRec>>;
    /// Sections (or, for terms all shorter than 3 characters, titles and
    /// descriptions) containing every term.
    async fn search_docs(&self, q: &DocSearchQuery) -> Result<DocSearchRes>;
    /// The documents of the per-turn index, in order (at most `scan`), and
    /// how many there are.
    async fn doc_index(
        &self,
        org_id: i64,
        team_id: Option<i64>,
        now: i64,
        scan: Option<i64>,
    ) -> Result<(Vec<DocRec>, i64)>;
    /// Documents of `kind` like a new one (see `docs/concepts.md`).
    #[allow(clippy::too_many_arguments)]
    async fn doc_candidates(
        &self,
        org_id: i64,
        kind: &str,
        title: &str,
        description: &str,
        slug: Option<&str>,
        except: i64,
        scan: Option<i64>,
    ) -> Result<Vec<(DocRec, f64)>>;
    /// Links out of a document and into it.
    async fn doc_links(
        &self,
        org_id: i64,
        num: i64,
    ) -> Result<Option<(Vec<DocLinkRec>, Vec<DocLinkRec>)>>;
    /// Dangling and stale links of the org's draft and active documents
    /// (of a team).
    async fn doc_check(
        &self,
        org_id: i64,
        team_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DocLinkRec>>;
    async fn doc_kinds(&self, org_id: i64) -> Result<Vec<DocKindRec>>;
    async fn doc_kind(&self, org_id: i64, name: &str) -> Result<Option<DocKindRec>>;
    /// Add a kind or change one (`max`: kinds per org).
    async fn put_doc_kind(
        &self,
        org_id: i64,
        kind: &DocKindPut,
        max: Option<i64>,
    ) -> Result<DocKindRec>;
    /// Record a comment (message `message_id` in `thread_id`, on section
    /// `anchor` with heading path `path`); the first one makes `thread_id`
    /// the document's discussion thread.
    async fn record_doc_comment(
        &self,
        w: &DocWrite,
        message_id: i64,
        thread_id: i64,
        anchor: Option<&str>,
        path: Option<&str>,
    ) -> Result<DocRec>;
    /// Seats that commented, in the order of their first comment.
    async fn doc_commenters(&self, org_id: i64, num: i64) -> Result<Vec<String>>;
    /// Link a thread that cites the document (`[d12]`); false when nothing
    /// was linked.
    async fn add_doc_thread(&self, w: &DocWrite, thread_id: i64, note: &str) -> Result<bool>;
    /// Link a task whose title cites documents; the numbers newly linked.
    async fn link_task_docs(
        &self,
        w: &DocWrite,
        task_id: i64,
        task_num: i64,
        nums: &[i64],
    ) -> Result<Vec<i64>>;
    /// Documents of the org whose discussion thread this is or that it
    /// cites.
    async fn thread_docs(&self, org_id: i64, thread_id: i64) -> Result<Vec<DocRec>>;
    /// Record a reminder mail about a document (`reminder_sent`).
    async fn record_doc_reminder(
        &self,
        org_id: i64,
        doc_id: i64,
        kind: &str,
        to: &str,
        message_id: i64,
    ) -> Result<()>;
    /// Draft and active documents whose `expires_at` passed, not reminded
    /// of it, marked reminded (at most `limit`; each once).
    async fn claim_expired_docs(&self, now: i64, limit: i64) -> Result<Vec<DocRec>>;
    /// Draft and active documents with dangling links not reminded of them
    /// for a day, marked reminded, with those links.
    async fn claim_dangling_docs(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<Vec<(DocRec, Vec<DocLinkRec>)>>;
    // --- seat handover (see `handover.rs`) ----------------------------------------

    /// Write the next revision of a seat's note. An `agent` note lacking
    /// one of the first six fields is refused with `note_fields_missing`,
    /// a note over [`amail_proto::MAX_HANDOVER_NOTE_CHARS`] with
    /// `note_too_large`. An empty `Pointers` is filled in from the worklog
    /// (tasks and documents of the session, else of the entries since the
    /// previous note, and the seat's last commit). Charged to the org's
    /// `task_bytes`.
    async fn write_handover_note(&self, note: &NewNoteRec) -> Result<NoteRec>;
    /// The seat's current note, or revision `rev`.
    async fn handover_note(
        &self,
        org_id: i64,
        seat_id: i64,
        rev: Option<i64>,
    ) -> Result<Option<NoteRec>>;
    /// The seat's newest `limit` revisions, newest first.
    async fn handover_notes(&self, org_id: i64, seat_id: i64, limit: i64) -> Result<Vec<NoteRec>>;
    /// The seat's latest revision, private setting and last request.
    async fn handover_seat(&self, org_id: i64, seat_id: i64) -> Result<HandoverSeatRec>;
    /// Record that `session` was shown revision `rev` (default: the
    /// current one); `None` when there is no such note.
    async fn record_handover_read(
        &self,
        org_id: i64,
        seat_id: i64,
        rev: Option<i64>,
        session: &str,
    ) -> Result<Option<NoteRec>>;
    /// The team file's `handover = "private"` for the seat.
    async fn set_handover_private(&self, org_id: i64, seat_id: i64, private: bool) -> Result<()>;
    /// Record a request for the seat's note by seat `by`.
    async fn request_handover(
        &self,
        org_id: i64,
        seat_id: i64,
        by: i64,
        at: i64,
    ) -> Result<HandoverSeatRec>;
    /// The server's note after the seat's task `num` closed: written
    /// (`State: idle`) only for an agent seat that holds no other unfinished
    /// task.
    async fn write_idle_note(
        &self,
        org_id: i64,
        seat_id: i64,
        num: i64,
        quota: Option<i64>,
    ) -> Result<Option<NoteRec>>;
    /// Append worklog entries of agent seats of the org (others are left out);
    /// the number written. Over the quota, the org's oldest entries are
    /// deleted to make room.
    async fn append_worklog(
        &self,
        org_id: i64,
        entries: &[NewWorklogEntry],
        quota: Option<i64>,
    ) -> Result<usize>;
    /// A seat's worklog entries, newest first.
    async fn worklog(&self, query: &WorklogQuery) -> Result<Vec<WorklogRec>>;
    /// Counts per kind, tasks touched and the last entries of a query.
    async fn worklog_summary(&self, query: &WorklogQuery) -> Result<WorklogSummaryRec>;
    /// Delete worklog entries older than `cutoff` (at most `limit`); the
    /// number deleted.
    async fn expire_worklog(&self, cutoff: i64, limit: i64) -> Result<u64>;

    // --- requests for the person (see `requests.rs`) ------------------------------
    //
    // Writes take the org's row, record events, charge the org's
    // `request_bytes` and are refused with `StoreError::Refused` (a code
    // and a message) when they break a rule.

    /// Create a request with the next number of the org (and its `created`
    /// event). Refused over the format rules (`title_empty`,
    /// `title_too_long`, `invalid_title`, `body_too_large`,
    /// `options_count`, `invalid_option_key`, `duplicate_option`,
    /// `invalid_option_label`, `option_label_too_long`,
    /// `option_description_too_long`, `invalid_default`,
    /// `free_text_not_allowed`) and the bounds (`too_many_open_requests`,
    /// `request_rate`). An `attention` request for a seat and code with a
    /// live request answers that one (true: it existed).
    async fn create_request(&self, r: &NewRequestRec) -> Result<(RequestRec, bool)>;
    async fn request(&self, org_id: i64, num: i64) -> Result<Option<RequestRec>>;
    /// The newest `limit` events, oldest first.
    async fn request_events(
        &self,
        org_id: i64,
        num: i64,
        limit: i64,
    ) -> Result<Vec<RequestEventRec>>;
    /// Threads other than its own that name the request.
    async fn request_threads(&self, org_id: i64, num: i64) -> Result<Vec<i64>>;
    /// Requests by [`RequestQuery`]: open ones first (`now`, `soon`,
    /// `later`, oldest first), then the others, newest first; true when
    /// cut at the limit.
    async fn requests(&self, q: &RequestQuery) -> Result<(Vec<RequestRec>, bool)>;
    /// Record the request's mail thread and its first message.
    async fn set_request_thread(
        &self,
        org_id: i64,
        num: i64,
        thread_id: i64,
        message_id: i64,
    ) -> Result<()>;
    /// Answer an open request. Refused with `answered` (answered or
    /// expired already: the caller shows the first answer), `not_open`
    /// (withdrawn or superseded), `stale` (`if_rev` differs),
    /// `invalid_option`, `free_text_not_allowed`, `answer_required`,
    /// `answer_too_long`, `note_too_long`.
    async fn answer_request(&self, a: &RequestAnswerRec) -> Result<RequestRec>;
    /// Withdraw an open request (`not_open` otherwise).
    async fn withdraw_request(
        &self,
        org_id: i64,
        num: i64,
        actor: &str,
        note: Option<&str>,
    ) -> Result<RequestRec>;
    /// Record an event that changes nothing else (`commented`, `mailed`);
    /// `None`: no such request.
    async fn record_request_event(
        &self,
        org_id: i64,
        num: i64,
        actor: Option<&str>,
        kind: amail_proto::RequestEventKind,
        note: Option<&str>,
        message_id: Option<i64>,
    ) -> Result<Option<RequestRec>>;
    /// Link a thread that names the request (`[r12]`); false when nothing
    /// was linked (linked already, the request's own thread, no request).
    async fn link_request_thread(
        &self,
        org_id: i64,
        num: i64,
        thread_id: i64,
        actor: Option<&str>,
        note: &str,
    ) -> Result<bool>;
    /// Requests whose own thread this is.
    async fn thread_requests(&self, org_id: i64, thread_id: i64) -> Result<Vec<RequestRec>>;
    /// The deadline pass (see [`RequestsDue`]).
    async fn claim_due_requests(&self, now: i64, limit: i64) -> Result<RequestsDue>;
    /// The org's `attention` requests whose condition the node still
    /// reports.
    async fn live_attention_requests(&self, org_id: i64) -> Result<Vec<RequestRec>>;
    /// The node reports an `attention` request's condition gone: it is no
    /// longer live and, if open, withdrawn by `node`.
    async fn end_attention_request(&self, org_id: i64, num: i64) -> Result<Option<RequestRec>>;
}
