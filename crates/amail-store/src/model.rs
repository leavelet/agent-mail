//! Store-level records. Timestamps are milliseconds since the Unix epoch.

use amail_proto::{
    DeliveryClass, DeliveryMode, LinkState, Meta, OrgVisibility, Outbound, PermissionMode,
    PostPolicy, Priority, SeatKind, TaskEventKind, TaskPriority, TaskResult, TaskState, TokenScope,
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
    /// Hours after which a `doing` or `review` task nobody changed is
    /// reminded (0: never).
    pub task_stale_hours: i64,
    /// Bytes of the org's tasks, counted against the storage quota with
    /// `stored_bytes`.
    pub task_bytes: i64,
    /// Bytes of the org's documents, counted against the storage quota
    /// with `stored_bytes` and `task_bytes`.
    pub doc_bytes: i64,
    /// Bytes of the org's requests for the person (and their events),
    /// counted against the storage quota with the others.
    pub request_bytes: i64,
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
    /// `human`: every document of the team waits for the user to activate
    /// it.
    pub doc_activate_by: Option<String>,
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
    /// `human` or `None` (see [`Team::doc_activate_by`]).
    pub doc_activate_by: Option<Option<String>>,
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

/// A participant of a thread that crosses orgs (see
/// [`crate::Store::thread_parts`]).
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
    /// The name of the org's user seat when the user wrote the message in
    /// the name of `sender_seat_id` (an agent seat).
    pub written_by: Option<String>,
    /// Refuse the message ([`crate::StoreError::StorageQuota`]) if the
    /// org's stored bytes would then exceed this.
    pub storage_quota: Option<i64>,
    /// A notice of the mail system (`POST /v1/node/notices`): charged to
    /// nobody (see [`message_bytes`]) and deleted after
    /// [`amail_proto::limits::NOTICE_RETENTION_DAYS`].
    pub notice: bool,
    /// Checked in the transaction that stores the message: when one no
    /// longer holds (a link ended or an opening was removed meanwhile), the
    /// message is refused with [`crate::StoreError::Conflict`]
    /// ([`GUARD_FAILED`]).
    pub guards: Vec<SendGuard>,
}

/// A condition a send depends on (see [`NewMessage::guards`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendGuard {
    /// The two orgs have an active link.
    Link { org_id: i64, peer_id: i64 },
    /// `seat_id` of org `org_id` is open to the org `audience` (by name):
    /// a grant on the seat, or on its team while it is a contact.
    Open {
        org_id: i64,
        seat_id: i64,
        audience: String,
    },
}

/// The message of the conflict a failed [`SendGuard`] gives.
pub const GUARD_FAILED: &str = "send guard failed";

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

/// Which of a seat's deliveries `Store::mark_read_many` marks read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadSelection {
    /// The seat's deliveries of these messages (ids of messages the seat
    /// has no delivery of are skipped).
    Ids(Vec<i64>),
    /// Every unread delivery of the seat; with `before_seq`, only those
    /// with `seq < before_seq` (as `InboxQuery::before_seq`).
    All { before_seq: Option<i64> },
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
    /// The org's user wrote this message in the name of the sender (an
    /// agent seat): the user seat's name.
    pub written_by: Option<String>,
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
    /// When the sender (or the org's user) withdrew the delivery (its mode
    /// is then `archive`).
    pub withdrawn_at: Option<i64>,
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
    /// The request the seat waits for (see
    /// [`amail_proto::Presence::waiting_for`]).
    pub waiting_for: Option<i64>,
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
            && self.waiting_for == next.waiting_for
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

// --- tasks ---------------------------------------------------------------------

