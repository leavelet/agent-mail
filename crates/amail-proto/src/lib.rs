//! Wire types for the agent-mail HTTP API (`/v1`) and address parsing.
//!
//! Timestamps are RFC 3339 strings in UTC. IDs are 64-bit integers.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

mod address;
pub mod text;
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
    /// Kind of seat. Every org has exactly one `user` seat, and one hidden
    /// `system` seat that sends system mail (it is never in the directory,
    /// has no tokens and cannot be addressed).
    SeatKind { Agent => "agent", User => "user", System => "system" }
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
    /// mail carries the authority of the org's owner. `system` mail is
    /// written by the mail server itself (e.g. an acknowledgement that did
    /// not come); no client can send it.
    Source { User => "user", Agent => "agent", External => "external", System => "system" }
);

string_enum!(
    /// Who may post to a list. External orgs may only post to `anyone` lists.
    PostPolicy { Anyone => "anyone", Members => "members", UserOnly => "user_only" }
);

string_enum!(
    /// What a token may do. `operator` tokens belong to no org: they run a
    /// public server (stats, suspending and deleting orgs) and never see
    /// mail.
    TokenScope { User => "user", Seat => "seat", Node => "node", Operator => "operator" }
);

string_enum!(
    /// How a seat's harness is launched with respect to permission prompts.
    /// `default` keeps the harness's own prompting; `unattended` runs without
    /// prompts (Claude Code: bypass permissions; Codex: `-a never -s
    /// workspace-write`).
    PermissionMode { Default => "default", Unattended => "unattended" }
);

string_enum!(
    /// Session state of a seat, as reported by the node hosting it.
    /// `dormant`: the seat sleeps; its session can be resumed but no process
    /// runs (mail for it starts the session again).
    SeatState { Offline => "offline", Busy => "busy", Idle => "idle", Awaiting => "awaiting", Dormant => "dormant" }
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
    /// With `ack_required`: seconds until the sender is told who has not
    /// acknowledged (default: the org's `ack_timeout_min`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_timeout_s: Option<u64>,
    /// Free-form string key/value pairs stored with the message and shown to
    /// recipients. The server does not interpret them.
    #[serde(default, skip_serializing_if = "Meta::is_empty")]
    pub meta: Meta,
}

/// Structured message fields (`meta`): a small string map.
pub type Meta = std::collections::BTreeMap<String, String>;

/// The fixed bounds of the server and the node, in one place. The
/// configurable limits (rates, counts per org, the storage quota) are in the
/// server's `config::Limits`. Both are listed in `docs/security.md`.
pub mod limits {
    // --- one message (HTTP 422 when exceeded) ---

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

    // --- storage quota accounting ---

    /// Bytes counted against the storage quota for each message, on top of
    /// its subject, body, `meta` and address lists: the message's row
    /// without those texts, and its index entries. Measured on Postgres 17
    /// at about 470 bytes (SQLite: about 100).
    pub const STORED_BYTES_PER_MESSAGE: i64 = 512;
    /// Bytes counted against the storage quota for each delivery (one per
    /// recipient seat): the delivery's row and its index entries. Measured
    /// on Postgres 17 at about 360 bytes (SQLite: about 130). The
    /// migration `0013_storage_deliveries.sql` uses the same two values.
    pub const STORED_BYTES_PER_DELIVERY: i64 = 384;

    // --- other stored text, in characters (HTTP 400 when exceeded) ---

    /// Token labels.
    pub const MAX_LABEL_CHARS: usize = 200;
    /// A seat's display name.
    pub const MAX_DISPLAY_NAME_CHARS: usize = 200;
    /// A seat's role (part of its agent's system prompt).
    pub const MAX_ROLE_CHARS: usize = 16_000;
    /// A seat's model name.
    pub const MAX_MODEL_CHARS: usize = 200;
    /// A seat's harness name.
    pub const MAX_HARNESS_CHARS: usize = 64;
    /// A list's description.
    pub const MAX_DESCRIPTION_CHARS: usize = 1_000;
    /// Presence: the `attention` tag.
    pub const MAX_ATTENTION_CHARS: usize = 64;
    /// Presence: the attention hint.
    pub const MAX_HINT_CHARS: usize = 500;
    /// URLs (webhook, terminal).
    pub const MAX_URL_CHARS: usize = 2_000;
    /// A node's label.
    pub const MAX_NODE_LABEL_CHARS: usize = 200;

