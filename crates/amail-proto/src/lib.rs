//! Wire types for the agent-mail HTTP API (`/v1`) and address parsing.
//!
//! Timestamps are RFC 3339 strings in UTC. IDs are 64-bit integers.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

mod address;
pub use address::{Address, AddressError, valid_name};

/// Declares a string-valued enum with `as_str`, `FromStr`, `Display` and serde
/// support. The wire form and the database form are the same string.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s {
                    $($s => Ok($name::$variant),)+
                    other => Err(format!(concat!("invalid ", stringify!($name), ": {:?}"), other)),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_enum!(
    /// Kind of seat. Every org has exactly one `user` seat.
    SeatKind { Agent => "agent", User => "user" }
);

string_enum!(
    /// How a seat came to receive a message. When a seat is reached through
    /// several paths, the highest class wins (`to` > `cc` > `list`).
    DeliveryClass { To => "to", Cc => "cc", List => "list" }
);

impl DeliveryClass {
    /// Higher rank wins when the same seat is reached more than once.
    pub fn rank(self) -> u8 {
        match self {
            DeliveryClass::To => 3,
            DeliveryClass::Cc => 2,
            DeliveryClass::List => 1,
        }
    }
}

string_enum!(
    /// Per-list-member delivery mode, recorded on the delivery row.
    DeliveryMode { Immediate => "immediate", Digest => "digest", Archive => "archive" }
);

impl DeliveryMode {
    /// Higher rank wins when the same seat is reached more than once.
    pub fn rank(self) -> u8 {
        match self {
            DeliveryMode::Immediate => 3,
            DeliveryMode::Digest => 2,
            DeliveryMode::Archive => 1,
        }
    }
}

string_enum!(Priority { Normal => "normal", Urgent => "urgent" });

string_enum!(
    /// Where a message comes from, relative to the reader's org. Only `user`
    /// mail carries the authority of the org's owner.
    Source { User => "user", Agent => "agent", External => "external" }
);

string_enum!(
    /// Who may post to a list. External orgs may only post to `anyone` lists.
    PostPolicy { Anyone => "anyone", Members => "members", UserOnly => "user_only" }
);

string_enum!(TokenScope { User => "user", Seat => "seat", Node => "node" });

string_enum!(
    /// How a seat's harness is launched with respect to permission prompts.
    /// `default` keeps the harness's own prompting; `unattended` runs without
    /// prompts (Claude Code: bypass permissions; Codex: `-a never -s
    /// workspace-write`).
    PermissionMode { Default => "default", Unattended => "unattended" }
);

string_enum!(
    /// Session state of a seat, as reported by the node hosting it.
    SeatState { Offline => "offline", Busy => "busy", Idle => "idle", Awaiting => "awaiting" }
);

string_enum!(
    /// Link state as seen from one side. `requested`: we asked, they have not
    /// accepted. `invited`: they asked, we can accept. `active`: both agreed.
    LinkState { Requested => "requested", Invited => "invited", Active => "active" }
);

/// Delivery state derived from the timestamps, for display.
pub fn delivery_state(d: &Delivery) -> &'static str {
    if d.acked_at.is_some() {
        "acked"
    } else if d.read_at.is_some() {
        "read"
    } else if d.notified_at.is_some() {
        "notified"
    } else {
        "queued"
    }
}

// ---------------------------------------------------------------------------
// Errors

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    /// Set on `rate_limited` errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    /// Set when a send fails because no recipient could be resolved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
}

// ---------------------------------------------------------------------------
// Messages

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendRequest {
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_required: Option<bool>,
    /// Free-form string key/value pairs stored with the message and shown to
    /// recipients. The server does not interpret them.
    #[serde(default, skip_serializing_if = "Meta::is_empty")]
    pub meta: Meta,
}

/// Structured message fields (`meta`): a small string map.
pub type Meta = std::collections::BTreeMap<String, String>;

