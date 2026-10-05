//! Store-level records. Timestamps are milliseconds since the Unix epoch.

use amail_proto::{
    DeliveryClass, DeliveryMode, LinkState, Meta, OrgVisibility, Outbound, PermissionMode,
    PostPolicy, Priority, SeatKind, TokenScope,
};

pub use amail_proto::{NewList, NewSeat};
/// Seat changes: a missing field is unchanged, `Some(None)` clears it.
pub type SeatUpdate = amail_proto::SeatPatch;
/// Org settings changes: a missing field is unchanged.
pub type OrgSettings = amail_proto::OrgPatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Org {
    pub id: i64,
    pub name: String,
    pub send_rate_per_min: i64,
    pub send_burst: i64,
    pub max_recipients: i64,
    pub max_thread_depth: i64,
    pub webhook_url: Option<String>,
    pub ack_timeout_min: i64,
    pub focus_max_min: i64,
    pub digest_interval_min: i64,
    pub created_at: i64,
    /// Suspended by the operator: its tokens do not work and mail to it is
    /// refused.
    pub suspended_at: Option<i64>,
    /// Bytes of the messages the org sent (see [`message_bytes`]); its
    /// sends are refused when they would take it over the storage quota.
    pub stored_bytes: i64,
    /// Bytes of the mail of deleted orgs that the org keeps (see
    /// [`message_bytes`]); not counted against the storage quota.
    pub kept_bytes: i64,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub visibility: OrgVisibility,
    pub outbound: Outbound,
    pub new_threads_per_hour: i64,
    /// A one-time notice for the user.
    pub notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seat {
    pub id: i64,
    pub org_id: i64,
    pub name: String,
    pub kind: SeatKind,
    pub parent_id: Option<i64>,
    pub parent_name: Option<String>,
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    /// The name of the seat's team.
    pub group: Option<String>,
    pub team_id: Option<i64>,
    pub description: Option<String>,
    /// A contact of its team.
    pub contact: bool,
    pub last_seq: i64,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
    /// Focus mode (only urgent mail is shown) until then.
    pub focus_until: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewToken {
    pub org_id: i64,
    pub scope: TokenScope,
    /// The seat for `seat` tokens and the user seat for `user` tokens.
    pub seat_id: Option<i64>,
    pub hash: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TokenAuth {
    pub token_id: i64,
    pub scope: TokenScope,
    pub org: Org,
    /// Set for `seat` and `user` tokens.
    pub seat: Option<Seat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    pub id: i64,
    pub org_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub post_policy: PostPolicy,
    /// Maintained by `amail up`.
    pub managed: bool,
    pub member_count: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// `name` for a local seat (then `seat_id` is set), `name@org` otherwise.
    pub address: String,
    pub seat_id: Option<i64>,
    pub mode: DeliveryMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub peer_org_id: i64,
    pub peer_name: String,
    pub state: LinkState,
    /// When it became active, or (not active) when it was requested.
    pub since: i64,
    /// The requester's note.
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Team {
    pub id: i64,
    pub org_id: i64,
    pub name: String,
    pub description: Option<String>,
    pub root_seat_id: Option<i64>,
    pub root: Option<String>,
    pub parent_id: Option<i64>,
    pub parent: Option<String>,
    pub managed: bool,
    pub created_at: i64,
}

/// Changes of a team (see [`crate::Store::put_team`]): a missing field is
/// unchanged, `Some(None)` clears it.
#[derive(Debug, Clone, Default)]
pub struct TeamChange {
    pub description: Option<Option<String>>,
    /// A seat name of the org.
    pub root: Option<Option<String>>,
    /// A team name of the org.
    pub parent: Option<Option<String>>,
    /// Replace the contacts (member seat names).
    pub contacts: Option<Vec<String>>,
    pub managed: Option<bool>,
}

/// The visible part of a seat or team: what a card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRec {
    /// Seat or team id.
    pub id: i64,
    pub name: String,
    /// `None`: a team.
    pub kind: Option<SeatKind>,
    pub description: Option<String>,
}

/// What a grant opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantTarget {
    Seat(CardRec),
    /// A team, with its contact seats.
    Team {
        team: CardRec,
        contacts: Vec<CardRec>,
    },
}

/// A grant as stored, with its target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRec {
    pub id: i64,
    pub org_id: i64,
    pub org_name: String,
    /// `None`: every linked org.
    pub audience: Option<String>,
    pub target: GrantTarget,
    pub managed: bool,
    pub created_at: i64,
}

/// Which grants [`crate::Store::grants`] returns.
#[derive(Debug, Clone, Default)]
pub struct GrantFilter {
    /// Grants of this org.
    pub grantor: Option<i64>,
    /// Grants toward this org (id, name): of orgs with an active link
    /// with it, whose audience is every linked org or this org.
    pub audience: Option<(i64, String)>,
}

/// A seat of another org that wrote into a thread (see
/// [`crate::Store::thread_peers`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPeer {
    pub thread_id: i64,
    pub org_id: i64,
    pub org_name: String,
    pub card: CardRec,
}