    // --- node notices (`POST /v1/node/notices`) ---

    /// Notice reports one org's nodes may send per hour, all together
    /// (bursts of [`NODE_NOTICE_BURST`]); further ones get 429.
    pub const NODE_NOTICES_PER_HOUR: f64 = 60.0;
    pub const NODE_NOTICE_BURST: f64 = 10.0;
    /// Notices in one report.
    pub const MAX_NODE_NOTICES: usize = 20;
    /// Setting changes or actions in one notice.
    pub const MAX_NOTICE_ITEMS: usize = 100;
    /// Seat names listed for one action (more are counted, not named).
    pub const MAX_NOTICE_TARGETS: usize = 50;
    /// Bytes of the body of one notice mail. One report becomes at most one
    /// mail; items that do not fit are left out and counted.
    pub const MAX_NOTICE_BODY_BYTES: usize = 4096;
    /// Notice mails one org may receive in 24 hours (a sliding window,
    /// counted in the database); reports beyond it are dropped.
    pub const NOTICE_MAILS_PER_DAY: i64 = 50;
    /// Bytes of notice mail (subject and body) one org may receive in 24
    /// hours; reports beyond it are dropped.
    pub const NOTICE_BYTES_PER_DAY: i64 = 128 * 1024;
    /// Days after which notice mail is deleted, independent of the server's
    /// retention setting. Notice mail is not charged to the org's storage
    /// quota; this and the daily budget bound it instead.
    pub const NOTICE_RETENTION_DAYS: i64 = 7;

    // --- responses ---

    /// Upper bound of the bytes of message headers (and snippets) in one
    /// answer of `/inbox`, `/changes`, `/archive/messages`, `/sent` and
    /// `/archive/search`; a page that would be larger is cut and continues
    /// with its cursor.
    pub const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
    /// Upper bound of the message bodies in one answer of
    /// `GET /threads/{id}`; older messages beyond it come without their
    /// body (`body_omitted`), to be fetched one at a time.
    pub const MAX_THREAD_BODY_BYTES: usize = 1024 * 1024;
    /// The most messages `GET /threads/{id}` answers (the newest ones).
    pub const MAX_THREAD_MESSAGES: i64 = 500;
    /// Messages of the org one archive or search request looks at, newest
    /// first, before it answers with what it found and a cursor
    /// (`next_before_id`) to continue.
    pub const MAX_SCAN_MESSAGES: i64 = 50_000;
    /// `GET /inbox` and `GET /sent` pages of up to this many entries are
    /// served without one of the org's query slots; larger pages, and every
    /// `GET /changes` page, take one.
    pub const SMALL_PAGE: i64 = 100;
    /// Deliveries of the user seat that `GET /attention` (and the count in
    /// `GET /overview`) looks at, newest first.
    pub const MAX_ATTENTION_SCAN: i64 = 5_000;
    /// Messages a search with a term shorter than three characters looks at.
    pub const SHORT_TERM_SCAN_MESSAGES: i64 = 2_000;
    /// The database cancels a query of the endpoints under the per-org
    /// query limit (search, archive, threads, attention, delivery status,
    /// lists) that runs longer than this; the request fails with 503
    /// `query_timeout`.
    pub const QUERY_TIMEOUT_MS: u64 = 10_000;
    /// Members of one list.
    pub const MAX_LIST_MEMBERS: i64 = 1_000;
    /// Characters of a seat's role in `GET /seats` (the list); longer roles
    /// are cut there and marked `role_truncated`. `GET /seats/{name}` has
    /// the whole role.
    pub const LIST_ROLE_CHARS: usize = 1_000;
    /// Registrations from one IPv6 /48 network, as a multiple of the
    /// per-address limit (`registrations_per_hour`, which keys IPv6 clients
    /// by /64). A request this limit refuses creates no per-/64 entry.
    pub const REGISTRATION_NET48_FACTOR: f64 = 4.0;
    /// Registrations refused with `org_exists` that one invite may get;
    /// after that many the invite is no longer valid, so that one invite
    /// cannot be used to test many org names.
    pub const MAX_INVITE_NAME_CONFLICTS: i64 = 3;
    /// Days after which a link request that was not accepted is dropped,
    /// whether or not an org of that name exists.
    pub const LINK_REQUEST_DAYS: i64 = 30;
    /// Failed authentications from one client address (IPv6: one /64)
    /// after which its requests with a credential the server has not seen
    /// valid lately are refused before the database is asked, as a
    /// multiple of the per-minute rate (`auth_failures_per_min`).
    pub const AUTH_FAILURE_BURST_MINUTES: f64 = 3.0;