/// Size limits enforced on every send (HTTP 422 when exceeded).
pub mod limits {
    /// Maximum subject length in characters.
    pub const MAX_SUBJECT_CHARS: usize = 300;
    /// Maximum body size in bytes.
    pub const MAX_BODY_BYTES: usize = 256 * 1024;
    /// Maximum number of addresses (`to` plus `cc`, as written) per message.
    pub const MAX_ADDRESSES: usize = 100;
    /// Maximum number of `meta` keys.
    pub const MAX_META_KEYS: usize = 16;
    /// Maximum total size of `meta` keys and values in bytes.
    pub const MAX_META_BYTES: usize = 4096;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResponse {
    pub id: i64,
    pub thread_id: i64,
    /// One entry per accepted address, with the number of seats it expanded to.
    pub recipients: Vec<AddressResult>,
    /// Addresses that were rejected, with the reason.
    pub rejected: Vec<Rejected>,
    /// Number of distinct seats that received a delivery.
    pub deliveries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressResult {
    pub address: String,
    pub seats: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejected {
    pub address: String,
    /// Machine-readable reason: `invalid_address`, `unknown_address`,
    /// `not_linked`, `post_denied`.
    pub reason: String,
}

/// Message headers as seen by a particular reader. Addresses are relative to
/// the reader's org (`name` for local, `name@org` for other orgs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeader {
    pub id: i64,
    pub thread_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<i64>,
    pub from: String,
    pub source: Source,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: String,
    pub priority: Priority,
    pub ack_required: bool,
    pub lines: i64,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Meta::is_empty")]
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(flatten)]
    pub header: MessageHeader,
    pub body: String,
}

/// One delivery row: the state of one message in one seat's mailbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub seat: String,
    /// Position in the seat's mailbox, assigned when the delivery is created.
    pub seq: i64,
    /// Seat sequence number of the last change to this delivery (creation,
    /// notification, read, ack). Used as the `changes` cursor.
    pub mod_seq: i64,
    pub class: DeliveryClass,
    pub mode: DeliveryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified_via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxEntry {
    pub delivery: Delivery,
    pub message: MessageHeader,
    /// Present on `claim` responses when the body is short enough to inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxResponse {
    pub seat: String,
    pub entries: Vec<InboxEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResponse {
    pub message: Message,
    /// The caller's own delivery of this message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AckResponse {
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub message_id: i64,
    pub recipients: Vec<RecipientStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipientStatus {
    /// Recipient address relative to the caller's org.
    pub address: String,
    pub state: String,
    #[serde(flatten)]
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread_id: i64,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRequest {
    #[serde(default = "all_classes")]
    pub classes: Vec<DeliveryClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_priority: Option<Priority>,
    /// Free-form label of the notification path, stored as `notified_via`.
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_body_max: Option<u32>,
    #[serde(default)]
    pub include_digest: bool,
    /// Also mark read the claimed deliveries whose whole body is returned
    /// inline (the caller shows them in full).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mark_inlined_read: bool,
}

fn all_classes() -> Vec<DeliveryClass> {
    DeliveryClass::ALL.to_vec()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimResponse {
    pub seat: String,
    pub entries: Vec<InboxEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangesResponse {
    pub seat: String,
    pub entries: Vec<InboxEntry>,
    /// Cursor to pass as `since` next time.
    pub next: i64,
    /// True when more changes are available past `next`.
    pub more: bool,
}

/// Payload of a server-sent `mailbox` event: the seat's mailbox changed and its
/// sequence counter is now at least `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxEvent {
    pub seat: String,
    pub seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveResponse {
    pub messages: Vec<MessageHeader>,
    /// Pass as `before_id` to get the next (older) page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Directory and administration

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seat {
    pub name: String,
    pub kind: SeatKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    pub created_at: String,
    /// The seat's session state as last reported by a node. Absent when no
    /// node has reported the seat recently (treat as offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewSeat {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

/// PATCH body for a seat. A missing field is left unchanged; `null` clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeatPatch {
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub role: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub harness: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub model: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub permission_mode: Option<Option<PermissionMode>>,
}

fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenRequest {
    /// `node` or `user` for `POST /v1/tokens`; ignored for seat tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<TokenScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub token: String,
    pub scope: TokenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub post_policy: PostPolicy,
    pub member_count: i64,
    /// Present on `GET /lists/{name}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<ListMember>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewList {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_policy: Option<PostPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListMember {
    pub address: String,
    pub mode: DeliveryMode,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PutMember {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DeliveryMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub org: String,
    pub state: LinkState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLink {
    pub org: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgInfo {
    pub name: String,
    pub send_rate_per_min: i64,
    pub send_burst: i64,
    pub max_recipients: i64,
    pub max_thread_depth: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrgPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_rate_per_min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_burst: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_recipients: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_thread_depth: Option<i64>,
}

/// Response of `GET /v1/whoami`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhoAmI {
    pub org: String,
    pub scope: TokenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
}

// ---------------------------------------------------------------------------
// Nodes: pending counts, presence, launch

/// Pending (not yet notified, `immediate` mode) deliveries of one seat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCounts {
    pub seat: String,
    pub to: i64,
    pub cc: i64,
    pub list: i64,
    /// Pending deliveries of urgent messages, in any class.
    pub urgent: i64,
    /// The seat's sequence counter when the counts were taken.
    pub seq: i64,
}

impl PendingCounts {
    pub fn total(&self) -> i64 {
        self.to + self.cc + self.list
    }
}

/// Response of `GET /v1/pending`: every live seat of the org (or the one
/// named by `seat=`), including seats with nothing pending.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PendingResponse {
    pub seats: Vec<PendingCounts>,
}

/// A seat's session state as shown in the directory and the event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub state: SeatState,
    /// The last wake-up attempt for this seat was not answered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wake_failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Label of the reporting node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// When the state last changed.
    pub since: String,
    /// When the node last reported the seat.
    pub updated_at: String,
}

/// One entry of `POST /v1/presence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceUpdate {
    pub seat: String,
    pub state: SeatState,
    #[serde(default)]
    pub wake_failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// When the state last changed, if the node knows (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// Body of `POST /v1/presence` (node tokens).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresenceReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub seats: Vec<PresenceUpdate>,
}

/// Payload of a server-sent `presence` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceEvent {
    pub seat: String,
    #[serde(flatten)]
    pub presence: Presence,
}

/// Response of `GET /v1/seats/{name}/launch` (node tokens): what a node needs
/// to start a harness session for the seat, including a fresh seat token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchInfo {
    pub org: String,
    pub seat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// Name of the org's user seat.
    pub user_seat: String,
    /// A new seat token for the session.
    pub token: String,
}

/// A token as listed by `GET /v1/tokens` (never the secret itself).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInfo {
    pub id: i64,
    pub scope: TokenScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: String,
}