#[derive(Debug, Clone)]
pub struct NewMessage {
    pub org_id: i64,
    pub sender_seat_id: i64,
    /// `None` starts a new thread whose id is the message id.
    pub thread_id: Option<i64>,
    pub parent_id: Option<i64>,
    pub depth: i64,
    pub subject: String,
    pub body: String,
    /// Fully qualified (`name@org`) addresses as written by the sender.
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub priority: Priority,
    pub ack_required: bool,
    pub meta: Meta,
    /// One per distinct recipient seat. Must not be empty.
    pub deliveries: Vec<NewDelivery>,
    /// With `ack_required`: when the sender is told about recipients that
    /// have not acknowledged.
    pub ack_due: Option<i64>,
    /// Refuse the message ([`crate::StoreError::StorageQuota`]) if the
    /// org's stored bytes would then exceed this.
    pub storage_quota: Option<i64>,
    /// A notice of the mail system (`POST /v1/node/notices`): charged to
    /// nobody (see [`message_bytes`]) and deleted after
    /// [`amail_proto::limits::NOTICE_RETENTION_DAYS`].
    pub notice: bool,
}

/// The bytes a message counts against the storage quota without its
/// deliveries: its subject, body, meta (a JSON object, or nothing when
/// empty) and address lists as stored, plus
/// [`amail_proto::limits::STORED_BYTES_PER_MESSAGE`]. Each of its deliveries
/// adds [`amail_proto::limits::STORED_BYTES_PER_DELIVERY`].
///
/// Who is charged: the sending org, for the message and all its deliveries
/// (in any org), in its `stored_bytes`. Once the sending org is deleted,
/// each org that keeps deliveries of the message is charged the message's
/// full bytes and its own deliveries in its `kept_bytes`, which is not
/// counted against the quota. The SQL form of this rule is
/// `testutil::RECOUNT`. Notice mail (`messages.notice`, see
/// [`NewMessage::notice`]) is charged to nobody: it goes only to its own
/// org, and its daily budget and retention bound it.
pub fn message_bytes(subject: &str, body: &str, meta_db: &str, to_db: &str, cc_db: &str) -> i64 {
    amail_proto::limits::STORED_BYTES_PER_MESSAGE
        + (subject.len() + body.len() + meta_db.len() + to_db.len() + cc_db.len()) as i64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDelivery {
    pub seat_id: i64,
    pub org_id: i64,
    pub class: DeliveryClass,
    pub mode: DeliveryMode,
}

/// A seat's sequence counter moved to `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqBump {
    pub org_id: i64,
    pub seat_id: i64,
    pub seq: i64,
}

#[derive(Debug, Clone)]
pub struct Sent {
    pub message_id: i64,
    pub thread_id: i64,
    pub created_at: i64,
    /// One per delivery: the recipient seat's new sequence number.
    pub seqs: Vec<SeqBump>,
}

