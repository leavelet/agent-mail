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

mod tasks;
pub use tasks::*;
mod docs;
pub use docs::*;
mod handover;
pub use handover::*;

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
    /// Who outside the org can read the org's card. `listed-by-name`: any
    /// signed-in user or seat that knows the exact name. `hidden`: only orgs
    /// with an active link; to everyone else the org does not exist.
    OrgVisibility { ListedByName => "listed-by-name", Hidden => "hidden" }
);

string_enum!(
    /// Which seats of the org may write to other orgs. `any-seat`: every
    /// seat. `contacts-only`: the user, and the seats that are contacts
    /// for the receiving org (open to it).
    Outbound { AnySeat => "any-seat", ContactsOnly => "contacts-only" }
);

string_enum!(
    /// What a card describes.
    CardKind { User => "user", Agent => "agent", Team => "team" }
);

string_enum!(
    /// Link state as seen from one side. `requested`: we asked, they have not
    /// accepted. `invited`: they asked, we can accept. `active`: both agreed.
    LinkState { Requested => "requested", Invited => "invited", Active => "active" }
);

/// Delivery state derived from the timestamps, for display. `withdrawn`:
/// the sender withdrew it before the recipient saw it (whatever happened
/// afterwards).
pub fn delivery_state(d: &Delivery) -> &'static str {
    if d.withdrawn_at.is_some() {
        "withdrawn"
    } else if d.acked_at.is_some() {
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
    /// acknowledged (default: the org's `ack_timeout_min`). Zero asks for
    /// the acknowledgement without scheduling any reminder (no system mail
    /// to the sender or the leads).
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

    /// Maximum subject length in characters (Unicode scalar values, so a
    /// character outside the Basic Multilingual Plane counts once).
    pub const MAX_SUBJECT_CHARS: usize = 4096;
    /// A subject shown on one line (injected header lines, the command
    /// line's text output, system mail, list rows of the web interface) is
    /// cut to this many characters, followed by `…`. The message itself,
    /// `--json` and the API keep the whole subject.
    pub const SUBJECT_SHOWN_CHARS: usize = 200;
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
    /// The description of an org, seat, team or list: one paragraph that
    /// others read to find it (see [`crate::check_description`]).
    pub const MAX_DESCRIPTION_CHARS: usize = 500;
    /// A link request's note to the other org's user.
    pub const MAX_LINK_NOTE_CHARS: usize = 500;
    /// An org's display name.
    pub const MAX_ORG_DISPLAY_NAME_CHARS: usize = 100;
    /// Grants (openings) an org may hold.
    pub const MAX_GRANTS: usize = 1_000;
    /// Teams an org may have.
    pub const MAX_TEAMS: usize = 500;
    /// Highest `new_threads_per_hour` an org may set (new threads to other
    /// orgs per agent seat per hour); the server uses at most this.
    pub const MAX_NEW_THREADS_PER_HOUR: i64 = 100;
    /// New threads to other orgs the user seat may start per hour.
    pub const USER_NEW_THREADS_PER_HOUR: i64 = 60;
    /// New threads one seat may receive from one other org per hour.
    pub const NEW_THREADS_TO_SEAT_PER_HOUR: f64 = 20.0;
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
    /// The most messages one `DELETE /threads/{id}` or
    /// `POST /archive/purge-deleted-orgs` deletes; the answer says whether
    /// more remain.
    pub const MAX_DELETE_MESSAGES: i64 = 1_000;
    /// Messages of the org one archive or search request looks at, newest
    /// first, before it answers with what it found and a cursor
    /// (`next_before_id`) to continue.
    pub const MAX_SCAN_MESSAGES: i64 = 50_000;
    /// Body bytes one step of a search looks at, divided by the number of
    /// search terms (at least one message per step).
    pub const SEARCH_STEP_BYTES: i64 = 4 * 1024 * 1024;
    /// A search or archive request starts no further step of
    /// [`MAX_SCAN_MESSAGES`] after this many milliseconds; it answers with
    /// what it found and a cursor (`next_before_id`) to continue. This
    /// bounds how long one request holds a query slot.
    pub const SCAN_TIME_LIMIT_MS: u64 = 100;
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

    // --- tasks (see `docs/concepts.md`, Tasks) ---

    /// Characters of a task's title (one line).
    pub const MAX_TASK_TITLE_CHARS: usize = 512;
    /// Bytes of a task's body.
    pub const MAX_TASK_BODY_BYTES: usize = 64 * 1024;
    /// Labels of one task.
    pub const MAX_TASK_LABELS: usize = 10;
    /// Characters of one label.
    pub const MAX_TASK_LABEL_CHARS: usize = 32;
    /// Characters of a blocked task's reason (one line).
    pub const MAX_TASK_REASON_CHARS: usize = 512;
    /// Ancestors a task may have (`parent`, its parent, ...).
    pub const MAX_TASK_PARENT_DEPTH: usize = 8;
    /// Characters of a task's `result_ref`.
    pub const MAX_TASK_RESULT_REF_CHARS: usize = 200;
    /// Characters of a task's `result_note`.
    pub const MAX_TASK_RESULT_NOTE_CHARS: usize = 2_000;
    /// Characters of a `client_key` of `POST /tasks`.
    pub const MAX_TASK_CLIENT_KEY_CHARS: usize = 64;
    /// Hours a `client_key` names the task it created.
    pub const TASK_CLIENT_KEY_HOURS: i64 = 24;
    /// Records of one `POST /tasks/import`.
    pub const MAX_TASK_IMPORT_RECORDS: usize = 5_000;
    /// Characters of a record's `id` in an import.
    pub const MAX_TASK_IMPORT_ID_CHARS: usize = 200;
    /// Bytes of the body of `POST /tasks/import` (other requests: 2 MiB).
    pub const MAX_TASK_IMPORT_BYTES: usize = 64 * 1024 * 1024;
    /// Tasks of one org that are not `done` or `cancelled`.
    pub const MAX_OPEN_TASKS: i64 = 20_000;
    /// Tasks of one org in all (numbers are never reused).
    pub const MAX_TASKS: i64 = 200_000;
    /// Largest page of `GET /tasks/{num}/events`. Every event of a task is
    /// kept; the org's storage quota bounds them.
    pub const MAX_TASK_EVENTS_PAGE: i64 = 1_000;
    /// Threads linked to one task.
    pub const MAX_TASK_LINKS: i64 = 50;
    /// Blockers of one task (`blocked_by` edges).
    pub const MAX_TASK_BLOCKERS: i64 = 50;
    /// Tasks one task blocks.
    pub const MAX_TASK_BLOCKED: i64 = 200;
    /// Longest chain of dependencies the cycle check follows; an edge that
    /// would make a longer one is refused.
    pub const MAX_TASK_DEP_DEPTH: usize = 32;
    /// Tasks one `GET /tasks?text=` request looks at (newest first) before
    /// it answers with what it found and a cursor (`next_before`).
    pub const TASK_TEXT_SCAN: i64 = 5_000;
    /// Largest page of `GET /tasks` and of the personal queries.
    pub const MAX_TASK_PAGE: i64 = 500;
    /// Tasks per column of `GET /tasks/board` (the column's `count` is
    /// complete).
    pub const BOARD_COLUMN_TASKS: i64 = 100;
    /// Events in `GET /tasks/{num}`.
    pub const TASK_SHOWN_EVENTS: i64 = 20;
    /// Highest `task_stale_hours` an org may set (0 turns stale reminders
    /// off).
    pub const MAX_TASK_STALE_HOURS: i64 = 24 * 365;
    /// Hours after which a request to take over a task (`steal`) that the
    /// assignee did not answer expires.
    pub const STEAL_REQUEST_HOURS: i64 = 24;
    /// Bytes counted against the storage quota for each task, on top of
    /// its texts (title, body, labels, reason).
    pub const STORED_BYTES_PER_TASK: i64 = 512;
    /// Bytes counted against the storage quota for each task event, on top
    /// of its texts, and for each dependency and link.
    pub const STORED_BYTES_PER_TASK_EVENT: i64 = 128;

    // --- documents (see `docs/concepts.md`, Documents) ---
    //
    // Format and model-protection rules; they apply everywhere. The resource
    // rules of documents are configuration of public servers
    // (`amail_server::config::DocLimits`).

    /// Bytes of a document's body (a kind may set less).
    pub const MAX_DOC_BODY_BYTES: usize = 512 * 1024;
    /// Estimated tokens of a body above which writes and `show` advise
    /// splitting it.
    pub const DOC_SPLIT_ADVICE_TOKENS: i64 = 40_000;
    /// Characters of an `index` document's body above which its owner is
    /// advised to shorten it.
    pub const DOC_INDEX_BODY_CHARS: usize = 1_000;
    /// Characters of a document's title (one line).
    pub const MAX_DOC_TITLE_CHARS: usize = 512;
    /// Characters of a document's description (one line).
    pub const MAX_DOC_DESCRIPTION_CHARS: usize = 200;
    /// Characters of a slug (`[a-z0-9][a-z0-9._-]*`).
    pub const MAX_DOC_SLUG_CHARS: usize = 128;
    /// Characters of a revision message.
    pub const MAX_DOC_MESSAGE_CHARS: usize = 500;
    /// Labels of one document, and characters of one label.
    pub const MAX_DOC_LABELS: usize = 10;
    pub const MAX_DOC_LABEL_CHARS: usize = 32;
    /// Editors of one document.
    pub const MAX_DOC_EDITORS: usize = 20;
    /// Ancestors a document may have (`parent`, its parent, ...).
    pub const MAX_DOC_PARENT_DEPTH: usize = 8;
    /// Characters of a kind's name and of a required section's title.
    pub const MAX_DOC_KIND_NAME_CHARS: usize = 32;
    pub const MAX_DOC_SECTION_NAME_CHARS: usize = 200;
    /// Default budget of `GET /docs/index`: lines and characters.
    pub const DOC_INDEX_LINES: i64 = 60;
    pub const DOC_INDEX_CHARS: i64 = 3_000;
    /// Default hits of `GET /docs/search`.
    pub const DOC_SEARCH_HITS: i64 = 20;
    /// Characters of a search snippet.
    pub const DOC_SNIPPET_CHARS: usize = 200;
    /// Candidates a new document's answer lists, and the trigram similarity
    /// above which an existing document is one.
    pub const DOC_CANDIDATES: usize = 5;
    pub const DOC_CANDIDATE_SIMILARITY: f64 = 0.3;
    /// Events in `GET /docs/{num}`.
    pub const DOC_SHOWN_EVENTS: i64 = 20;
    /// Largest page of `GET /docs` and of the history's events.
    pub const MAX_DOC_PAGE: i64 = 500;
    /// Hours between two reminders about one document's dangling links.
    pub const DOC_DANGLING_REMIND_HOURS: i64 = 24;
    /// Bytes counted against the storage quota for each document, section,
    /// link, relation and event, on top of their texts.
    pub const STORED_BYTES_PER_DOC: i64 = 512;
    pub const STORED_BYTES_PER_DOC_ROW: i64 = 128;

    /// Characters of `s`, or an error naming `field` and the limit.
    pub fn check_chars(field: &str, s: &str, max: usize) -> Result<(), String> {
        if s.chars().count() > max {
            Err(format!("{field} is limited to {max} characters"))
        } else {
            Ok(())
        }
    }
}

