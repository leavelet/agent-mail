//! Wire types of requests for the person (M10, `/v1/requests…`): one
//! record per thing that waits for the org's user, with a kind, typed
//! options, a default, a deadline and an answer.
//!
//! Requests are numbered per org (`num`, shown as `r12`); numbers are
//! never reused. Seats ask (`question`, `plan`, `merge`), nodes report
//! what a seat cannot ask itself (`attention`); the user answers. The
//! `permission` kind is part of the record but is not created yet.

use super::{Deserialize, FromStr, Serialize, fmt};

// --- format rules (HTTP 422 when exceeded) -------------------------------------

/// Characters of a request's title (one line).
pub const MAX_REQUEST_TITLE_CHARS: usize = 512;
/// Bytes of a request's body (Markdown).
pub const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
/// Options of a request that has options.
pub const MIN_REQUEST_OPTIONS: usize = 2;
pub const MAX_REQUEST_OPTIONS: usize = 6;
/// Characters of an option's key (`A-Z`, `a-z`, `0-9`, `_`, `-`, `.`;
/// not digits only, so that a number always names an option by position).
pub const MAX_REQUEST_OPTION_KEY_CHARS: usize = 32;
/// Characters of an option's label (one line).
pub const MAX_REQUEST_OPTION_LABEL_CHARS: usize = 200;
/// Characters of an option's description.
pub const MAX_REQUEST_OPTION_DESCRIPTION_CHARS: usize = 1_000;
/// Characters of a free-text answer.
pub const MAX_REQUEST_ANSWER_CHARS: usize = 16_384;
/// Characters of the note that goes with an answer.
pub const MAX_REQUEST_NOTE_CHARS: usize = 4_000;
/// Characters of a seat's attention code and hint kept in an `attention`
/// request.
pub const MAX_REQUEST_ATTENTION_CHARS: usize = 64;
/// Longest wait of one `GET /requests/{num}/wait`, in seconds.
pub const MAX_REQUEST_WAIT_S: u64 = 600;
/// The wait when `timeout_s` is not given, in seconds.
pub const DEFAULT_REQUEST_WAIT_S: u64 = 60;
/// Characters of a permission request's tool input kept in its body.
pub const MAX_PERMISSION_INPUT_CHARS: usize = 4_000;
/// How long a node may hold a Claude Code permission hook while a person
/// may answer, in seconds: the default and the most (under the hook's
/// 180-second timeout).
pub const DEFAULT_PERMISSION_HOLD_S: u64 = 120;
pub const MAX_PERMISSION_HOLD_S: u64 = 170;
/// Bytes of a prompt sent to a seat from the web (`POST
/// /seats/{name}/prompt`).
pub const MAX_SEAT_PROMPT_BYTES: usize = 64 * 1024;
/// Requests per page of `GET /requests`.
pub const MAX_REQUEST_PAGE: i64 = 500;
/// Events shown with a request.
pub const REQUEST_SHOWN_EVENTS: i64 = 100;
/// Bytes counted against the storage quota for each request and each of
/// its events, on top of their texts.
pub const STORED_BYTES_PER_REQUEST: i64 = 256;
pub const STORED_BYTES_PER_REQUEST_EVENT: i64 = 64;

string_enum!(
    /// What a request is (see `docs/concepts.md`).
    RequestKind {
        Question => "question",
        Permission => "permission",
        Plan => "plan",
        Merge => "merge",
        Attention => "attention",
    }
);

string_enum!(
    /// How soon the person should answer: decides the delivery of the
    /// request's mail, its default deadline and its reminders.
    RequestClass { Now => "now", Soon => "soon", Later => "later" }
);

impl RequestClass {
    /// The deadline when the request gives none, from its creation.
    pub fn deadline_ms(self) -> i64 {
        match self {
            RequestClass::Now => 15 * 60_000,
            RequestClass::Soon => 4 * 3_600_000,
            RequestClass::Later => 24 * 3_600_000,
        }
    }

