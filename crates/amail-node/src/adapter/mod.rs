//! Harness adapters: everything that differs between harnesses (hook input
//! and output shapes, how a session is tied to a seat, how to wake an idle
//! session, how to launch one). The seat state machine in [`crate::node`] is
//! harness-independent and talks to harnesses only through [`Adapter`].

pub mod claude;
pub mod codex;
pub mod dsh;

use crate::wire::{GitAccess, LaunchPlan};
use amail_proto::{LaunchInfo, PermissionMode, SeatState};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Harness {
    Claude,
    Codex,
    /// DeepSeek Harness.
    Dsh,
}

impl Harness {
    pub fn parse(s: &str) -> Option<Harness> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude_code" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "dsh" | "deepseek" | "deepseek-harness" => Some(Harness::Dsh),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Dsh => "dsh",
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
    /// The turn was interrupted by the person (ends the turn without
    /// `Stop`).
    Interrupt,
    /// The turn ended in a failure the harness reported (a model or API
    /// error); `reason` is its error text.
    TurnFailed {
        reason: String,
    },
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
    pub dsh_bin: String,
    /// Environment given only to dsh hosts.
    pub dsh_env: BTreeMap<String, String>,
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
    /// The git access chosen on this machine (team file or `amail run`
    /// flag). Used by the Codex adapter's sandbox profile.
    pub git_access: GitAccess,
    /// Whether an unattended dsh seat's host runs in its bwrap jail (chosen
    /// on this machine; on by default).
    pub dsh_jail: bool,
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

    /// Whether the node acts on this hook at all (a harness process may
    /// also serve sessions that are not the seat's).
    fn accepts(&self, _env: &BTreeMap<String, String>, _hook: &Hook) -> bool {
        true
    }

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

    /// The transcript file of `hook`'s session of `seat` (default: the
    /// path the hook reports).
    fn transcript(&self, hook: &Hook, _seat: &str) -> Option<String> {
        hook.transcript_path.clone()
    }

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

    /// Whether the node must ask the harness how a turn ended
    /// ([`Adapter::last_turn`]) because its hooks do not report failed
    /// turns.
    fn checks_turns(&self) -> bool {
        false
    }

    /// How the latest turns of the session behind wake target `target`
    /// ended, newest first (only for adapters whose
    /// [`Adapter::checks_turns`] is true).
    async fn last_turns(&self, _target: &str) -> Result<Vec<TurnInfo>> {
        Ok(Vec::new())
    }

    /// Whether a turn can end without any hook in a way the node must find
    /// itself (Claude Code fires no hook when a person interrupts a turn):
    /// the node then looks at the end of the session's transcript
    /// ([`Adapter::transcript_turn`]) and, for supervised seats, at the pane
    /// ([`Adapter::screen_running`]) when a busy seat sends no hooks.
    fn finds_interrupts(&self) -> bool {
        false
    }

    /// What the end of the session's transcript (`tail`: whole JSONL lines)
    /// says about its current turn.
    fn transcript_turn(&self, _tail: &str) -> TranscriptTurn {
        TranscriptTurn::Unknown
    }

    /// Whether a pane screen shows a turn running (`None`: cannot tell).
    fn screen_running(&self, _screen: &str) -> Option<bool> {
        None
    }
}

/// See [`Adapter::transcript_turn`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptTurn {
    /// The person interrupted the turn; the record's time (Unix ms), if the
    /// transcript gives one.
    Interrupted(Option<i64>),
    /// The turn may be running.
    Running,
    /// Nothing to go by.
    Unknown,
}

/// How a turn ended, as the harness reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnStatus {
    InProgress,
    Completed,
    /// Interrupted by the person.
    Interrupted,
    Failed(String),
}

/// One turn of a session (see [`Adapter::last_turns`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnInfo {
    pub status: TurnStatus,
    /// Unix seconds, if known.
    pub started_at: Option<i64>,
    /// The harness's turn id, if known.
    pub id: Option<String>,
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