/// A task as stored, with the names of its seats, team and parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRec {
    pub id: i64,
    pub org_id: i64,
    pub num: i64,
    pub title: String,
    /// Empty in listings (see [`crate::Store::tasks`]).
    pub body: String,
    /// The stored state (never `ready`).
    pub state: TaskState,
    pub priority: TaskPriority,
    pub rank: i64,
    pub reporter_id: i64,
    pub reporter: String,
    pub assignee_id: Option<i64>,
    pub assignee: Option<String>,
    pub team_id: Option<i64>,
    pub team: Option<String>,
    pub parent_id: Option<i64>,
    /// The parent's number.
    pub parent: Option<i64>,
    pub labels: Vec<String>,
    pub due: Option<i64>,
    pub waiting_on_id: Option<i64>,
    pub waiting_on: Option<String>,
    pub blocked_reason: Option<String>,
    /// The discussion thread.
    pub thread_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    /// Events written in all (the last event's seq).
    pub event_seq: i64,
    pub events_compacted: i64,
    /// Numbers of the blockers that do not satisfy their dependency (not
    /// closed, or not `done` with `ok` where the dependency needs it),
    /// lowest first.
    pub waits_on: Vec<i64>,
    /// The numbers in `waits_on` whose dependency needs an ok result.
    pub waits_on_ok: Vec<i64>,
    /// A pending request to take the task over: the seat and when.
    pub steal_requested_by_id: Option<i64>,
    pub steal_requested_by: Option<String>,
    pub steal_requested_at: Option<i64>,
    /// With child tasks: (children `done`, children not `cancelled`).
    pub progress: Option<(i64, i64)>,
    pub result: Option<TaskResult>,
    pub result_ref: Option<String>,
    pub result_note: Option<String>,
    /// A seat name (imported results may name seats that do not exist).
    pub result_by: Option<String>,
    pub result_at: Option<i64>,
    /// The id of the record the task was imported from.
    pub import_key: Option<String>,
    /// The number of the oldest open request for the person on the task
    /// (derived from `requests`, not stored on the task).
    pub blocked_on_human: Option<i64>,
}

impl TaskRec {
    /// Stored `open` and every blocker closed.
    pub fn ready(&self) -> bool {
        self.state == TaskState::Open && self.waits_on.is_empty()
    }
    /// The state answers show: `ready` for a ready task.
    pub fn shown_state(&self) -> TaskState {
        if self.ready() {
            TaskState::Ready
        } else {
            self.state
        }
    }
    /// The seat reminders about the task go to: the assignee, else the
    /// reporter.
    pub fn responsible(&self) -> (i64, &str) {
        match (self.assignee_id, &self.assignee) {
            (Some(id), Some(n)) => (id, n),
            _ => (self.reporter_id, &self.reporter),
        }
    }
}

/// A task event as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskEventRec {
    pub task_id: i64,
    pub seq: i64,
    pub at: i64,
    pub actor: Option<String>,
    pub written_by: Option<String>,
    pub kind: TaskEventKind,
    pub field: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub note: Option<String>,
    pub message_id: Option<i64>,
}

/// A link of a thread to a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskLinkRec {
    pub thread_id: i64,
    pub seat: Option<String>,
    pub created_at: i64,
}

/// [`crate::Store::task_detail`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDetailRec {
    pub task: TaskRec,
    pub blockers: Vec<TaskRec>,
    pub blocked: Vec<TaskRec>,
    pub children: Vec<TaskRec>,
    pub links: Vec<TaskLinkRec>,
    /// The newest events, oldest first.
    pub events: Vec<TaskEventRec>,
    /// Numbers of the blockers whose dependency needs an ok result.
    pub blockers_needs_ok: Vec<i64>,
}

/// A new task ([`crate::Store::create_task`]). Seat, team and parent are
/// checked to be of the org.
#[derive(Debug, Clone, Default)]
pub struct NewTaskRec {
    pub org_id: i64,
    pub reporter_id: i64,
    /// The user seat when the user acts in the name of `reporter_id`.
    pub written_by: Option<String>,
    pub title: String,
    pub body: String,
    pub priority: Option<TaskPriority>,
    pub assignee_id: Option<i64>,
    /// `None`: the assignee's team, else the reporter's.
    pub team_id: Option<i64>,
    /// The parent's number.
    pub parent: Option<i64>,
    pub labels: Vec<String>,
    pub due: Option<i64>,
    /// A thread to link.
    pub link_thread: Option<i64>,
    /// See [`crate::Store::create_task_once`].
    pub client_key: Option<String>,
    /// Refuse ([`crate::StoreError::StorageQuota`]) when the org's stored
    /// bytes and task bytes would exceed this.
    pub quota: Option<i64>,
}