    /// The time between reminders of an open request after its deadline.
    pub fn remind_every_ms(self) -> i64 {
        match self {
            RequestClass::Now => 3_600_000,
            RequestClass::Soon | RequestClass::Later => 24 * 3_600_000,
        }
    }

    /// Sort key: `now` first.
    pub fn order(self) -> i64 {
        match self {
            RequestClass::Now => 0,
            RequestClass::Soon => 1,
            RequestClass::Later => 2,
        }
    }
}

string_enum!(
    /// A request's state. `open` until it is answered, its default is
    /// taken at the deadline (`expired`), the asker withdraws it (or, for
    /// `attention`, the node reports the condition gone), or a newer
    /// request replaces it (`superseded`).
    RequestState {
        Open => "open",
        Answered => "answered",
        Expired => "expired",
        Withdrawn => "withdrawn",
        Superseded => "superseded",
    }
);

string_enum!(
    /// How a request was made.
    RequestSource { Cli => "cli", Mcp => "mcp", Hook => "hook", Node => "node", Mail => "mail" }
);

string_enum!(
    /// The kind of a request event.
    RequestEventKind {
        Created => "created",
        Answered => "answered",
        Expired => "expired",
        Withdrawn => "withdrawn",
        Superseded => "superseded",
        Commented => "commented",
        Reminded => "reminded",
        Linked => "linked",
        Mailed => "mailed",
    }
);

/// One typed answer of a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestOption {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The options an `attention` request offers: actions the system can take.
pub const ATTENTION_OPTIONS: [(&str, &str); 4] = [
    ("restart", "Restart the seat"),
    ("sleep", "Put the seat to sleep"),
    ("open_terminal", "Open the seat's terminal"),
    ("dismiss", "Dismiss"),
];

/// Body of `POST /v1/requests`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRequest {
    /// Default `question`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RequestKind>,
    /// The seat the request is about (it waits for the answer). Seat
    /// tokens: themselves (may be left out); user and node tokens name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    /// The task the request belongs to (`12` or `"t12"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<serde_json::Value>,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// 2 to 6 options; may be empty with `free_text`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<RequestOption>,
    /// The answer may be free text (`question` only).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free_text: bool,
    /// The key of the option taken at the deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// Default `soon`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<RequestClass>,
    /// Seconds from now to the deadline; default from the class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_s: Option<u64>,
    /// Default `cli`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<RequestSource>,
    /// `attention` (node tokens): the seat's attention code, as presence
    /// reports carry it (`trust-prompt`, `login`, `turn-failed`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
}

/// The answer of a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestAnswer {
    /// The key of the chosen option.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    /// A free-text answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The user's name, a seat's name, or `deadline` (the default was
    /// taken).
    pub by: String,
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A request as answers show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub num: i64,
    pub kind: RequestKind,
    /// The seat the request is about (the one that waits).
    pub seat: String,
    /// The seat that made the request; absent when a node made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<i64>,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<RequestOption>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free_text: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    pub class: RequestClass,
    pub state: RequestState,
    /// When the default is taken (with a default) or the first reminder
    /// is sent (without one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<RequestAnswer>,
    /// The mail thread of the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<i64>,
    pub source: RequestSource,
    /// `attention`: the seat's attention code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// The request's revision: the sequence number of its newest event.
    pub rev: i64,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<String>,
}

/// One entry of a request's history; events are never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestEvent {
    pub seq: i64,
    pub at: String,
    pub kind: RequestEventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<i64>,
}

/// `GET /v1/requests/{num}` and the answers to writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestDetail {
    pub request: Request,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<RequestEvent>,
    /// Other threads that name the request (`[r12]` in a subject).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<i64>,
    /// `POST /requests` (node tokens, `attention`): the request was open
    /// already and nothing was created.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub existing: bool,
    /// `POST /requests/{num}/answer` with `open_terminal`: the seat's
    /// terminal link, if its node reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_url: Option<String>,
}