/// Check a description (of an org, seat, team or list) or another
/// one-paragraph text: at most `max` characters, no line breaks and no
/// control characters.
pub fn check_paragraph(field: &str, s: &str, max: usize) -> Result<(), String> {
    limits::check_chars(field, s, max)?;
    if s.chars().any(|c| c.is_control() || text::is_line_break(c)) {
        return Err(format!(
            "{field} must be one paragraph without line breaks or control characters"
        ));
    }
    Ok(())
}

/// [`check_paragraph`] for a description.
pub fn check_description(s: &str) -> Result<(), String> {
    check_paragraph("description", s, limits::MAX_DESCRIPTION_CHARS)
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
    /// (its session does not answer wake-ups), `turn_failed` (its last turn
    /// ended in an error).
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
    /// The server added this address (in `cc`): a contact of one org on the
    /// message for another org on it, because none was on it (a contact
    /// already in the thread, or the sender's org's primary contact).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub added: bool,
    /// With `added`: the other org on the message this contact was added
    /// for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_for: Option<String>,
    /// With `added`: why, in a sentence (for example "added acme's contact
    /// liaison: every message between acme and globex carries one of
    /// each").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rejected {
    pub address: String,
    /// Machine-readable reason: `invalid_address`, `unknown_address` (no
    /// seat or list of that name in your org), `not_linked` (no active link
    /// with that org, or no such org), `unreachable` (the other org has not
    /// opened that seat to your org and it is not a participant of this
    /// thread, your org lets only contacts write outside, or no seat of
    /// that name exists there: these cases answer alike), `thread_closed`
    /// (one of the two orgs has no contact left on this thread),
    /// `post_denied`.
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
    /// The sender's org's user wrote this message in the name of `from`
    /// (an agent seat; `source` stays that of `from`): the user's address,
    /// relative to the reader's org.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
}

