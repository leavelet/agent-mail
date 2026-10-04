//! Harness adapters: everything that differs between harnesses (hook input
//! and output shapes, how a session is tied to a seat, how to wake an idle
//! session, how to launch one). The seat state machine in [`crate::node`] is
//! harness-independent and talks to harnesses only through [`Adapter`].

pub mod claude;
pub mod codex;

use crate::wire::LaunchPlan;
use amail_proto::{LaunchInfo, PermissionMode, SeatState};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn parse(s: &str) -> Option<Harness> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude_code" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
        }
    }
}

/// A hook event, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    SessionStart {
        source: String,
    },
    UserPromptSubmit {
        prompt: String,
    },
    PostToolUse,
    Stop,
    /// The turn was interrupted (ends the turn without `Stop`).
    Interrupt,
    /// The session waits for a permission decision.
    Awaiting,
    SessionEnd,
}

#[derive(Debug, Clone)]
pub struct Hook {
    /// The harness's event name, echoed in the answer.
    pub name: String,
    pub event: Event,
    pub session_id: Option<String>,
    /// The session's transcript file, as the harness reports it.
    pub transcript_path: Option<String>,
}

/// What a hook should answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    None,
    /// Add text to the model's context.
    Context(String),
    /// Refuse to end the turn; the text is shown to the model.
    Block(String),
}

/// Facts about the session a hook came from.
#[derive(Debug, Clone, Default)]
pub struct SessionHint {
    pub pid: Option<u32>,
    /// Adapter-specific address used by [`Adapter::wake`].
    pub wake_target: Option<String>,
}

/// Node settings adapters need.
#[derive(Debug, Clone)]
pub struct AdapterEnv {
    pub state_dir: PathBuf,
    /// Absolute path of the `amail` binary (for hook commands).
    pub amail_exe: PathBuf,
    pub node_socket: PathBuf,
    pub server_url: String,
    pub claude_bin: String,
    pub codex_bin: String,
    /// The `auth.json` the dedicated Codex home links to.
    pub codex_auth: PathBuf,
    /// The node's Codex pass ([`crate::wire::CODEX_PASS_ENV`]).
    pub codex_pass: String,
}

impl AdapterEnv {
    /// The directory of the `amail` binary, which agents must find on `PATH`.
    pub fn amail_dir(&self) -> Option<String> {
        self.amail_exe
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
    }

    /// `PATH` with the `amail` directory in front.
    pub fn path_with_amail(&self) -> String {
        let cur = std::env::var("PATH").unwrap_or_default();
        match self.amail_dir() {
            Some(d) if !cur.split(':').any(|p| p == d) => format!("{d}:{cur}"),
            _ => cur,
        }
    }
}

/// Inputs of [`Adapter::launch`].
pub struct LaunchCtx<'a> {
    /// What the server says about the seat. Its `permission_mode` is not
    /// used; see `permission_mode` below.
    pub info: &'a LaunchInfo,
    /// The permission mode chosen on this machine (team file or `amail run`
    /// flag). Only `Unattended` turns off the harness's permission prompts.
    pub permission_mode: PermissionMode,
    pub cwd: &'a str,
    pub extra: &'a [String],
    /// One-time registration nonce, for adapters that register sessions.
    pub nonce: &'a str,
    /// Resume this harness session instead of starting a new one.
    pub resume: Option<&'a str>,
    /// For a new session of a harness that accepts one: its id.
    pub new_session_id: Option<&'a str>,
    /// The seat had a session whose context is gone; tell the new one.
    pub context_lost: bool,
}

/// The first prompt of a new session that replaces a lost one.
pub const CONTEXT_LOST: &str = "[amail] This is a new session: the context of your previous \
session is gone. Read your unread mail first (amail inbox), then carry on with your role.";

#[async_trait::async_trait]
pub trait Adapter: Send + Sync + 'static {
    fn harness(&self) -> Harness;

    /// Normalize a hook input. `None`: not an event the node acts on.
    fn parse(&self, input: &Value) -> Option<Hook>;

    /// The seat, if the hook environment names it.
    fn seat_from_env(&self, env: &BTreeMap<String, String>) -> Option<String>;

    /// Whether sessions are tied to seats by a registration marker in their
    /// first prompt (see [`Adapter::registration_marker`]).
    fn registers_sessions(&self) -> bool {
        false
    }

    /// `(seat, nonce)` from a session-registration marker in a prompt, for
    /// harnesses whose hooks cannot see the session's environment.
    fn registration_marker(&self, _prompt: &str) -> Option<(String, String)> {
        None
    }

    fn session_hint(&self, env: &BTreeMap<String, String>, hook: &Hook) -> SessionHint;

    /// Seat state after `SessionStart` (`None`: unchanged).
    fn session_start_state(&self, source: &str) -> Option<SeatState>;

    /// Maximum length of injected summaries, in characters.
    fn inject_budget(&self) -> usize;

    /// How many deliveries one hook claims (and shows in full) at most, so
    /// the injected text stays within the harness's limit.
    fn claim_limit(&self) -> u32;

    /// Whether a persisted wake target still exists (checked on restart).
    fn target_alive(&self, _target: &str) -> bool {
        true
    }

    /// Render the hook's answer (stdout).
    fn render(&self, hook: &Hook, out: &Output) -> String {
        match out {
            Output::None => String::new(),
            Output::Context(text) => json!({
                "hookSpecificOutput": {"hookEventName": hook.name, "additionalContext": text}
            })
            .to_string(),
            Output::Block(text) => json!({"decision": "block", "reason": text}).to_string(),
        }
    }

    /// Nudge an idle session so it starts a turn (its `UserPromptSubmit`
    /// hook then claims and shows the mail).
    async fn wake(&self, target: &str, text: &str) -> Result<()>;

    /// Write harness configuration (called at node start).
    async fn prepare(&self) -> Result<()>;

    async fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan>;

    /// A session was tied to `seat` through a registration marker.
    async fn registered(&self, _session_id: &str, _seat: &str) {}

    /// No supervised session of this harness is left: stop helpers the
    /// adapter started (called after `amail down`).
    async fn shutdown(&self) {}

    /// Whether a resumed session reports itself only once a turn runs (its
    /// wake target is the session id, known before any hook).
    fn resume_is_lazy(&self) -> bool {
        false
    }
}

pub fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Quote a word for a POSIX shell command line if needed.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-+=:,@%".contains(&b))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