/// `GET /v1/requests`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestList {
    pub requests: Vec<Request>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Body of `POST /v1/requests/{num}/answer`: an option (its key, its
/// number from 1, or its label) or free text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Refused with 409 `stale` when the request is at another revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_rev: Option<i64>,
}

/// `GET /v1/requests/{num}/wait`: the request when it was no longer open,
/// or as it is when the wait ended (`done: false`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestWait {
    pub request: Request,
    pub done: bool,
}

/// A `request` event on the user's `GET /v1/events` stream: a request of
/// the org changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBusEvent {
    pub num: i64,
    /// `created`, `answered`, `expired`, `withdrawn`, `commented`,
    /// `reminded`, `linked`.
    pub change: String,
    pub state: RequestState,
    pub seat: String,
}

/// Body of `POST /v1/seats/{name}/prompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatPrompt {
    pub text: String,
}

/// Answer of `POST /v1/seats/{name}/prompt` and `/interrupt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatControlResponse {
    pub queued: bool,
    pub seat: String,
    /// Connected nodes the command was sent to.
    pub nodes: u32,
}

/// Whether `key` is a valid option key.
pub fn valid_option_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= MAX_REQUEST_OPTION_KEY_CHARS
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !key.chars().all(|c| c.is_ascii_digit())
}

/// The option `s` names: its number from 1, its key, or its label (keys
/// and labels compared without regard to ASCII case). Surrounding spaces,
/// a trailing `.` or `)` after a number, and `key — label` as the
/// creation mail lists it are accepted.
pub fn match_option<'a>(options: &'a [RequestOption], s: &str) -> Option<&'a RequestOption> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let num = s.trim_end_matches(['.', ')']);
    if let Ok(n) = num.parse::<usize>() {
        return n.checked_sub(1).and_then(|i| options.get(i));
    }
    let eq = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    if let Some(o) = options.iter().find(|o| eq(&o.key, s) || eq(&o.label, s)) {
        return Some(o);
    }
    // `1. key — label` (a line of the creation mail).
    if let Some((head, _)) = s.split_once(" — ")
        && let Some(o) = match_option(
            options,
            head.trim_start_matches(|c: char| {
                c.is_ascii_digit() || c == '.' || c == ')' || c == ' '
            }),
        )
    {
        return Some(o);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Vec<RequestOption> {
        ["yes:Ship it", "no:Hold it"]
            .iter()
            .map(|s| {
                let (k, l) = s.split_once(':').unwrap();
                RequestOption {
                    key: k.into(),
                    label: l.into(),
                    description: None,
                }
            })
            .collect()
    }

    #[test]
    fn options_by_number_key_and_label() {
        let o = opts();
        assert_eq!(match_option(&o, "1").unwrap().key, "yes");
        assert_eq!(match_option(&o, " 2. ").unwrap().key, "no");
        assert_eq!(match_option(&o, "NO").unwrap().key, "no");
        assert_eq!(match_option(&o, "ship it").unwrap().key, "yes");
        assert_eq!(match_option(&o, "2. no — Hold it").unwrap().key, "no");
        assert!(match_option(&o, "3").is_none());
        assert!(match_option(&o, "0").is_none());
        assert!(match_option(&o, "maybe").is_none());
        assert!(match_option(&o, "").is_none());
    }

    #[test]
    fn keys() {
        assert!(valid_option_key("allow_always"));
        assert!(valid_option_key("v1.2"));
        assert!(!valid_option_key("12"));
        assert!(!valid_option_key("a b"));
        assert!(!valid_option_key(&"k".repeat(33)));
    }

    #[test]
    fn classes() {
        assert_eq!(RequestClass::Now.deadline_ms(), 900_000);
        assert_eq!(
            "later".parse::<RequestClass>().unwrap(),
            RequestClass::Later
        );
        assert!(RequestClass::Now.order() < RequestClass::Later.order());
    }
}