/// How the sender of a message is labelled on one line, for agents and
/// people: `agent`, `USER`, `external`, `system`, followed by who wrote it
/// when the org's user wrote it in an agent's name (`agent, written by
/// USER`; for another org's reader `external, written by owner@acme, the
/// user of acme`).
pub fn sender_label(h: &MessageHeader) -> String {
    let source = match h.source {
        Source::User => "USER",
        Source::Agent => "agent",
        Source::External => "external",
        Source::System => "system",
    };
    match &h.written_by {
        None => source.to_string(),
        Some(w) => match w.split_once('@') {
            None => format!("{source}, written by USER"),
            Some((_, org)) => format!(
                "{source}, written by {}, the user of {}",
                text::line(w),
                text::line(org)
            ),
        },
    }
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
    /// Absent (0) for seats of another org (see `GET /messages/{id}/status`).
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub seq: i64,
    /// Seat sequence number of the last change to this delivery (creation,
    /// notification, read, ack). Used as the `changes` cursor. Absent (0)
    /// for seats of another org.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
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
    /// The sender (or its org's user) withdrew this delivery before it was
    /// notified, read or acknowledged; its `mode` is then `archive`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub withdrawn_at: Option<String>,
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
    /// The org's user wrote the message in the sender's name: the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub written_by: Option<String>,
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
    /// Cards of the seats of other orgs that take part in this thread (were
    /// on a message of it that crossed orgs): they may be addressed in
    /// replies in this thread, whether or not their org opened them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<Card>,
    /// Tasks of the caller's org linked to this thread, or whose discussion
    /// thread it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskRef>,
    /// Documents of the caller's org this thread cites (`[d12]` in a
    /// subject), or whose discussion thread it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<DocRef>,
    /// Older messages were left out (a thread answers at most its newest
    /// 500 messages, and headers of at most [`limits::MAX_PAGE_BYTES`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Response of `DELETE /v1/threads/{id}` and
/// `POST /v1/archive/purge-deleted-orgs`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeleteMailResponse {
    /// Messages the org no longer has.
    pub deleted: i64,
    /// Messages of the thread that stay: mail exchanged with orgs that
    /// still exist (neither side can delete it).
    #[serde(default)]
    pub kept: i64,
    /// More can be deleted: call again.
    #[serde(default)]
    pub more: bool,
    /// Purge: pass as `before_id` to the next call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_before_id: Option<i64>,
}

