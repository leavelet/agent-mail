//! The node socket protocol: one JSON request per line, one JSON response
//! per line, one request per connection.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Environment variables the hook client forwards to the node. Hooks of
/// some harnesses run in a daemon, so most of these may be absent.
pub const HOOK_ENV: &[&str] = &[
    "AMAIL_SEAT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_PID",
    "CODEX_HOME",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// A harness hook fired. `input` is the hook's stdin, verbatim.
    Hook {
        harness: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
        input: String,
        /// How long the client still waits for the answer, in milliseconds.
        /// An answer the node cannot give in time is kept for the next hook.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_ms: Option<u64>,
    },
    /// `amail run`: prepare a session for `seat` and say how to start it.
    Launch {
        seat: String,
        cwd: String,
        /// The pid of `amail run`, which becomes the harness's pid on exec.
        pid: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        harness: Option<String>,
        #[serde(default)]
        extra: Vec<String>,
        /// The zellij pane and session `amail run` runs in, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mux_session: Option<String>,
    },
    /// Mail credentials for the seat that owns a harness thread (for agents
    /// whose tool commands do not inherit the launch environment).
    Credentials { codex_thread: String },
    /// Node state, for people and tests.
    Status,
    /// `amail up`: supervise these seats (replacing earlier placements of
    /// the same seats) and, with `start`, start the ones not running (only
    /// those in `only`, when given).
    Up {
        seats: Vec<SeatPlacement>,
        settings: crate::team::Settings,
        start: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only: Option<Vec<String>>,
    },
    /// `amail down`: stop the sessions of these seats (all supervised seats
    /// when empty); they stay offline until the next `up`.
    Down {
        #[serde(default)]
        seats: Vec<String>,
    },
    /// Stop supervising these seats (after `down`), e.g. for `up --prune`.
    Forget { seats: Vec<String> },
    /// Put a seat to sleep: end its process, keep its session for resuming.
    Sleep { seat: String },
    /// Start a sleeping (or stopped) seat now.
    Wake { seat: String },
    /// One row per supervised seat.
    Ps,
    /// Where a seat's pane (or a group's session) is, and how to attach.
    Locate { target: String },
}

/// Where and how a supervised seat runs on this host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatPlacement {
    pub seat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub harness: String,
    pub cwd: String,
    /// The zellij session, `amail-<org>-<group>`.
    pub mux_session: String,
    pub tab: String,
    /// When native wake-ups fail, type the nudge into the seat's pane, if
    /// that is safe (see the node).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paste_fallback: bool,
}

/// A row of `amail ps`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PsRow {
    pub seat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub harness: String,
    /// offline, starting, busy, idle, awaiting, dormant, stopping,
    /// restarting or failed.
    pub state: String,
    pub mux_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Pending (not yet shown) deliveries.
    pub pending: i64,
    /// Last hook or launch, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    /// Needs a person: a short tag (`trust-prompt`, `login`, `permission`,
    /// `failed`, `wake-failed`, `no-activity`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<String>,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default)]
    pub restarts: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookResponse {
    /// Written verbatim to the hook's stdout. Empty means "nothing".
    #[serde(default)]
    pub stdout: String,
}

/// How to start the harness: `amail run` sets the environment and execs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub unset_env: Vec<String>,
    /// Directories to put in front of `PATH` (so agents find `amail`).
    #[serde(default)]
    pub path_prepend: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub url: String,
    pub token: String,
    pub seat: String,
}

/// Error responses carry `{"error": "..."}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}
