//! Antigravity CLI (`agy`, checked with 1.5.11).
//!
//! - Launch: one `agy` process per seat, in the seat's pane, with
//!   `AMAIL_SEAT`, `AMAIL_URL`, `AMAIL_TOKEN` and the node socket in its
//!   environment (as for Claude Code). agy has no system-prompt flag: the
//!   seat intro and the mail rules go in the first prompt (`-i`), which also
//!   tells the agent to run `amail inbox` (see "Learning the address"). The
//!   working directory is trusted for this session only (`--settings
//!   '{"trustedWorkspaces": [...]}'`), so no trust prompt appears and the
//!   user's settings are not changed. Resume: `--conversation <id>`.
//! - Configuration: a workspace plugin `<cwd>/.agents/plugins/amail/`
//!   (`plugin.json`, `hooks.json`, `rules/AGENTS.md` with the mail rules,
//!   and a `.gitignore`), written at launch. Seats that share a directory
//!   share the plugin; hooks tell the seats apart by `AMAIL_SEAT`. No MCP
//!   server is registered in this version: the node learns how to reach a
//!   session only from `amail` shell commands.
//! - Hooks run as children of the `agy` process and see its environment.
//!   `SessionStart` (new conversations only); `PreInvocation` before every
//!   model call: number 0 starts a turn (as `UserPromptSubmit`), later ones
//!   are model calls inside the turn (as a `PostToolUse` without a tool),
//!   answered with `{"injectSteps": [{"ephemeralMessage": ...}]}`;
//!   `PostToolUse` gives the worklog facts but cannot inject; `Stop` ends
//!   the turn, also with `fullyIdle: false` (a command moved to the
//!   background: agy fires no hook if it fails or is killed, and starts a
//!   short turn when it completes), and is held with `{"decision":
//!   "continue"}`.
//! - Learning the address: the in-process language server's address and
//!   CSRF token are only in the environment of the agent's own shell
//!   commands (`ANTIGRAVITY_LS_ADDRESS`, `ANTIGRAVITY_CSRF_TOKEN`), not in
//!   the hooks'. Every `amail` command run in the session reports them to
//!   the node ([`crate::wire::Request::SessionEnv`]), which keeps them as
//!   the session's wake target ([`Target`]). A new process (a resume) has
//!   new values, learned at its first `amail` command.
//! - Wake-up and prompts: `SendUserCascadeMessage` with delivery
//!   `WHEN_IDLE` (an idle conversation starts a turn at once; a busy one
//!   runs it as its own turn after the current one). Interrupt:
//!   `CancelCascadeInvocation` with `killBackgroundTasks`, when
//!   `GetAllCascadeTrajectories` says the conversation is running or
//!   `ListBackgroundTasks` lists a task of the conversation.
//!
//! Not done in this version: a turn the person cancels in the TUI fires no
//! hook, and the node does not ask the language server about it
//! (`checks_turns`, `finds_interrupts` are off); agy has no session-end or
//! compaction events and no distinct awaiting-permission state.

use super::*;
use amail_proto::PermissionMode;
use anyhow::{Context, bail};
use std::path::Path;
use std::time::Duration;

/// Hook events the plugin registers.
pub const EVENTS: &[&str] = &["SessionStart", "PreInvocation", "PostToolUse", "Stop"];

/// The language server's address (`localhost:<port>`), in the environment
/// of the agent's shell commands.
pub const ADDRESS_ENV: &str = "ANTIGRAVITY_LS_ADDRESS";
/// The language server's CSRF token, likewise.
pub const CSRF_ENV: &str = "ANTIGRAVITY_CSRF_TOKEN";
/// The conversation the command runs in, likewise (hooks have it too).
pub const CONVERSATION_ENV: &str = "ANTIGRAVITY_CONVERSATION_ID";

/// The variables an `amail` command inside a session reports to the node.
pub const SESSION_ENV: &[&str] = &[ADDRESS_ENV, CSRF_ENV, CONVERSATION_ENV];

/// Environment of the launching shell that must not leak into a new
/// session (an `amail run` started from inside another agy session).
const UNSET: &[&str] = &[
    "AMAIL_PROFILE",
    crate::wire::CODEX_PASS_ENV,
    ADDRESS_ENV,
    CSRF_ENV,
    CONVERSATION_ENV,
    "ANTIGRAVITY_TRAJECTORY_ID",
    "ANTIGRAVITY_APP_DATA_DIR",
    "ANTIGRAVITY_AGENTAPI_EXE",
    "ANTIGRAVITY_LS_VERSION",
];