/// Who changes which task ([`crate::Store::change_task`] and the other
/// task writes).
#[derive(Debug, Clone, Default)]
pub struct TaskWrite {
    pub org_id: i64,
    pub num: i64,
    /// The seat recorded as the actor; `None` for the server itself.
    pub actor_id: Option<i64>,
    pub written_by: Option<String>,
    /// See [`NewTaskRec::quota`].
    pub quota: Option<i64>,
    /// Refuse the write with [`crate::StoreError::Refused`] `stale` unless
    /// the task's revision (`event_seq`) is this.
    pub if_rev: Option<i64>,
}

/// Changes of a task's fields: a missing field is unchanged, `Some(None)`
/// clears it. Seats and teams are ids of the org's.
#[derive(Debug, Clone, Default)]
pub struct TaskChange {
    pub title: Option<String>,
    pub body: Option<String>,
    /// A stored state (not `ready`).
    pub state: Option<TaskState>,
    pub priority: Option<TaskPriority>,
    pub rank: Option<i64>,
    pub assignee_id: Option<Option<i64>>,
    pub team_id: Option<Option<i64>>,
    /// The parent's number.
    pub parent: Option<Option<i64>>,
    pub labels: Option<Vec<String>>,
    pub due: Option<Option<i64>>,
    pub waiting_on_id: Option<Option<i64>>,
    pub blocked_reason: Option<Option<String>>,
    /// `Some(Some(r))`: set the result (the task must be `done` after the
    /// change) with `result_ref` and `result_note`; `Some(None)`: withdraw
    /// it (`note` is the reason).
    pub result: Option<Option<TaskResult>>,
    pub result_ref: Option<String>,
    pub result_note: Option<String>,
    pub note: Option<String>,
    /// Close a parent although some of its children are not closed.
    pub force: bool,
}

/// What a task write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskChanged {
    pub before: TaskRec,
    /// The task afterwards.
    pub task: TaskRec,
    /// The fields whose value changed (`assignee` among them).
    pub fields: Vec<String>,
    /// Tasks that became startable by this write: their last open blocker
    /// closed (or, removing a dependency, the task itself).
    pub unblocked: Vec<TaskRec>,
    /// When the task was closed by this write: the unfinished tasks it
    /// blocks (those in `unblocked` included).
    pub dependents: Vec<TaskRec>,
    /// This write closed the last child of a parent that is not closed:
    /// the parent and how many children it has.
    pub children_done: Option<(TaskRec, i64)>,
    /// The task got the result `rejected`, or its `ok` result was
    /// withdrawn: the unfinished tasks whose dependency on it needs an ok
    /// result.
    pub needs_ok_waiting: Vec<TaskRec>,
}

/// The listing of [`crate::Store::tasks`].
#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    pub org_id: i64,
    /// Shown states (`ready` among them); empty: every state.
    pub states: Vec<TaskState>,
    /// `Some(None)`: unassigned tasks.
    pub assignee_id: Option<Option<i64>>,
    /// With `assignee_id`: also unassigned tasks of this team.
    pub pool_team_id: Option<i64>,
    pub reporter_id: Option<i64>,
    /// Tasks of the team: `team` set to it, or assignee or reporter one of
    /// its seats.
    pub team_id: Option<i64>,
    pub priority: Option<TaskPriority>,
    pub label: Option<String>,
    /// The parent's number.
    pub parent: Option<i64>,
    pub due_before: Option<i64>,
    /// `doing` or `review` and not changed since then.
    pub stale_before: Option<i64>,
    /// Only tasks with blockers that are not closed.
    pub waiting_only: bool,
    /// Substring of the title or body (case-insensitive for ASCII); looks
    /// at the newest [`amail_proto::limits::TASK_TEXT_SCAN`] tasks below
    /// `before`.
    pub text: Option<String>,
    /// Number cursor: tasks below it.
    pub before: Option<i64>,
    pub limit: i64,
    pub order: TaskOrder,
    /// `Some(None)`: tasks without a result.
    pub result: Option<Option<TaskResult>>,
    /// The task imported from the record with this id.
    pub import_key: Option<String>,
    /// Leave out tasks with an open request for the person.
    pub skip_blocked_on_human: bool,
}

