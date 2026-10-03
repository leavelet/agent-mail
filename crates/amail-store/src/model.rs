//! Store-level records. Timestamps are milliseconds since the Unix epoch.

use amail_proto::{
    DeliveryClass, DeliveryMode, LinkState, Meta, PermissionMode, PostPolicy, Priority, SeatKind,
    TokenScope,
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
    pub created_at: i64,
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
    pub group: Option<String>,
    pub last_seq: i64,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
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
    pub last_seq: i64,
    pub to: i64,
    pub cc: i64,
    pub list: i64,
    pub urgent: i64,
}

/// A full-text search (`Store::search`). Every term must occur in the
/// subject or the body (case-insensitive substring).
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub terms: Vec<String>,
    pub before_id: Option<i64>,
    pub limit: i64,
    /// `None`: use the index (every term has at least three characters).
    /// `Some(n)`: scan only the `n` most recent visible messages before
    /// `before_id`.
    pub scan: Option<i64>,
}

/// Result of [`SearchQuery`]: matches newest first, with bodies, and in scan
/// mode the oldest message id that was scanned.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub messages: Vec<MessageRec>,
    pub scanned_to: Option<i64>,
    /// Scan mode: fewer than `scan` messages were left to scan.
    pub scan_exhausted: bool,
}

/// Mailbox counters of one seat.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CountRec {
    pub seat_id: i64,
    pub seat_name: String,
    pub last_seq: i64,
    pub unread: i64,
    pub pending: i64,
}