/// The plugin's directory, relative to the seat's working directory.
pub const PLUGIN_DIR: &str = ".agents/plugins/amail";

/// Seconds agy gives a hook command.
const HOOK_TIMEOUT_S: u64 = 5;

/// Timeout of one language server call.
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// The language server's RPC path prefix.
const SERVICE: &str = "exa.language_server_pb.LanguageServerService";

/// The first prompt of a resumed process (not adopted, context kept): the
/// node learns the new process's address at its first `amail` command.
pub const RESUMED: &str = "[amail] This session was resumed. Run `amail inbox` in the shell, \
then carry on with your role.";

/// The language server directory of agy's state under a home directory
/// (`<home>/.gemini/antigravity-cli/brain`): one directory per
/// conversation, with the transcript under `.system_generated/logs/`.
pub fn brain_dir(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli").join("brain")
}

/// Where a session's language server is: the wake target the node keeps
/// (as compact JSON, see [`Target::to_target`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Target {
    /// `localhost:<port>` (or `127.0.0.1:<port>`, `[::1]:<port>`).
    pub address: String,
    /// The CSRF token (`x-codeium-csrf-token`).
    pub token: String,
    /// The conversation id (`cascadeId`).
    pub conversation: String,
}

impl Target {
    pub fn to_target(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_target(s: &str) -> Option<Target> {
        let t: Target = serde_json::from_str(s).ok()?;
        t.valid().then_some(t)
    }

    /// The target the environment of an `amail` command names, if it names
    /// a complete and plausible one.
    pub fn from_env(env: &BTreeMap<String, String>) -> Option<Target> {
        let get = |k: &str| env.get(k).map(|v| v.trim().to_string());
        let t = Target {
            address: get(ADDRESS_ENV)?,
            token: get(CSRF_ENV)?,
            conversation: get(CONVERSATION_ENV)?,
        };
        t.valid().then_some(t)
    }

    /// A loopback address with a port, a token that fits in a header, and a
    /// conversation id. The node sends the token and wake-up text to this
    /// address, so only the local machine is accepted.
    fn valid(&self) -> bool {
        let loopback = match self.address.rsplit_once(':') {
            Some((host, port)) => {
                matches!(host, "localhost" | "127.0.0.1" | "[::1]") && port.parse::<u16>().is_ok()
            }
            None => false,
        };
        let token_ok = !self.token.is_empty()
            && self.token.len() <= 256
            && self.token.bytes().all(|b| b.is_ascii_graphic());
        loopback && token_ok && crate::team::check_session_id(&self.conversation).is_ok()
    }
}

pub struct Agy {
    env: AdapterEnv,
    http: reqwest::Client,
}

impl Agy {
    pub fn new(env: AdapterEnv) -> Self {
        let http = reqwest::Client::builder()
            // The server is on this machine: never through a proxy.
            .no_proxy()
            .timeout(RPC_TIMEOUT)
            .build()
            .unwrap_or_default();
        Agy { env, http }
    }

    pub fn hook_command(&self) -> String {
        format!(
            "{} hook agy",
            shell_quote(&self.env.amail_exe.to_string_lossy())
        )
    }

    /// The plugin's `hooks.json`: one named hook with every event of
    /// [`EVENTS`]. Tool events take a matcher, the others a plain list.
    pub fn hooks(&self) -> Value {
        let action = json!({
            "type": "command",
            "command": self.hook_command(),
            "timeout": HOOK_TIMEOUT_S,
        });
        let events: serde_json::Map<String, Value> = EVENTS
            .iter()
            .map(|e| {
                let list = if *e == "PostToolUse" {
                    json!([{"matcher": "*", "hooks": [action.clone()]}])
                } else {
                    json!([action.clone()])
                };
                (e.to_string(), list)
            })
            .collect();
        json!({ "amail": events })
    }