    // --- what the node reads (so a server cannot exhaust its memory) ---

    /// Largest response body the node reads from the server.
    pub const NODE_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
    /// Longest line, and largest event, the node reads from the server's
    /// event stream; a longer one ends the stream (the node reconnects).
    pub const NODE_MAX_EVENT_BYTES: usize = 1024 * 1024;
    /// Largest request (one line) the node reads from its socket. The hook
    /// client forwards at most 32 MiB of hook input.
    pub const NODE_MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;
    /// Seats the node keeps state for without hosting them (their pending
    /// counts, from the server); further names from the server are ignored.
    pub const NODE_MAX_OTHER_SEATS: usize = 1_000;

    /// The most body characters `POST /claim` works with per delivery:
    /// `inline_body_max` and `preview_chars` are capped at it, and the
    /// server loads no more of a body than that for a claim.
    pub const MAX_CLAIM_BODY_CHARS: usize = 16_384;

    // --- browser sessions ---

    /// Browser sessions per token; signing in once more ends the oldest.
    pub const MAX_SESSIONS_PER_TOKEN: i64 = 20;

    // --- conversation mirror (server memory, per server instance) ---

    /// Largest body of one transcript post (`POST /seats/{name}/transcript`,
    /// HTTP 413 beyond). Nodes split their batches to fit.
    pub const MAX_TRANSCRIPT_POST_BYTES: usize = 1024 * 1024;
    /// Events in one transcript post (the newest are kept).
    pub const MAX_TRANSCRIPT_POST_EVENTS: usize = 1000;
    /// Characters of one transcript event's text (longer texts are cut).
    pub const MAX_TRANSCRIPT_TEXT_CHARS: usize = 16_000;
    /// Characters of each text field of a transcript status (longer ones
    /// are cut).
    pub const MAX_TRANSCRIPT_STATUS_CHARS: usize = 2_000;
    /// Events of a watched seat kept for watchers that join later.
    pub const MIRROR_REPLAY_EVENTS: usize = 1000;
    /// Bytes of those events (the oldest go first).
    pub const MIRROR_REPLAY_BYTES: usize = 4 * 1024 * 1024;
    /// Bytes waiting to be sent to one watcher. A watcher that falls
    /// further behind is disconnected; it reconnects and starts again from
    /// the replay events.
    pub const MIRROR_WATCHER_BYTES: usize = 8 * 1024 * 1024;
    /// Bytes of one watched seat: its replay events plus what waits to be
    /// sent to all its watchers. Beyond it, the watchers furthest behind
    /// are disconnected.
    pub const MIRROR_SEAT_BYTES: usize = 16 * 1024 * 1024;

    /// Characters of `s`, or an error naming `field` and the limit.
    pub fn check_chars(field: &str, s: &str, max: usize) -> Result<(), String> {
        if s.chars().count() > max {
            Err(format!("{field} is limited to {max} characters"))
        } else {
            Ok(())
        }
    }
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
    /// Recipients of the sender's org that will not see the message at
    /// once: asleep, offline, focused, or not answering wake-ups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<RecipientNote>,
}

/// Why a recipient will not see a message at once (see
/// [`SendResponse::notes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipientNote {
    pub seat: String,
    /// `dormant` (asleep; `to` and urgent mail start it), `offline` (no
    /// session), `focus` (only urgent mail until `until`), `wake_failed`
    /// (its session does not answer wake-ups).
    pub state: String,
    /// Mail of this class and priority starts or wakes the seat now.
    pub wakes: bool,
    /// `focus`: when it ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressResult {
    pub address: String,
    pub seats: u32,
    /// For a list in `To`: how many of its members are notified at once,
    /// which wakes them when they are idle (members with `digest` or
    /// `archive` mode are not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wakes: Option<u32>,
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
    /// The retention period cleared the body (the header is kept).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(flatten)]
    pub header: MessageHeader,
    pub body: String,
    /// `GET /threads/{id}`: the body was left out to keep the answer small
    /// (`body` is empty); fetch it with `GET /messages/{id}`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub body_omitted: bool,
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
    /// On `claim` responses with `preview_chars`: the body's first non-empty
    /// line, cut to that many characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxResponse {
    pub seat: String,
    pub entries: Vec<InboxEntry>,
    /// The page was cut to stay within [`limits::MAX_PAGE_BYTES`]: more
    /// entries follow. Continue after the last entry (`after_seq`), or,
    /// when paging with `before_seq`, before the first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
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
    /// Set for the seats another org's list reached: they are counted
    /// together under the list's address (`address`), without their names.
    /// `state` and the times then describe the slowest of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<MemberCounts>,
}