#[derive(Debug, Clone)]
pub struct MessageRec {
    pub id: i64,
    pub org_id: i64,
    pub org_name: String,
    pub sender_seat_id: i64,
    pub sender_name: String,
    pub sender_kind: SeatKind,
    pub thread_id: i64,
    pub parent_id: Option<i64>,
    pub depth: i64,
    pub subject: String,
    /// Empty when loaded as a header only.
    pub body: String,
    pub lines: i64,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub priority: Priority,
    pub ack_required: bool,
    pub meta: Meta,
    pub created_at: i64,
    /// When the retention period cleared the body.
    pub expired_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct DeliveryRec {
    pub id: i64,
    pub org_id: i64,
    pub org_name: String,
    pub message_id: i64,
    pub seat_id: i64,
    pub seat_name: String,
    pub class: DeliveryClass,
    pub mode: DeliveryMode,
    pub seq: i64,
    pub mod_seq: i64,
    pub notified_at: Option<i64>,
    pub notified_via: Option<String>,
    pub read_at: Option<i64>,
    pub acked_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub delivery: DeliveryRec,
    pub message: MessageRec,
}

#[derive(Debug, Clone, Default)]
pub struct InboxQuery {
    pub unread_only: bool,
    /// Return entries with `seq > after_seq`, oldest first.
    pub after_seq: Option<i64>,
    /// Return the newest entries with `seq < before_seq` (still oldest first).
    /// Takes precedence over `after_seq`.
    pub before_seq: Option<i64>,
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct ClaimFilter {
    pub classes: Vec<DeliveryClass>,
    pub urgent_only: bool,
    pub include_digest: bool,
    pub via: String,
    pub limit: i64,
    /// Mark read, in the same change, the claimed deliveries whose message
    /// body has at most this many characters.
    pub read_body_max: Option<i64>,
    /// Claim `digest` deliveries only (instead of `immediate` ones).
    pub digest_only: bool,
    /// Idempotency key: a claim with a key the seat used before returns the
    /// deliveries that claim took (see [`crate::Store::claim`]).
    pub key: Option<String>,
}

impl ClaimFilter {
    pub fn has(&self, c: DeliveryClass) -> bool {
        self.classes.contains(&c)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ArchiveQuery {
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// Sender as `(org name, seat name)`.
    pub from: Option<(String, String)>,
    /// A recipient seat as `(org name, seat name)`: messages delivered to it.
    pub to: Option<(String, String)>,
    pub thread_id: Option<i64>,
    pub before_id: Option<i64>,
    pub limit: i64,
    /// Time after which the query stops looking at further steps of
    /// messages and answers with what it found and a cursor
    /// (`next_before_id`). Checked between steps; `None`: no limit.
    pub time_limit: Option<std::time::Duration>,
}

/// One page of [`ArchiveQuery`]: messages newest first, without bodies.
#[derive(Debug, Clone, Default)]
pub struct ArchivePage {
    pub messages: Vec<MessageRec>,
    /// Where the next (older) page starts (`before_id`): set when the page
    /// is full, or when the request looked at its share of the org's
    /// messages ([`amail_proto::limits::MAX_SCAN_MESSAGES`]) without
    /// filling the page. `None`: nothing older matches.
    pub next_before_id: Option<i64>,
}

/// A message of a thread; bodies past the request's byte budget are left
/// out (`body` empty, `body_omitted` set).
#[derive(Debug, Clone)]
pub struct ThreadMessage {
    pub message: MessageRec,
    pub body_omitted: bool,
}

/// A live token, without its secret.
#[derive(Debug, Clone)]
pub struct TokenRec {
    pub id: i64,
    pub scope: TokenScope,
    /// The token's seat (seat and user tokens).
    pub seat_name: Option<String>,
    pub label: Option<String>,
    pub created_at: i64,
}

/// Pending (unnotified, `immediate`) deliveries of one seat by class, plus
/// the seat's sequence counter, read in one snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingRec {
    pub seat_id: i64,
    pub seat_name: String,
    /// The org's user seat (it has no session).
    pub user: bool,
    pub last_seq: i64,
    pub to: i64,
    pub cc: i64,
    pub list: i64,
    pub urgent: i64,
    /// Pending `digest` deliveries, and the creation time of the oldest.
    pub digest: i64,
    pub digest_since: Option<i64>,
    pub focus_until: Option<i64>,
}

/// What storing a node's presence report changed (see
/// [`crate::Store::put_presence`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceStored {
    /// The entries that changed or were absent, as stored.
    pub changed: Vec<PresenceRec>,
    /// Seats whose stored entry (stale or not) had a turn failure that the
    /// report no longer has.
    pub turn_failures_cleared: Vec<String>,
}

/// A seat's presence as reported by a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceRec {
    pub seat: String,
    pub state: amail_proto::SeatState,
    pub wake_failed: bool,
    pub harness: Option<String>,
    pub node: Option<String>,
    /// When the state last changed.
    pub since: i64,
    /// When the node last reported the seat.
    pub updated_at: i64,
    pub attention: Option<String>,
    pub attention_hint: Option<String>,
    pub terminal_url: Option<String>,
    pub turn_failed: Option<amail_proto::TurnFailure>,
}

impl PresenceRec {
    /// Whether `next` (a new report) differs from `self` in anything but
    /// its timestamps.
    pub fn same_as(&self, next: &PresenceRec) -> bool {
        self.state == next.state
            && self.wake_failed == next.wake_failed
            && self.harness == next.harness
            && self.attention == next.attention
            && self.attention_hint == next.attention_hint
            && self.terminal_url == next.terminal_url
            && self.turn_failed == next.turn_failed
    }
}

/// What a sender should know about a recipient seat (see
/// [`crate::Store::recipient_states`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientState {
    pub seat_id: i64,
    pub name: String,
    pub kind: SeatKind,
    pub focus_until: Option<i64>,
}