/// Body of `POST /v1/claim/release?seat=` (node tokens): the harness turn
/// that showed these messages of a keyed claim failed, so their deliveries
/// go back to "not notified, not read" (unless acknowledged meanwhile) and
/// are delivered again.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseRequest {
    /// The claim's idempotency key ([`ClaimRequest::key`]).
    pub key: String,
    /// Message ids of the claim's deliveries to release.
    pub messages: Vec<i64>,
}

/// Response of `POST /v1/claim/release`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseResponse {
    pub seat: String,
    /// Message ids whose deliveries were released.
    pub released: Vec<i64>,
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
    /// `GET /sent?delivery=true`: how many recipients' deliveries of each
    /// listed message are in each state (one entry per message that has
    /// deliveries).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivery: Vec<DeliveryCounts>,
}

/// How many deliveries of one message are in each state. The states are
/// those of [`delivery_state`]; each delivery counts once, in its most
/// advanced state (a withdrawn one as `withdrawn`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryCounts {
    pub message_id: i64,
    pub queued: i64,
    pub notified: i64,
    pub read: i64,
    pub acked: i64,
    #[serde(default)]
    pub withdrawn: i64,
}

/// Answer of `POST /v1/messages/{id}/deliveries/{seat}/withdraw`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawResponse {
    pub delivery: Delivery,
    /// False when the delivery had been withdrawn before.
    pub changed: bool,
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
    /// The team the seat belongs to (at most one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// What the seat does, for others to find it (shown to other orgs on
    /// its card when the seat is open to them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The seat is a contact of its team: opening the team opens it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contact: bool,
    /// The audiences the seat is open to through grants on the seat or (as
    /// a contact) on its team: `linked` or org names. Shown in its own org
    /// only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_to: Vec<String>,
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
    /// The seat's team (created when there is none of that name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contact: bool,
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
    /// The seat's team; `null` takes it out of its team.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub group: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    /// Whether the seat is a contact of its team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<bool>,
}