    /// The plugin's files: `(path in the plugin directory, content)`.
    pub fn plugin_files(&self) -> Vec<(&'static str, String)> {
        let pretty =
            |v: &Value| format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default());
        vec![
            (
                "plugin.json",
                pretty(&json!({"name": "amail", "description": "amail mail hooks and rules"})),
            ),
            ("hooks.json", pretty(&self.hooks())),
            (
                "rules/AGENTS.md",
                crate::inject::MAIL_INSTRUCTIONS.to_string(),
            ),
            // Keeps the plugin out of the project's commits.
            (".gitignore", "*\n".to_string()),
        ]
    }

    /// Write the plugin into `cwd`; files whose content is current are not
    /// written again. Refuses a symbolic link on the way.
    pub fn write_plugin(&self, cwd: &Path) -> Result<()> {
        let dir = cwd.join(PLUGIN_DIR);
        let files = self.plugin_files();
        let link =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
        let dirs = [
            cwd.join(".agents"),
            cwd.join(".agents/plugins"),
            dir.clone(),
            dir.join("rules"),
        ];
        if let Some(p) = dirs
            .iter()
            .cloned()
            .chain(files.iter().map(|(name, _)| dir.join(name)))
            .find(|p| link(p))
        {
            bail!("{} is a symbolic link; check it and remove it", p.display());
        }
        std::fs::create_dir_all(dir.join("rules"))
            .with_context(|| format!("creating {}", dir.display()))?;
        for (name, text) in files {
            let file = dir.join(name);
            if std::fs::read(&file).ok().as_deref() != Some(text.as_bytes()) {
                crate::persist::write_atomic(&file, text.as_bytes(), 0o600)?;
            }
        }
        Ok(())
    }

    /// The first prompt of a session started for `ctx`.
    pub fn first_prompt(ctx: &LaunchCtx<'_>) -> String {
        let info = ctx.info;
        if ctx.resume.is_some() {
            if ctx.adopted {
                return adopted_prompt(info);
            }
            return RESUMED.to_string();
        }
        let end = if ctx.context_lost {
            CONTEXT_LOST
        } else {
            "Run `amail inbox` in the shell now, then carry on with your role."
        };
        format!(
            "[amail] This is the start of your session as a seat of an amail team.\n\n{}\n# Your seat\n\n{}\n\n{end}",
            crate::inject::MAIL_INSTRUCTIONS,
            crate::inject::seat_intro(info)
        )
    }

    /// Call `method` of the language server behind `t`.
    async fn rpc(&self, t: &Target, method: &str, body: Value) -> Result<Value> {
        let url = format!("http://{}/{SERVICE}/{method}", t.address);
        let resp = self
            .http
            .post(&url)
            .header("x-codeium-csrf-token", &t.token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("agy {method}: connecting to {}", t.address))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            let text = amail_proto::text::line(text.trim()).into_owned();
            bail!("agy {method}: HTTP {status}: {}", cut_chars(&text, 200));
        }
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text).with_context(|| format!("agy {method}: not JSON"))
    }

    fn target(target: &str) -> Result<Target> {
        Target::from_target(target).with_context(|| format!("not an agy wake target: {target:?}"))
    }
}

/// Facts of a `PostToolUse` input: `toolCall.name` and `toolCall.args`
/// (the shell tool `run_command` names its command `CommandLine`), read as
/// [`HookFacts::from_input`] reads `tool_name` and `tool_input`.
fn tool_facts(input: &Value) -> HookFacts {
    let call = input.get("toolCall").unwrap_or(&Value::Null);
    let args = call.get("args").cloned().unwrap_or(Value::Null);
    let tool_input = match str_field(&args, "CommandLine") {
        Some(c) => json!({"command": c}),
        None => args,
    };
    HookFacts::from_input(&json!({
        "tool_name": call.get("name"),
        "tool_input": tool_input,
    }))
}

#[async_trait::async_trait]
impl Adapter for Agy {
    fn harness(&self) -> Harness {
        Harness::Agy
    }