/// The order of [`TaskQuery`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskOrder {
    /// Newest (highest number) first; pages continue with `before`.
    #[default]
    Newest,
    /// By shown state (`ready`, `doing`, `review`, `blocked`, `open`,
    /// closed), then priority, rank and number.
    Personal,
    /// By priority, rank and number.
    Priority,
    /// Most recently closed first.
    Closed,
}

/// A page of [`crate::Store::tasks`].
#[derive(Debug, Clone, Default)]
pub struct TaskPage {
    pub tasks: Vec<TaskRec>,
    /// Order `Newest`: more may match below this number.
    pub next_before: Option<i64>,
    /// The page is full: more tasks may match.
    pub truncated: bool,
}

/// One entry of the waiting and owed queries: a task, the seat concerned
/// and the other task (see [`crate::Store::tasks_waiting`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWaitRec {
    pub task: TaskRec,
    pub seat: Option<String>,
    pub other: Option<TaskRec>,
}

/// A steal request that expired ([`crate::Store::expire_task_steals`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StealExpiredRec {
    /// The task afterwards (no request).
    pub task: TaskRec,
    pub requester_id: i64,
    pub requester: String,
}

/// An import ([`crate::Store::import_tasks`]): records checked by the
/// caller, seats resolved.
#[derive(Debug, Clone, Default)]
pub struct TaskImportRec {
    pub org_id: i64,
    /// The importing user's seat: the actor of the changes and the
    /// reporter of records without a known one.
    pub actor_id: i64,
    pub quota: Option<i64>,
    /// Do everything, report it, and keep nothing.
    pub dry_run: bool,
    pub records: Vec<ImportTaskRec>,
}

/// One record of an import. `None` fields are not given: a new task gets
/// the default, an existing one keeps its value.
#[derive(Debug, Clone, Default)]
pub struct ImportTaskRec {
    /// The record's `id` (stored as `import_key`).
    pub key: String,
    pub title: String,
    pub body: Option<String>,
    /// A stored state (not `ready`).
    pub state: Option<TaskState>,
    pub priority: Option<TaskPriority>,
    /// `Some(None)`: unassigned.
    pub assignee_id: Option<Option<i64>>,
    /// Only for a new task; `None`: the importing user.
    pub reporter_id: Option<i64>,
    pub labels: Option<Vec<String>>,
    pub due: Option<Option<i64>>,
    /// The parent's record id.
    pub parent: Option<String>,
    /// Blockers' record ids, with `needs_ok`.
    pub deps: Vec<(String, bool)>,
    pub result: Option<ImportResultRec>,
    pub created_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub history: Vec<ImportHistoryRec>,
}