fn is_zero_i64(n: &i64) -> bool {
    *n == 0
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
    /// When the link became active (state `active`), or when it was
    /// requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The note that came with the request (state `invited`: the other
    /// org's note to you, untrusted text; `requested`: yours).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Active links: your org's primary contact for that org (the seat
    /// the server adds in Cc when a seat that is not a contact writes to
    /// it); absent when your org has no contact for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

/// `PATCH /v1/links/{org}` (user): set your org's primary contact for that
/// org, or a team's (`team`). `primary: null` returns to the default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkPatch {
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub primary: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

/// A team's primary contact for one linked org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamPrimary {
    pub org: String,
    pub seat: String,
}

/// `POST /v1/messages/preview`: what sending would do, without sending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendPreview {
    pub recipients: Vec<AddressResult>,
    pub rejected: Vec<Rejected>,
    /// Distinct recipient seats.
    pub deliveries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewLink {
    pub org: String,
    /// A note for the other org's user, at most
    /// [`limits::MAX_LINK_NOTE_CHARS`] characters on one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
    /// amount per delivery.
    #[serde(default)]
    pub stored_bytes: i64,
    /// Bytes of the mail of deleted orgs the org keeps, counted the same
    /// way (the message and the org's own deliveries of it). Not counted
    /// against the quota; retention deletes that mail, and the user can.
    #[serde(default)]
    pub kept_bytes: i64,
    /// The server's limit on `stored_bytes`; absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_quota_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_visibility")]
    pub visibility: OrgVisibility,
    #[serde(default = "default_outbound")]
    pub outbound: Outbound,
    /// New threads to other orgs a seat may start per hour (the user seat
    /// is exempt).
    #[serde(default = "default_new_threads")]
    pub new_threads_per_hour: i64,
    /// Hours after which a task in `doing` or `review` that nobody changed
    /// is reminded to its assignee (once per period); 0: never.
    #[serde(default = "default_stale_hours")]
    pub task_stale_hours: i64,
    /// Bytes of the org's tasks (their texts, events, dependencies and
    /// links), counted against the storage quota with `stored_bytes`.
    #[serde(default)]
    pub task_bytes: i64,
    /// Bytes of the org's documents (every revision, the sections and
    /// links of the current one, events and relations), counted against
    /// the storage quota with `stored_bytes` and `task_bytes`.
    #[serde(default)]
    pub doc_bytes: i64,
    /// A one-time notice for the user (user tokens), for example that an
    /// upgrade closed what links used to open; cleared with
    /// `DELETE /v1/org/notice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

fn default_visibility() -> OrgVisibility {
    OrgVisibility::ListedByName
}

fn default_outbound() -> Outbound {
    Outbound::AnySeat
}

fn default_new_threads() -> i64 {
    10
}

fn default_stale_hours() -> i64 {
    24
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
    /// `null` clears it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<Option<String>>,
    /// `null` clears it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<OrgVisibility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbound: Option<Outbound>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_threads_per_hour: Option<i64>,
    /// See [`OrgInfo::task_stale_hours`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_stale_hours: Option<i64>,
}

// ---------------------------------------------------------------------------
// Cards, teams and openings

/// What another org may know of a seat or team: its name, kind and
/// description, nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    /// `name@org` (for cards of another org) or `name`.
    pub address: String,
    pub kind: CardKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// An org's card: what any signed-in user or seat that knows the name may
/// read (unless the org is hidden).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgCard {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `GET /v1/orgs/{name}`: another org as the caller's org sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerOrg {
    pub card: OrgCard,
    /// The link between the two orgs, if any (active, requested or
    /// invited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    /// Cards of the seats and teams that org opened to the caller's org
    /// (active link only).
    #[serde(default)]
    pub open_to_you: Vec<Card>,
    /// The caller's org's grants whose audience includes that org.
    #[serde(default)]
    pub you_open: Vec<Grant>,
    /// The caller's org's primary contact for that org (active link).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

/// A team: a named group of seats of one org, with an optional root seat
/// and parent team. A seat belongs to at most one team.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Member seats, by name.
    #[serde(default)]
    pub members: Vec<String>,
    /// The members that opening the team opens.
    #[serde(default)]
    pub contacts: Vec<String>,
    /// Audiences of the grants on the team (`linked` or org names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_to: Vec<String>,
    /// Created by `amail up` from a team file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
    /// The team's own primary contacts per linked org (used for senders in
    /// the team).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primaries: Vec<TeamPrimary>,
    /// `human`: every document of the team's seats waits for the user to
    /// activate it, whatever its kind says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_activate_by: Option<ActivateBy>,
    pub created_at: String,
}