/// How many of the seats behind another org's list reached each state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberCounts {
    pub seats: i64,
    pub notified: i64,
    pub read: i64,
    pub acked: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadResponse {
    pub thread_id: i64,
    pub messages: Vec<Message>,
    /// Older messages were left out (a thread answers at most its newest
    /// 500 messages, and headers of at most [`limits::MAX_PAGE_BYTES`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
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
    /// Claim only `digest` deliveries (of the given classes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub digest_only: bool,
    /// Idempotency key chosen by the caller: a retried claim with the same
    /// key (for the same seat, within a few minutes) returns the deliveries
    /// the first one took, instead of claiming again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Fill `preview` of each returned entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_chars: Option<u32>,
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
    /// `GET /seats` (the list): `role` holds only the first
    /// [`limits::LIST_ROLE_CHARS`] characters; `GET /seats/{name}` has all.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub role_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// The team group the seat belongs to (for display; set by `amail up`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub created_at: String,
    /// The seat's session state as last reported by a node. Absent when no
    /// node has reported the seat recently (treat as offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
    /// The seat is in focus mode (only urgent mail reaches it) until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_until: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
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
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub group: Option<Option<String>>,
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
    /// Maintained by `amail up` (generated from the team file); lists
    /// without the mark are the user's and `amail up` leaves them alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
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
    /// Default time for recipients to acknowledge before the sender is told.
    pub ack_timeout_min: i64,
    /// Longest focus a seat may declare.
    pub focus_max_min: i64,
    /// An idle seat with digest mail is woken at most this often for it.
    pub digest_interval_min: i64,
    /// Every new message to the org's user is announced here (POST, headers
    /// only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,
    /// Bytes counted against the storage quota: each message the org sent
    /// (subject, body, meta, addresses and a fixed amount) with a fixed
    /// amount per delivery, and the same for mail of deleted orgs it keeps.
    #[serde(default)]
    pub stored_bytes: i64,
    /// The server's limit on `stored_bytes`; absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_quota_bytes: Option<i64>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_timeout_min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_max_min: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_interval_min: Option<i64>,
    /// `null` clears it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub webhook_url: Option<Option<String>>,
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
    /// Pending `digest` deliveries (not counted above), and when the oldest
    /// of them was sent.
    #[serde(default)]
    pub digest: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_since: Option<String>,
    /// The seat is in focus mode until then: only urgent mail is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_until: Option<String>,
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
    /// The org's digest interval: an idle seat whose oldest pending digest
    /// mail is older than this is woken for it (at most once per interval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_interval_ms: Option<u64>,
}

/// Body of `PUT /v1/focus`: focus for `for_s` seconds, or until `until`
/// (RFC 3339); with neither, for the org's maximum.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FocusRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

/// Response of `PUT` and `DELETE /v1/focus`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusResponse {
    pub seat: String,
    /// `None`: not in focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_until: Option<String>,
    /// The requested time was longer than the org allows and was cut.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub capped: bool,
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
    /// The seat needs a person: a short tag (`trust-prompt`, `login`,
    /// `permission`, `failed`, `wake-failed`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// What to do about `attention`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_hint: Option<String>,
    /// A browser terminal for the seat's session, if its node serves one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_url: Option<String>,
    /// Some node of the org has its event stream open, so commands (sleep,
    /// wake, transcript) can reach it. Set by the server when rendering.
    #[serde(default)]
    pub node_connected: bool,
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
    /// The seat needs a person (see [`Presence::attention`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_url: Option<String>,
}

/// Body of `POST /v1/presence` (node tokens).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresenceReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub seats: Vec<PresenceUpdate>,
}

string_enum!(
    /// Which processes of the person's OS user may use a node's socket.
    /// `open`: every such process, including the agents of the sessions
    /// the node launched (they can manage seats and obtain any seat's
    /// credentials; the node tells the person when they do). `restricted`:
    /// the node tells sessions apart from other processes by their parent
    /// processes and gives a session only its own seat's credentials and
    /// no seat management. Both are best effort; see docs/security.md.
    LocalPolicy { Open => "open", Restricted => "restricted" }
);