#[derive(Debug, Clone)]
pub struct ImportResultRec {
    pub result: TaskResult,
    pub result_ref: Option<String>,
    pub note: Option<String>,
    /// A name; `None`: the importing user.
    pub by: Option<String>,
    pub at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ImportHistoryRec {
    pub at: i64,
    pub by: Option<String>,
    pub text: String,
}

/// What [`crate::Store::import_tasks`] did.
#[derive(Debug, Clone, Default)]
pub struct TaskImportRes {
    /// Per record: its key, the task's number (none for a task a dry run
    /// would create) and `created`, `updated` or `same`.
    pub results: Vec<(String, Option<i64>, &'static str)>,
    pub warnings: Vec<String>,
}

/// One task of a report section ([`crate::Store::task_report`]).
#[derive(Debug, Clone)]
pub struct ReportItemRec {
    pub task: TaskRec,
    pub seat: Option<String>,
    pub waiting_for: Vec<String>,
    pub dependents: Vec<TaskRec>,
}

/// [`crate::Store::task_report`]: per section its name, count and first
/// tasks.
#[derive(Debug, Clone, Default)]
pub struct TaskReportRec {
    pub sections: Vec<(&'static str, i64, Vec<ReportItemRec>)>,
}

/// One seat's load ([`crate::Store::task_board`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeatLoadRec {
    pub seat: String,
    pub doing: i64,
    pub ready: i64,
    pub blocked: i64,
    pub waiting: i64,
    pub review: i64,
}

/// [`crate::Store::task_board`].
#[derive(Debug, Clone, Default)]
pub struct TaskBoardRec {
    /// Per shown state: its count and its first tasks.
    pub columns: Vec<(TaskState, i64, Vec<TaskRec>)>,
    pub load: Vec<SeatLoadRec>,
    pub stale: Vec<TaskRec>,
    pub overdue: Vec<TaskRec>,
    /// Unfinished tasks with unfinished blockers.
    pub chains: Vec<TaskRec>,
}

// --- documents -------------------------------------------------------------------

pub use amail_proto::{ActivateBy, DocEventKind, DocStatus};

/// The resource rules of documents that a public server configures (see
/// `amail_server::config::DocLimits`); `None`: no limit.
#[derive(Debug, Clone, Copy, Default)]
pub struct DocBounds {
    /// Documents of one org (numbers given out).
    pub docs_per_org: Option<i64>,
    /// Sections of one revision that are stored and searched.
    pub sections_per_doc: Option<usize>,
    /// Links of one revision that are recorded.
    pub links_per_rev: Option<usize>,
    /// Kinds of one org.
    pub kinds_per_org: Option<i64>,
    /// Documents the per-turn index is built from.
    pub index_scan: Option<i64>,
    /// Documents of a kind a new document is compared with.
    pub candidates_scan: Option<i64>,
}

/// A document kind of an org.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocKindRec {
    pub id: i64,
    pub org_id: i64,
    pub name: String,
    pub description: String,
    pub required_sections: Vec<String>,
    pub activate_by: ActivateBy,
    pub in_index: bool,
    pub body_cap_bytes: i64,
    pub verify_days: Option<i64>,
    pub builtin: bool,
    pub disabled: bool,
    /// Documents of the kind ([`crate::Store::doc_kinds`] only).
    pub docs: i64,
}

/// A kind to add or change ([`crate::Store::put_doc_kind`]): a missing
/// field keeps its value (a new kind: the default).
#[derive(Debug, Clone, Default)]
pub struct DocKindPut {
    pub name: String,
    pub description: Option<String>,
    pub required_sections: Option<Vec<String>>,
    pub activate_by: Option<ActivateBy>,
    pub in_index: Option<bool>,
    pub body_cap_bytes: Option<i64>,
    /// `Some(None)`: no expiry.
    pub verify_days: Option<Option<i64>>,
    pub disabled: Option<bool>,
}

/// A document as stored, with the names of its owner and team and its
/// relations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRec {
    pub id: i64,
    pub org_id: i64,
    pub num: i64,
    pub slug: Option<String>,
    pub kind: String,
    pub title: String,
    pub description: String,
    /// The current revision's body; empty in listings.
    pub body: String,
    pub status: DocStatus,
    pub owner_id: i64,
    pub owner: String,
    pub team_id: Option<i64>,
    pub team: Option<String>,
    pub labels: Vec<String>,
    pub editors: Vec<String>,
    pub pinned: bool,
    pub rev: i64,
    pub body_bytes: i64,
    pub tokens: i64,
    pub sha256: String,
    pub verified_at: Option<i64>,
    pub verified_by: Option<String>,
    pub expires_at: Option<i64>,
    pub thread_id: Option<i64>,
    pub event_seq: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Relations, as numbers.
    pub parent: Option<i64>,
    pub supplements: Option<i64>,
    pub superseded_by: Option<i64>,
    pub replaces: Vec<i64>,
    pub related: Vec<i64>,
}

impl DocRec {
    /// `expires_at` has passed.
    pub fn stale(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|e| e <= now)
    }
}