/// `PUT /v1/teams/{name}`: create the team or change it. A missing field
/// is unchanged (for a new team: empty); `null` clears it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamPut {
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub root: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub parent: Option<Option<String>>,
    /// Replace the team's contacts (they must be members).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contacts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed: Option<bool>,
    /// `human`: every document of the team's seats waits for the user to
    /// activate it; `null` or `owner`: the kind decides.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub doc_activate_by: Option<Option<ActivateBy>>,
}

/// An opening: the org shows `target` (a seat or a team) to `audience`
/// and lets it start threads to the target (or, for a team, its
/// contacts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: i64,
    /// A seat or team name of the granting org.
    pub target: String,
    /// `user`, `agent` or `team`.
    pub kind: CardKind,
    /// `linked` (every org with an active link) or an org name.
    pub audience: String,
    /// Whether the grant takes effect now: for a named org, whether the
    /// link with it is active (otherwise the grant waits).
    pub active: bool,
    /// Created by `amail up` from a team file (removed by it when the file
    /// no longer has it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
    pub created_at: String,
}

/// `POST /v1/grants`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewGrant {
    pub target: String,
    /// `linked` (default) or an org name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
}

/// `POST /v1/grants/preview`: changes to try without applying them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrantPreview {
    #[serde(default)]
    pub add: Vec<NewGrant>,
    /// Grant ids to remove.
    #[serde(default)]
    pub remove: Vec<i64>,
}