    fn parse(&self, input: &Value) -> Option<Hook> {
        let name = str_field(input, "hookEventName")?.to_string();
        let mut facts = HookFacts::default();
        let event = match name.as_str() {
            "SessionStart" => Event::SessionStart {
                source: "startup".into(),
            },
            "PreInvocation" => match input.get("invocationNum").and_then(Value::as_u64) {
                Some(0) | None => Event::UserPromptSubmit {
                    prompt: crate::transcript::agy::user_request(
                        str_field(input, "lastUserInput").unwrap_or_default(),
                    ),
                },
                // A model call inside the turn: after a tool, or after a
                // held `Stop`.
                Some(_) => Event::PostToolUse,
            },
            "PostToolUse" => {
                facts = tool_facts(input);
                Event::PostToolUse
            }
            // Also with `fullyIdle: false` (a command runs in the
            // background): agy fires no hook when such a command fails or is
            // killed, so the turn ends here. A command that completes starts
            // a short turn of its own (`PostToolUse`, `PreInvocation` 0,
            // `Stop`).
            "Stop" => Event::Stop,
            _ => return None,
        };
        let s = |k: &str| {
            str_field(input, k)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        facts.cwd = s("cwd");
        facts.model = s("modelName");
        Some(Hook {
            name,
            event,
            session_id: s("conversationId"),
            transcript_path: s("transcriptPath"),
            facts,
        })
    }

    fn seat_from_env(&self, env: &BTreeMap<String, String>) -> Option<String> {
        env.get("AMAIL_SEAT").filter(|s| !s.is_empty()).cloned()
    }

    fn session_hint(&self, _env: &BTreeMap<String, String>, _hook: &Hook) -> SessionHint {
        // Hooks name neither the process nor the language server: the
        // process is the one the node launched, the server's address comes
        // from `amail` commands (`session_env`).
        SessionHint::default()
    }

    fn root_is_session(&self) -> bool {
        true
    }

    fn injects(&self, hook: &Hook) -> bool {
        // `PostToolUse` accepts only `{}`. No answer of `SessionStart` was
        // seen to reach the model; its notes come with the first
        // `PreInvocation`.
        !matches!(hook.name.as_str(), "PostToolUse" | "SessionStart")
    }

    fn session_env(&self, env: &BTreeMap<String, String>) -> Option<(Option<String>, String)> {
        let t = Target::from_env(env)?;
        Some((Some(t.conversation.clone()), t.to_target()))
    }

    fn session_start_state(&self, _source: &str) -> Option<SeatState> {
        Some(SeatState::Idle)
    }

    fn inject_budget(&self) -> usize {
        9_000
    }

    fn claim_limit(&self) -> u32 {
        8
    }

    fn render(&self, hook: &Hook, out: &Output) -> String {
        match (hook.name.as_str(), out) {
            (_, Output::None) => String::new(),
            ("PreInvocation", Output::Context(text)) => {
                json!({"injectSteps": [{"ephemeralMessage": text}]}).to_string()
            }
            ("Stop", Output::Block(text)) => {
                json!({"decision": "continue", "reason": text}).to_string()
            }
            _ => "{}".to_string(),
        }
    }

    async fn wake(&self, target: &str, text: &str) -> Result<()> {
        let t = Self::target(target)?;
        self.rpc(
            &t,
            "SendUserCascadeMessage",
            json!({
                "cascadeId": t.conversation,
                "items": [{"text": text}],
                "deliveryStrategy": "MESSAGE_DELIVERY_STRATEGY_WHEN_IDLE",
            }),
        )
        .await
        .map(drop)
    }

    /// `CancelCascadeInvocation` with `killBackgroundTasks`, when the
    /// conversation runs or has a command running in the background (agy
    /// then reports the conversation as idle). agy fires no hook after a
    /// cancelled turn or a killed background task.
    async fn interrupt(&self, target: &str) -> Result<bool> {
        let t = Self::target(target)?;
        let all = self
            .rpc(
                &t,
                "GetAllCascadeTrajectories",
                json!({"excludeSubtrajectories": true}),
            )
            .await?;
        let status = all
            .get("trajectorySummaries")
            .and_then(|m| m.get(&t.conversation))
            .and_then(|s| str_field(s, "status"))
            .unwrap_or_default();
        let running = status.ends_with("RUNNING") || status.ends_with("BUSY");
        let background = running || {
            let list = self.rpc(&t, "ListBackgroundTasks", json!({})).await?;
            list.get("tasks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|task| str_field(task, "conversationId") == Some(t.conversation.as_str()))
        };
        if !background {
            return Ok(false);
        }
        self.rpc(
            &t,
            "CancelCascadeInvocation",
            json!({"cascadeId": t.conversation, "killBackgroundTasks": true}),
        )
        .await?;
        Ok(true)
    }

    async fn prepare(&self) -> Result<()> {
        let dir = self.env.state_dir.join("agy");
        std::fs::create_dir_all(&dir)?;
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        Ok(())
    }

    async fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan> {
        let info = ctx.info;
        let cwd = Path::new(ctx.cwd);
        self.write_plugin(cwd)?;
        // Trust is by exact path: the directory as given and as resolved.
        let mut trusted = vec![ctx.cwd.to_string()];
        if let Ok(real) = std::fs::canonicalize(cwd) {
            let real = real.to_string_lossy().into_owned();
            if real != ctx.cwd {
                trusted.push(real);
            }
        }
        let mut args = Vec::new();
        if let Some(m) = info.model.as_deref().filter(|m| !m.is_empty()) {
            args.extend(["--model".to_string(), m.to_string()]);
        }
        args.extend([
            "--settings".to_string(),
            json!({ "trustedWorkspaces": trusted }).to_string(),
        ]);
        if ctx.permission_mode == PermissionMode::Unattended {
            args.push("--dangerously-skip-permissions".into());
        }
        if let Some(id) = ctx.resume {
            args.extend(["--conversation".to_string(), id.to_string()]);
        }
        args.extend(ctx.extra.iter().cloned());
        args.extend(["-i".to_string(), Self::first_prompt(ctx)]);
        let mut env = BTreeMap::from([
            ("AMAIL_SEAT".to_string(), info.seat.clone()),
            ("AMAIL_URL".to_string(), self.env.server_url.clone()),
            ("AMAIL_TOKEN".to_string(), info.token.clone()),
            (
                crate::paths::SOCK_ENV.to_string(),
                self.env.node_socket.to_string_lossy().into_owned(),
            ),
        ]);
        if std::env::var_os("TMPDIR").is_none_or(|t| t.is_empty()) {
            env.insert("TMPDIR".to_string(), "/tmp".to_string());
        }
        Ok(LaunchPlan {
            program: self.env.agy_bin.clone(),
            args,
            env,
            unset_env: UNSET.iter().map(|s| s.to_string()).collect(),
            path_prepend: self.env.amail_dir().into_iter().collect(),
            notice: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use amail_proto::LaunchInfo;

    fn agy(dir: &Path) -> Agy {
        Agy::new(AdapterEnv {
            state_dir: dir.to_path_buf(),
            amail_exe: PathBuf::from("/opt/amail dir/bin/amail"),
            node_socket: dir.join("node.sock"),
            server_url: "http://127.0.0.1:1".into(),
            claude_bin: "claude".into(),
            codex_bin: "codex".into(),
            codex_sandbox: None,
            dsh_bin: "dsh".into(),
            dsh_env: BTreeMap::new(),
            agy_bin: "/opt/agy".into(),
            codex_auth: dir.join("auth.json"),
            codex_pass: String::new(),
        })
    }

    fn info() -> LaunchInfo {
        serde_json::from_value(json!({
            "seat": "w1", "org": "acme", "token": "amail_t", "user_seat": "root",
            "model": "gemini-3.8-flash-low", "role": "You are w1.",
        }))
        .unwrap()
    }

    fn fixture(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../amail-cli/tests/fixtures/hooks/agy")
            .join(format!("{name}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn parses_hooks() {
        let a = agy(Path::new("/s"));
        let h = a.parse(&fixture("SessionStart")).unwrap();
        assert_eq!(
            h.event,
            Event::SessionStart {
                source: "startup".into()
            }
        );
        assert_eq!(
            h.session_id.as_deref(),
            Some("601130f6-5ef4-482a-bcfd-cd0a847d35b6")
        );
        assert!(h.transcript_path.as_deref().unwrap().ends_with(
            "brain/601130f6-5ef4-482a-bcfd-cd0a847d35b6/.system_generated/logs/transcript_full.jsonl"
        ));
        assert_eq!(h.facts.cwd.as_deref(), Some("/home/u/work/w1"));
        assert_eq!(h.facts.model.as_deref(), Some("gemini-3.8-flash-low"));

        let h = a.parse(&fixture("PreInvocation")).unwrap();
        assert_eq!(
            h.event,
            Event::UserPromptSubmit {
                prompt: "[amail] You have new mail (wake #3).".into()
            }
        );
        let mut later = fixture("PreInvocation");
        later["invocationNum"] = json!(2);
        let h = a.parse(&later).unwrap();
        assert_eq!(h.event, Event::PostToolUse);
        assert_eq!(h.facts.tool, None);

        let h = a.parse(&fixture("PostToolUse")).unwrap();
        assert_eq!(h.event, Event::PostToolUse);
        assert_eq!(h.facts.tool.as_deref(), Some("run_command"));
        assert_eq!(
            h.facts.tool_input.as_deref(),
            Some("cd /home/u/work/w1 && amail task done t3 --result ok")
        );
        assert!(h.facts.task_tool);
        assert!(!h.facts.note_tool);
        let mut view = fixture("PostToolUse");
        view["toolCall"] =
            json!({"id": "c2", "name": "view_file", "args": {"AbsolutePath": "/w/a"}});
        let h = a.parse(&view).unwrap();
        assert_eq!(
            h.facts.tool_input.as_deref(),
            Some("{\"AbsolutePath\":\"/w/a\"}")
        );
        assert!(!h.facts.task_tool);

        let h = a.parse(&fixture("Stop")).unwrap();
        assert_eq!(h.event, Event::Stop);
        // A command in the background: the turn ends too.
        let mut bg = fixture("Stop");
        bg["fullyIdle"] = json!(false);
        assert_eq!(a.parse(&bg).unwrap().event, Event::Stop);
        bg.as_object_mut().unwrap().remove("fullyIdle");
        assert_eq!(a.parse(&bg).unwrap().event, Event::Stop);

        for name in ["PreToolUse", "PostInvocation", "Other"] {
            assert!(a.parse(&json!({"hookEventName": name})).is_none());
        }
        assert!(a.parse(&json!({"hook_event_name": "Stop"})).is_none());
        assert!(a.parse(&json!([])).is_none());
    }

    #[test]
    fn renders_answers() {
        let a = agy(Path::new("/s"));
        let hook = |name: &str| a.parse(&fixture(name)).unwrap();
        let text = "[amail] 1 new message".to_string();
        let pre = hook("PreInvocation");
        let v: Value =
            serde_json::from_str(&a.render(&pre, &Output::Context(text.clone()))).unwrap();
        assert_eq!(v, json!({"injectSteps": [{"ephemeralMessage": text}]}));
        assert_eq!(a.render(&pre, &Output::None), "");
        let stop = hook("Stop");
        let v: Value =
            serde_json::from_str(&a.render(&stop, &Output::Block(text.clone()))).unwrap();
        assert_eq!(v, json!({"decision": "continue", "reason": text}));
        assert_eq!(a.render(&stop, &Output::None), "");
        assert_eq!(a.render(&stop, &Output::Context(text.clone())), "{}");
        let post = hook("PostToolUse");
        assert_eq!(a.render(&post, &Output::Context(text.clone())), "{}");
        let start = hook("SessionStart");
        assert_eq!(a.render(&start, &Output::Context(text)), "{}");
        assert!(a.injects(&pre));
        assert!(a.injects(&stop));
        assert!(!a.injects(&post));
        assert!(!a.injects(&start));
    }

    #[test]
    fn plugin_files_written_on_change() {
        let dir = tempfile::tempdir().unwrap();
        let a = agy(dir.path());
        let cwd = dir.path().join("w");
        std::fs::create_dir_all(&cwd).unwrap();
        a.write_plugin(&cwd).unwrap();
        let p = cwd.join(PLUGIN_DIR);
        assert_eq!(
            std::fs::read_to_string(p.join(".gitignore")).unwrap(),
            "*\n"
        );
        let plugin: Value =
            serde_json::from_str(&std::fs::read_to_string(p.join("plugin.json")).unwrap()).unwrap();
        assert_eq!(plugin["name"], "amail");
        assert_eq!(
            std::fs::read_to_string(p.join("rules/AGENTS.md")).unwrap(),
            crate::inject::MAIL_INSTRUCTIONS
        );
        let hooks: Value =
            serde_json::from_str(&std::fs::read_to_string(p.join("hooks.json")).unwrap()).unwrap();
        let events = hooks["amail"].as_object().unwrap();
        assert_eq!(events.len(), EVENTS.len());
        assert_eq!(
            hooks["amail"]["Stop"][0]["command"],
            "'/opt/amail dir/bin/amail' hook agy"
        );
        assert_eq!(hooks["amail"]["Stop"][0]["timeout"], 5);
        assert_eq!(
            hooks["amail"]["PostToolUse"][0]["hooks"][0]["command"],
            "'/opt/amail dir/bin/amail' hook agy"
        );
        assert!(hooks["amail"].get("PreToolUse").is_none());
        assert!(!p.join("mcp_config.json").exists());
        // Unchanged files are not written again; a changed one is.
        let ino =
            |f: &str| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p.join(f)).unwrap());
        let before = ino("hooks.json");
        a.write_plugin(&cwd).unwrap();
        assert_eq!(ino("hooks.json"), before);
        std::fs::write(p.join("hooks.json"), "{}").unwrap();
        let edited = ino("hooks.json");
        a.write_plugin(&cwd).unwrap();
        assert_ne!(ino("hooks.json"), edited);
        assert_eq!(
            std::fs::read_to_string(p.join("hooks.json")).unwrap(),
            a.plugin_files()[1].1
        );
        // A symbolic link on the way is refused.
        let other = dir.path().join("x");
        std::fs::create_dir_all(other.join(".agents")).unwrap();
        std::fs::create_dir_all(dir.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), other.join(".agents/plugins"))
            .unwrap();
        assert!(a.write_plugin(&other).is_err());
    }

    fn ctx<'a>(
        info: &'a LaunchInfo,
        cwd: &'a str,
        extra: &'a [String],
        mode: PermissionMode,
        resume: Option<&'a str>,
    ) -> LaunchCtx<'a> {
        LaunchCtx {
            info,
            permission_mode: mode,
            git_access: Default::default(),
            dsh_jail: true,
            cwd,
            extra,
            nonce: "n",
            resume,
            codex_effort: None,
            new_session_id: None,
            context_lost: false,
            adopted: false,
        }
    }

    #[tokio::test]
    async fn launch_plans() {
        let dir = tempfile::tempdir().unwrap();
        let a = agy(dir.path());
        let cwd = dir.path().join("w").to_string_lossy().into_owned();
        std::fs::create_dir_all(&cwd).unwrap();
        let info = info();
        let extra = vec!["--mode".to_string(), "plan".to_string()];
        let plan = a
            .launch(&ctx(&info, &cwd, &extra, PermissionMode::Unattended, None))
            .await
            .unwrap();
        assert_eq!(plan.program, "/opt/agy");
        let a0 = &plan.args;
        assert_eq!(&a0[..2], ["--model", "gemini-3.8-flash-low"]);
        assert_eq!(a0[2], "--settings");
        let settings: Value = serde_json::from_str(&a0[3]).unwrap();
        assert_eq!(settings["trustedWorkspaces"][0], cwd.as_str());
        assert_eq!(a0[4], "--dangerously-skip-permissions");
        assert_eq!(&a0[5..7], ["--mode", "plan"]);
        assert_eq!(a0[7], "-i");
        assert_eq!(a0.len(), 9);
        let prompt = &a0[8];
        assert!(prompt.starts_with("[amail] This is the start"), "{prompt}");
        assert!(prompt.contains(crate::inject::MAIL_INSTRUCTIONS));
        assert!(prompt.contains("You are seat `w1`"), "{prompt}");
        assert!(
            prompt.ends_with("Run `amail inbox` in the shell now, then carry on with your role.")
        );
        assert!(!a0.iter().any(|x| x == "--conversation"));
        assert_eq!(plan.env["AMAIL_SEAT"], "w1");
        assert_eq!(plan.env["AMAIL_TOKEN"], "amail_t");
        assert_eq!(plan.env["AMAIL_URL"], "http://127.0.0.1:1");
        assert!(plan.env.contains_key(crate::paths::SOCK_ENV));
        for k in [
            "AMAIL_PROFILE",
            crate::wire::CODEX_PASS_ENV,
            "ANTIGRAVITY_LS_ADDRESS",
            "ANTIGRAVITY_CSRF_TOKEN",
            "ANTIGRAVITY_CONVERSATION_ID",
            "ANTIGRAVITY_TRAJECTORY_ID",
            "ANTIGRAVITY_APP_DATA_DIR",
            "ANTIGRAVITY_AGENTAPI_EXE",
            "ANTIGRAVITY_LS_VERSION",
        ] {
            assert!(plan.unset_env.iter().any(|u| u == k), "{k}");
        }
        assert_eq!(plan.path_prepend, vec!["/opt/amail dir/bin".to_string()]);
        assert!(
            Path::new(&cwd)
                .join(PLUGIN_DIR)
                .join("hooks.json")
                .is_file()
        );

        // Default mode: the prompts stay; the context-lost start says so.
        let mut c = ctx(&info, &cwd, &[], PermissionMode::Default, None);
        c.context_lost = true;
        let plan = a.launch(&c).await.unwrap();
        assert!(
            !plan
                .args
                .iter()
                .any(|x| x == "--dangerously-skip-permissions")
        );
        assert!(plan.args.last().unwrap().ends_with(CONTEXT_LOST));

        // Resume: the conversation, and a short prompt.
        let plan = a
            .launch(&ctx(
                &info,
                &cwd,
                &[],
                PermissionMode::Default,
                Some("conv-1"),
            ))
            .await
            .unwrap();
        let i = plan
            .args
            .iter()
            .position(|x| x == "--conversation")
            .unwrap();
        assert_eq!(plan.args[i + 1], "conv-1");
        assert_eq!(plan.args[plan.args.len() - 2], "-i");
        assert_eq!(plan.args.last().unwrap(), RESUMED);

        // Adopted: told that it is a seat now.
        let mut c = ctx(&info, &cwd, &[], PermissionMode::Default, Some("conv-2"));
        c.adopted = true;
        let plan = a.launch(&c).await.unwrap();
        assert_eq!(plan.args.last().unwrap(), &adopted_prompt(&info));

        // No model: no `--model`.
        let mut bare = info.clone();
        bare.model = None;
        let plan = a
            .launch(&ctx(&bare, &cwd, &[], PermissionMode::Default, None))
            .await
            .unwrap();
        assert_eq!(plan.args[0], "--settings");
    }

    #[test]
    fn harness_names() {
        for n in ["agy", "Antigravity", "antigravity-cli"] {
            assert_eq!(Harness::parse(n), Some(Harness::Agy), "{n}");
        }
        assert_eq!(Harness::Agy.as_str(), "agy");
    }

    #[test]
    fn targets() {
        let env = |addr: &str, token: &str, conv: &str| {
            BTreeMap::from([
                (ADDRESS_ENV.to_string(), addr.to_string()),
                (CSRF_ENV.to_string(), token.to_string()),
                (CONVERSATION_ENV.to_string(), conv.to_string()),
                ("AMAIL_SEAT".to_string(), "w1".to_string()),
            ])
        };
        let t = Target::from_env(&env("localhost:43123", "tok-1", "c-1")).unwrap();
        assert_eq!(
            t,
            Target {
                address: "localhost:43123".into(),
                token: "tok-1".into(),
                conversation: "c-1".into()
            }
        );
        let s = t.to_target();
        assert_eq!(Target::from_target(&s), Some(t.clone()));
        assert!(!s.contains(' '));
        let a = agy(Path::new("/s"));
        assert_eq!(
            a.session_env(&env("127.0.0.1:1", "x", "c-1")),
            Some((
                Some("c-1".to_string()),
                Target {
                    address: "127.0.0.1:1".into(),
                    token: "x".into(),
                    conversation: "c-1".into()
                }
                .to_target()
            ))
        );
        // Not on this machine, no port, a token with a space or a newline,
        // a conversation id with a slash, or a variable missing.
        for (addr, token, conv) in [
            ("example.com:80", "x", "c"),
            ("localhost", "x", "c"),
            ("localhost:99999", "x", "c"),
            ("localhost:1", "a b", "c"),
            ("localhost:1", "a\nb", "c"),
            ("localhost:1", "", "c"),
            ("localhost:1", "x", "../c"),
        ] {
            assert_eq!(
                Target::from_env(&env(addr, token, conv)),
                None,
                "{addr} {token:?} {conv}"
            );
        }
        let mut partial = env("localhost:1", "x", "c");
        partial.remove(CONVERSATION_ENV);
        assert_eq!(Target::from_env(&partial), None);
        assert_eq!(Target::from_target("not json"), None);
        assert_eq!(Target::from_target("/run/x.sock"), None);
    }
}