impl LocalPolicy {
    /// The policy of a node, and of a team file, that set none.
    pub const DEFAULT: LocalPolicy = LocalPolicy::Open;
}

string_enum!(
    /// A node setting named in a settings notice.
    NoticeSetting { LocalPolicy => "local_policy", PermissionMode => "permission_mode", GitAccess => "git_access", DshJail => "dsh_jail" }
);

string_enum!(
    /// What a process of an agent session asked the node to do (see
    /// [`NodeNotice::AgentActions`]). `hook`: it sent a hook in the name of
    /// another seat; `credentials`: it obtained a seat's credentials.
    NoticeOp { Up => "up", Down => "down", Forget => "forget", Run => "run", Wake => "wake", Sleep => "sleep", Credentials => "credentials", Hook => "hook" }
);

/// One changed setting: of the node (`seat` absent) or of one seat.
/// `old` is absent when the node never reported the setting before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    pub setting: NoticeSetting,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    pub new: String,
}

/// Requests of one kind from one session, to these seats (at most
/// [`limits::MAX_NOTICE_TARGETS`] named), `count` times in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAction {
    pub op: NoticeOp,
    #[serde(default)]
    pub targets: Vec<String>,
    pub count: u32,
}

/// An event a node reports so that the server tells the org's person, by
/// system mail written from fixed texts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeNotice {
    /// The node runs with settings other than those it reported last.
    Settings { changes: Vec<SettingChange> },
    /// A process of an agent session managed seats or obtained another
    /// seat's credentials. `seat` is the session's seat; absent for Codex,
    /// whose seats on one node share one process.
    AgentActions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seat: Option<String>,
        actions: Vec<AgentAction>,
    },
}

/// Body of `POST /v1/node/notices` (node tokens).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeNotices {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    pub notices: Vec<NodeNotice>,
}

/// Answer to `POST /v1/node/notices`: the number of mails written (one
/// report becomes at most one mail; 0 when the report was empty or the org
/// is over its daily notice budget, in which case the report is dropped).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeNoticesAccepted {
    pub sent: usize,
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

// ---------------------------------------------------------------------------
// Web interface: overview, counts, attention, search, sessions

/// Body of `POST /v1/session`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequest {
    pub token: String,
}

/// Response of `GET /v1/overview`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Overview {
    /// Agent seats.
    pub seats: i64,
    /// Agent seats per state (`busy`, `idle`, `awaiting`, `dormant`,
    /// `offline`); seats no node reports count as offline.
    pub states: std::collections::BTreeMap<String, i64>,
    /// Seats whose presence carries an `attention` tag.
    pub needs_person: i64,
    pub wake_failed: i64,
    /// Messages the org can see, created in the last hour / day.
    pub mail_last_hour: i64,
    pub mail_last_day: i64,
    /// Unread messages in the user's mailbox.
    pub user_unread: i64,
    /// Entries of `GET /v1/attention`.
    pub attention: i64,
}

/// Mailbox counters of one seat (`GET /v1/counts`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatCounts {
    pub seat: String,
    /// Deliveries not yet read (archive-mode deliveries excluded).
    pub unread: i64,
    /// Deliveries not yet shown to the seat's agent (immediate mode).
    pub pending: i64,
    /// The seat's sequence counter.
    pub seq: i64,
    /// The seat is in focus until then (changing it moves the counter, so
    /// a client that follows `mailbox` events sees it here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_until: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CountsResponse {
    pub seats: Vec<SeatCounts>,
}

string_enum!(
    /// Why an entry is in the user's "needs you" queue.
    AttentionKind { Ack => "ack", Reply => "reply", Seat => "seat" }
);

/// One entry of `GET /v1/attention`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionItem {
    pub kind: AttentionKind,
    /// When the entry started waiting.
    pub since: String,
    /// The seat concerned: the sender of a message, or the seat that needs
    /// a person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageHeader>,
    /// The user's delivery of `message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SeatState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wake_failed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttentionResponse {
    pub items: Vec<AttentionItem>,
}

/// One result of `GET /v1/archive/search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    #[serde(flatten)]
    pub header: MessageHeader,
    /// A short excerpt around the first match (plain text).
    pub snippet: String,
}