/// An acknowledgement-requiring message whose next escalation step is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckDue {
    pub message_id: i64,
    pub org_id: i64,
    pub sender_seat_id: i64,
    /// Escalation steps already taken (0: none).
    pub stage: i64,
    pub created_at: i64,
    pub due: i64,
}

/// A recipient that has not acknowledged a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Laggard {
    pub seat_id: i64,
    pub org_id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
}

/// A substring search (`Store::search`). Every term must occur in the
/// subject or the body (case-insensitive substring).
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub terms: Vec<String>,
    pub before_id: Option<i64>,
    pub limit: i64,
    /// How many of the org's messages, newest first before `before_id`,
    /// the request looks at.
    pub scan: i64,
    /// As [`ArchiveQuery::time_limit`].
    pub time_limit: Option<std::time::Duration>,
}

/// One match of a [`SearchQuery`]: the message without its body, and an
/// excerpt of the body around the first match.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub message: MessageRec,
    /// Up to [`SNIPPET_CHARS`] characters of the body.
    pub excerpt: String,
    /// Whether the body has text before and after the excerpt.
    pub cut_before: bool,
    pub cut_after: bool,
}

/// Characters of the body a search hit carries.
pub const SNIPPET_CHARS: i64 = 160;

/// Result of [`SearchQuery`]: matches newest first.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Where to continue (`before_id`): set when the page is full or when
    /// `scan` messages were looked at; `None` when nothing older is left.
    pub next_before_id: Option<i64>,
}

/// Mailbox counters of one seat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountRec {
    pub seat_id: i64,
    pub seat_name: String,
    pub last_seq: i64,
    pub unread: i64,
    pub pending: i64,
    pub focus_until: Option<i64>,
}

/// A registration invite (the code itself is never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRec {
    pub id: i64,
    pub note: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub used_at: Option<i64>,
    /// The org registered with it.
    pub used_by: Option<String>,
    pub revoked_at: Option<i64>,
    /// Registrations refused with `org_exists`; at
    /// `amail_proto::limits::MAX_INVITE_NAME_CONFLICTS` the invite is spent.
    pub name_conflicts: i64,
}

/// An operator token (no secret).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorTokenRec {
    pub id: i64,
    pub label: Option<String>,
    pub created_at: i64,
}

/// Why a registration was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InviteError {
    /// Unknown, used, revoked or expired: indistinguishable to the caller.
    Invalid,
    /// The org name is taken.
    OrgExists,
}

/// Load and activity of one org, for the operator (no mail content).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgStat {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub suspended_at: Option<i64>,
    /// Live agent seats.
    pub seats: i64,
    pub messages: i64,
    pub messages_hour: i64,
    pub messages_day: i64,
    /// When the org last sent a message.
    pub last_activity: Option<i64>,
    /// Bytes of the messages the org sent (see [`message_bytes`]).
    pub stored_bytes: i64,
    /// Bytes of the mail of deleted orgs the org keeps.
    pub kept_bytes: i64,
}

/// What [`crate::Store::delete_thread`] and
/// [`crate::Store::purge_deleted_org_mail`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailDeleted {
    /// Messages the org no longer has.
    pub deleted: i64,
    /// Messages of the thread the org cannot delete: mail exchanged with
    /// orgs that still exist.
    pub kept: i64,
    /// More could be deleted with another call.
    pub more: bool,
    /// Purge: the `before` of the next call.
    pub next_before_id: Option<i64>,
    /// The new counter of each seat that lost deliveries (in seat id
    /// order), so that its mailbox clients and its node look again.
    pub seqs: Vec<SeqBump>,
}

/// What [`crate::Store::operator_stats`] reports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OperatorStats {
    /// Busiest first (messages in the last hour, then the last day).
    pub orgs: Vec<OrgStat>,
    /// Messages sent in the last five minutes, all orgs.
    pub messages_5min: i64,
    /// Node event streams per org recorded by server instances (several
    /// instances only; a single instance keeps them in memory).
    pub node_streams: Vec<(i64, i64)>,
}

/// Row counts of some tables ([`crate::Store::row_counts`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RowCounts {
    pub tokens: i64,
    pub revoked_tokens: i64,
    pub web_sessions: i64,
}
