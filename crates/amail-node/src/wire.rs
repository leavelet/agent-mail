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
    },
    /// Mail credentials for the seat that owns a harness thread (for agents
    /// whose tool commands do not inherit the launch environment).
    Credentials { codex_thread: String },
    /// Node state, for people and tests.
    Status,
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