string_enum!(
    /// How a search ran. Servers now always answer `scan` (a bounded scan
    /// of the org's recent messages); `index` is kept for older servers.
    SearchMode { Index => "index", Scan => "scan" }
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResponse {
    pub messages: Vec<SearchHit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before_id: Option<i64>,
    pub mode: SearchMode,
    /// How many of the org's most recent messages one request looks at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_limit: Option<i64>,
}

/// Response of `POST /v1/seats/{name}/sleep` and `/wake`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatOpResponse {
    pub seat: String,
    pub op: String,
    /// Connected nodes the command was sent to.
    pub nodes: u32,
}

// ---------------------------------------------------------------------------
// Node commands (`command` events on `/v1/events`, node tokens only)

/// A command for the node hosting `seat`. Nodes ignore unknown ops and
/// seats they do not host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCommand {
    /// `transcript`, `sleep` or `wake`.
    pub op: String,
    pub seat: String,
    /// `transcript`: forward until this many milliseconds from now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ms: Option<u64>,
    /// `transcript`: how many recent events to send first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfill: Option<u32>,
    /// The label of the node that last reported the seat, if known; other
    /// nodes ignore the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
}

// ---------------------------------------------------------------------------
// Transcript mirror

string_enum!(TranscriptRole { User => "user", Assistant => "assistant", Tool => "tool", System => "system" });

string_enum!(
    /// `notice`: text the mail system put into the session (injected mail,
    /// wake-ups), as opposed to what a person typed.
    TranscriptKind { Text => "text", ToolCall => "tool_call", ToolResult => "tool_result", Notice => "notice" }
);

/// One entry of a seat's conversation, in a harness-independent form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    pub role: TranscriptRole,
    pub kind: TranscriptKind,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// `text` was cut.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// A tool result that reports an error.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}

string_enum!(
    /// What the mirror of a seat currently shows.
    TranscriptState {
        Waiting => "waiting",
        Streaming => "streaming",
        NodeOffline => "node_offline",
        NoSession => "no_session",
        NoFile => "no_file",
        Unknown => "unknown"
    }
);

/// Payload of a transcript `status` event (and part of a node's post).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptStatus {
    pub state: TranscriptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Lines of the file that could not be parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<u64>,
}

impl TranscriptStatus {
    pub fn new(state: TranscriptState) -> Self {
        TranscriptStatus {
            state,
            message: None,
            harness: None,
            path: None,
            session: None,
            skipped: None,
        }
    }
}

/// Body of `POST /v1/seats/{name}/transcript` (node tokens).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptPost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Drop what was sent before: a new file or session follows.
    #[serde(default)]
    pub reset: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TranscriptStatus>,
    #[serde(default)]
    pub events: Vec<TranscriptEvent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranscriptPostResponse {
    /// Browsers currently watching the seat.
    pub subscribers: u32,
}

// ---------------------------------------------------------------------------
// Public mode: registration, server information, the operator

/// Response of `GET /v1/info` (no authentication).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    /// The server runs in public mode (`amail serve --public`).
    pub public: bool,
    /// `POST /v1/register` accepts invite codes.
    pub registration: bool,
    /// Message bodies older than this many days are cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
}

/// Body of `POST /v1/register`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub invite: String,
    pub org: String,
    /// Name of the user seat (default `owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
}

/// Response of `POST /v1/register`: the user token is shown only here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub org: String,
    pub user_seat: String,
    pub token: String,
}

/// Body of `DELETE /v1/org` and of the operator's org deletion: the org's
/// name, typed again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteOrgRequest {
    pub confirm: String,
}

/// One org as the operator sees it (no mail content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgStatInfo {
    pub name: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended_at: Option<String>,
    /// Live agent seats.
    pub seats: i64,
    /// Connected node event streams.
    pub nodes: i64,
    pub messages: i64,
    pub messages_hour: i64,
    pub messages_day: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    /// Bytes of the mail the org sent that the server stores.
    #[serde(default)]
    pub stored_bytes: i64,
}

/// Response of `GET /v1/operator/stats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OperatorStatsResponse {
    pub orgs: i64,
    pub suspended: i64,
    pub seats: i64,
    pub nodes_online: i64,
    /// Messages sent in the last five minutes, all orgs.
    pub mail_last_5min: i64,
    pub mail_last_hour: i64,
    /// Busiest first.
    pub list: Vec<OrgStatInfo>,
}