/// One revision; `body` is empty in lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocRevRec {
    pub rev: i64,
    pub title: String,
    pub description: String,
    pub body: String,
    pub bytes: i64,
    pub sha256: String,
    pub message: Option<String>,
    pub author: Option<String>,
    pub written_by: Option<String>,
    pub at: i64,
}

/// A section of the current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSectionRec {
    pub anchor: String,
    pub level: i64,
    pub heading: String,
    pub path: String,
    pub start: i64,
    pub end: i64,
    pub tokens: i64,
}

/// A link of a current revision, with what is known of its target now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLinkRec {
    /// The number of the document the link is in.
    pub src: i64,
    pub src_anchor: String,
    /// `doc`, `task`, `message`, `thread`, `seat`, `path`.
    pub kind: String,
    pub id: String,
    pub num: Option<i64>,
    pub anchor: Option<String>,
    pub line: Option<i64>,
    pub title_at_write: Option<String>,
    pub title: Option<String>,
    pub dangling: bool,
    pub stale: bool,
}

/// A document event as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocEventRec {
    pub doc_id: i64,
    pub seq: i64,
    pub at: i64,
    pub actor: Option<String>,
    pub written_by: Option<String>,
    pub kind: DocEventKind,
    pub field: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub note: Option<String>,
    pub message_id: Option<i64>,
    pub rev: Option<i64>,
}

/// A comment ([`DocDetailRec::comments`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocCommentRec {
    pub message_id: i64,
    pub by: Option<String>,
    pub at: i64,
    pub anchor: Option<String>,
    pub path: Option<String>,
    /// The current revision has no section `anchor`.
    pub removed: bool,
}

/// [`crate::Store::doc_detail`].
#[derive(Debug, Clone)]
pub struct DocDetailRec {
    pub doc: DocRec,
    pub sections: Vec<DocSectionRec>,
    /// Every document related to this one (parent, children, supersession,
    /// supplements, related), without bodies.
    pub relatives: Vec<DocRec>,
    /// Numbers of the children (`parent` is this document).
    pub children: Vec<i64>,
    pub supplemented_by: Vec<i64>,
    pub links: Vec<DocLinkRec>,
    pub linked_from: Vec<DocLinkRec>,
    /// Threads citing the document.
    pub threads: Vec<TaskLinkRec>,
    /// Tasks whose title cites the document.
    pub tasks: Vec<TaskRec>,
    pub comments: Vec<DocCommentRec>,
    /// The newest events, oldest first.
    pub events: Vec<DocEventRec>,
}

/// Who writes which document. The store checks the rights: `is_user` (the
/// org's user) may do everything; the owner may change every field; seats in
/// `editors` may revise the body, title, description, labels, slug, team
/// and parent.
#[derive(Debug, Clone, Default)]
pub struct DocWrite {
    pub org_id: i64,
    pub num: i64,
    /// The seat recorded as the actor; `None` for the server itself.
    pub actor_id: Option<i64>,
    pub is_user: bool,
    pub written_by: Option<String>,
    /// Refuse growth ([`crate::StoreError::StorageQuota`]) beyond this
    /// many bytes of mail, tasks and documents.
    pub quota: Option<i64>,
    /// Refuse with `stale` unless the document's `rev` is this.
    pub if_rev: Option<i64>,
    pub bounds: DocBounds,
}

/// A new document ([`crate::Store::create_doc`]).
#[derive(Debug, Clone, Default)]
pub struct NewDocRec {
    pub w: DocWrite,
    pub owner_id: i64,
    pub kind: String,
    pub title: String,
    pub description: String,
    pub body: String,
    pub slug: Option<String>,
    /// `None`: the owner's team.
    pub team_id: Option<i64>,
    pub labels: Vec<String>,
    pub editors: Vec<String>,
    pub parent: Option<i64>,
    pub supplements: Option<i64>,
    pub message: Option<String>,
    pub client_key: Option<String>,
    /// (path, exists) as the node found them.
    pub paths: Vec<(String, bool)>,
}