/// What a change does to what one linked org sees: the cards it gains and
/// loses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SightChange {
    pub org: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loses: Vec<String>,
}

/// Answer of grant changes and their preview.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrantChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<Grant>,
    /// Per linked org whose sight changes (none: nothing visible changes).
    pub changes: Vec<SightChange>,
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

/// The last turn of a seat ended in a failure: the harness reported an
/// error (model or API error), or the session process ended during the
/// turn. A turn interrupted by the person is not a failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFailure {
    /// What the harness reported, with control characters removed and at
    /// most [`limits::MAX_HINT_CHARS`] characters.
    pub reason: String,
    /// When the last failed turn ended (RFC 3339).
    pub at: String,
    /// Failed turns in a row.
    pub count: u32,
    /// The node shows the mail of the failed turns to the seat again.
    /// `false` once it stopped after too many failures in a row; the seat
    /// then needs a person.
    pub retrying: bool,
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
    /// The seat's last turn failed (see [`TurnFailure`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_failed: Option<TurnFailure>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_failed: Option<TurnFailure>,
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
    /// The turns of `seat` failed `count` times in a row, so the node
    /// stopped showing the mail of those turns to it again. `error` is the
    /// harness's last error text (the server shows it as one line, cut to
    /// [`limits::MAX_HINT_CHARS`] characters).
    TurnFailures {
        seat: String,
        count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        harness: Option<String>,
        error: String,
    },
    /// Hooks on the node's host kept missing their budget for the server:
    /// in the last `window_min` minutes, `misses` of the `hooks` hooks that
    /// asked the server for mail got no answer in time (the mail was kept
    /// and shown by the next hook). `rtt_ms` is the slowest answer the node
    /// measured in that time; `budget_ms` the time a hook now waits.
    SlowServer {
        hooks: u32,
        misses: u32,
        rtt_ms: u64,
        budget_ms: u64,
        window_min: u32,
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

/// Payload of a server-sent `directory` event (user tokens): a seat or list
/// of the org was created, changed or deleted. Clients reload what they show
/// of the directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEvent {
    /// `seat` or `list`.
    pub kind: String,
    pub name: String,
    /// `created`, `updated` or `deleted`.
    pub change: String,
}

/// Payload of a server-sent `nodes` event (user tokens): the number of node
/// event streams open for the org changed. Sent once when the stream opens
/// and then on every change; `connected > 0` is what
/// [`Presence::node_connected`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodesEvent {
    pub connected: u32,
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
    /// Seats counted as `offline` in `states` whose session waits at a
    /// prompt for a person before it starts (attention `trust-prompt`,
    /// `login` or `permission`); the web interface shows them as starting.
    #[serde(default)]
    pub starting: i64,
    /// Seats whose presence carries an `attention` tag.
    pub needs_person: i64,
    pub wake_failed: i64,
    /// Seats whose last turn failed.
    #[serde(default)]
    pub turn_failed: i64,
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
    /// Why an entry is in the user's "needs you" queue: an acknowledgement
    /// asked, mail to answer, a seat that needs a person, or a notice from
    /// the server (read it; it asks for nothing).
    AttentionKind { Ack => "ack", Reply => "reply", Seat => "seat", Notice => "notice" }
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
    /// The seat's last turn failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_failed: Option<TurnFailure>,
    /// The beginning of `message`'s body, its lines joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
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
    /// The URL path prefix under which this request reached the server:
    /// `/` or e.g. `/amail` (`amail serve --base-path`, and the prefix a
    /// trusted reverse proxy removed, `X-Forwarded-Prefix`). Absent from
    /// servers older than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
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
    /// Bytes of the mail of deleted orgs the org keeps.
    #[serde(default)]
    pub kept_bytes: i64,
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
