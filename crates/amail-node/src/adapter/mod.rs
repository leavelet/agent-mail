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
    /// The harness is about to compact the session's context; `trigger`
    /// is `manual` or `auto` as the harness says (empty when it says
    /// nothing).
    PreCompact {
        trigger: String,
    },
    /// The harness compacted the context; `summary` is the summary it
    /// wrote, when the hook input carries one.
    PostCompact {
        summary: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Hook {
    /// The harness's event name, echoed in the answer.
    pub name: String,
    pub event: Event,
    pub session_id: Option<String>,
    /// The session's transcript file, as the harness reports it.
    pub transcript_path: Option<String>,
    /// Further facts of the input the node records.
    pub facts: HookFacts,
}

/// Characters of a tool call's input kept for the worklog.
pub const TOOL_INPUT_CHARS: usize = 200;

/// Facts of a hook input the node records in the worklog and uses for the
/// handover note. Every field is optional: the inputs are not a stable
/// contract, and a missing or unreadable field is unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookFacts {
    /// The session's working directory.
    pub cwd: Option<String>,
    /// The model, when the input names it (Claude Code `SessionStart`,
    /// every Codex hook).
    pub model: Option<String>,
    /// `PostToolUse`: the tool's name.
    pub tool: Option<String>,
    /// `PostToolUse`: the tool's input, cut to [`TOOL_INPUT_CHARS`]
    /// characters: a shell command as it is, other input as compact JSON.
    pub tool_input: Option<String>,
    /// `SessionEnd`: why the session ended.
    pub reason: Option<String>,
}

impl HookFacts {
    /// Read the facts from a hook input (Claude Code, Codex and dsh name
    /// these fields alike).
    pub fn from_input(input: &Value) -> HookFacts {
        let s = |k: &str| {
            str_field(input, k)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let tool_input = input.get("tool_input").and_then(|v| {
            let text = match v {
                Value::Null => return None,
                Value::String(t) => t.clone(),
                Value::Object(o) => match o.get("command") {
                    Some(Value::String(c)) => c.clone(),
                    Some(Value::Array(parts)) => parts
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => v.to_string(),
                },
                other => other.to_string(),
            };
            let line = amail_proto::text::line(text.trim()).into_owned();
            Some(cut_chars(&line, TOOL_INPUT_CHARS))
        });
        HookFacts {
            cwd: s("cwd"),
            model: s("model"),
            tool: s("tool_name"),
            tool_input,
            reason: s("reason"),
        }
    }
}

/// `s` cut to `max` characters.
pub fn cut_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// The compaction events, read the same way for every harness: `PreCompact`
/// carries `trigger`; `PostCompact` the summary (`compact_summary`, or
/// `summary`).
pub fn compact_event(name: &str, input: &Value) -> Option<Event> {
    match name {
        "PreCompact" => Some(Event::PreCompact {
            trigger: str_field(input, "trigger")
                .unwrap_or_default()
                .trim()
                .to_string(),
        }),
        "PostCompact" => Some(Event::PostCompact {
            summary: str_field(input, "compact_summary")
                .or_else(|| str_field(input, "summary"))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        }),
        _ => None,
    }
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
    pub codex_sandbox: Option<codex::SandboxMode>,
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
    /// Explicit effort retained in the managed Codex seat, if any.
    pub codex_effort: Option<&'a str>,
    /// For a new session of a harness that accepts one: its id.
    pub new_session_id: Option<&'a str>,
    /// The seat had a session whose context is gone; tell the new one.
    pub context_lost: bool,
    /// `resume` is an existing session the seat adopted and that has not
    /// run as the seat yet: tell it that it is now a seat
    /// ([`adopted_prompt`]) where the harness cannot give a resumed
    /// session a new system prompt.
    pub adopted: bool,
}

/// The name under which harness configurations register `amail mcp`.
pub const MCP_SERVER: &str = "amail";

/// The first prompt of a new session that replaces a lost one.
pub const CONTEXT_LOST: &str = "[amail] This is a new session: the context of your previous \
session is gone. Read your unread mail first (amail inbox), then carry on with your role.";

/// The first prompt of an adopted session (one that existed before it
/// became a seat) on harnesses that cannot change a resumed session's
/// system prompt: who it is now, and the mail rules.
pub fn adopted_prompt(info: &LaunchInfo) -> String {
    format!(
        "[amail] This conversation now runs as a seat of an amail team: from now on you \
         work as this seat and receive mail through the `amail` command.\n\n{}\n\n{}\nRead your unread mail first (amail inbox), then carry on with your role.",
        crate::inject::seat_intro(info),
        crate::inject::MAIL_INSTRUCTIONS,
    )
}

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

    /// Prove normal completion after a history error, or at startup (None).
    /// Never infer success from silence or an idle label alone.
    async fn completed_quiet_turn(
        &self,
        _probe: &QuietTurnContext,
        _history_error: Option<&anyhow::Error>,
    ) -> Result<Option<QuietTurnProof>> {
        Ok(None)
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

/// A registered session and its last node observation, captured before I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietTurnContext {
    pub target: String,
    pub transcript: PathBuf,
    pub busy_since: i64,
    pub last_hook: i64,
    /// Startup has not observed a real hook for this surviving session. The
    /// restored mtime/since values are not timestamps of a new running turn.
    pub restoring: bool,
}

/// Normal completion plus the exact file observation supporting it. The node
/// must recheck this observation at the state-commit boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietTurnProof {
    pub turn: TurnInfo,
    pub file: QuietFileProof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuietFileProof {
    path: PathBuf,
    stamp: (u64, u64, u64, i64, i64, i64, i64),
}

impl QuietFileProof {
    pub(crate) fn new(path: PathBuf, meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            path,
            stamp: (
                meta.dev(),
                meta.ino(),
                meta.len(),
                meta.mtime(),
                meta.mtime_nsec(),
                meta.ctime(),
                meta.ctime_nsec(),
            ),
        }
    }

    /// No transcript content is read while the node lock is held. Reject a
    /// changed registered path, replacement, append, truncation or symlink.
    pub(crate) fn still_current(&self, registered: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        registered.canonicalize().ok().as_ref() == Some(&self.path)
            && std::fs::symlink_metadata(&self.path).is_ok_and(|meta| {
                meta.is_file()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && Self::new(self.path.clone(), &meta) == *self
            })
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