/// A write of the body or of one section.
#[derive(Debug, Clone, Default)]
pub struct DocBodyRec {
    pub body: String,
    pub section: Option<String>,
    pub message: Option<String>,
    pub paths: Vec<(String, bool)>,
}

/// Changes of a document's fields: a missing field is unchanged,
/// `Some(None)` clears it. Seats and teams are ids of the org's; documents
/// are numbers.
#[derive(Debug, Clone, Default)]
pub struct DocChange {
    pub title: Option<String>,
    pub description: Option<String>,
    pub slug: Option<Option<String>>,
    pub labels: Option<Vec<String>>,
    pub team_id: Option<Option<i64>>,
    pub parent: Option<Option<i64>>,
    pub supplements: Option<Option<i64>>,
    pub related: Option<Vec<i64>>,
    pub owner_id: Option<i64>,
    pub editors: Option<Vec<String>>,
    pub status: Option<DocStatus>,
    pub pinned: Option<bool>,
    pub expires_at: Option<Option<i64>>,
    pub message: Option<String>,
}

/// What a write did.
#[derive(Debug, Clone)]
pub struct DocChanged {
    pub before: DocRec,
    pub doc: DocRec,
    /// Fields that changed (`body`, `title`, `status`, ...).
    pub fields: Vec<String>,
    /// A new revision was written.
    pub revised: bool,
    /// The write's text equals the current revision: nothing was written.
    pub unchanged: bool,
    /// Dangling links of the current revision after the write.
    pub dangling: Vec<DocLinkRec>,
    /// Documents this write newly related this one to, with the relation
    /// (`supplements`, `supersedes`), for their owners' mail.
    pub related_to: Vec<(String, DocRec)>,
    /// `create_doc` with a `client_key` used before.
    pub existing: bool,
}

/// What `split`, `extract` or `merge` did.
#[derive(Debug, Clone, Default)]
pub struct DocMovedRec {
    /// The document the command was run on, after it.
    pub doc: Option<DocRec>,
    /// New documents (split, extract) or the target (merge).
    pub created: Vec<DocRec>,
    /// Other documents whose links were rewritten (numbers).
    pub rewritten: Vec<i64>,
    /// Tasks whose bodies were rewritten (numbers).
    pub tasks_rewritten: Vec<i64>,
    /// (`#anchor`, `d31` or `d12#anchor`).
    pub moved: Vec<(String, String)>,
}

/// [`crate::Store::docs`].
#[derive(Debug, Clone, Default)]
pub struct DocQuery {
    pub org_id: i64,
    pub kind: Option<String>,
    /// Empty: `draft` and `active`.
    pub statuses: Vec<DocStatus>,
    pub team_id: Option<i64>,
    pub owner_id: Option<i64>,
    pub label: Option<String>,
    /// Only documents past `expires_at` (at `now`).
    pub stale: bool,
    pub pinned: bool,
    /// Title, slug and description contain every term (`find`).
    pub terms: Vec<String>,
    pub before: Option<i64>,
    pub limit: i64,
    pub now: i64,
}

/// [`crate::Store::search_docs`].
#[derive(Debug, Clone, Default)]
pub struct DocSearchQuery {
    pub org_id: i64,
    pub terms: Vec<String>,
    pub kind: Option<String>,
    /// Empty: `active` only.
    pub statuses: Vec<DocStatus>,
    pub team_id: Option<i64>,
    pub label: Option<String>,
    pub owner_id: Option<i64>,
    /// Hits of this team's documents come first.
    pub caller_team: Option<i64>,
    pub limit: i64,
    pub now: i64,
}

/// One hit: the document (without body), the section and its own text.
#[derive(Debug, Clone)]
pub struct DocHitRec {
    pub doc: DocRec,
    pub anchor: String,
    pub path: String,
    pub text: String,
}

/// [`crate::Store::search_docs`].
#[derive(Debug, Clone, Default)]
pub struct DocSearchRes {
    pub hits: Vec<DocHitRec>,
    /// Every term was shorter than 3 characters: titles and descriptions
    /// were searched.
    pub titles_only: bool,
}
