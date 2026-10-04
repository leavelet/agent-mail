//! Claude Code.
//!
//! - Hooks run as children of the `claude` process and inherit its
//!   environment, so the seat comes from `AMAIL_SEAT`. Events that carry an
//!   `agent_id` come from internal helper agents and are ignored.
//! - Wake-up: one JSON line written to the session's inbox socket
//!   (`CLAUDE_CODE_MESSAGING_SOCKET`). That starts a turn and fires
//!   `UserPromptSubmit`, whose hook injects the mail.
//! - Launch: `--settings` with all hooks, `crossSessionInbound: "accept"`
//!   (needed for socket messages to reach bypass-mode sessions) and
//!   `skipDangerousModePermissionPrompt`; these two only take effect when
//!   given through `--settings`.

use super::*;
use amail_proto::PermissionMode;
use anyhow::Context;
use tokio::io::AsyncWriteExt;

/// Events the generated settings register.
const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Stop",
    "StopFailure",
    "Notification",
    "PermissionRequest",
    "SessionEnd",
];

/// Environment of the launching shell that must not leak into a new session.
const UNSET: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "AMAIL_PROFILE",
    crate::wire::CODEX_PASS_ENV,
];

pub struct Claude {
    env: AdapterEnv,
}

impl Claude {
    pub fn new(env: AdapterEnv) -> Self {
        Claude { env }
    }

    pub fn settings_path(&self) -> PathBuf {
        self.env.state_dir.join("claude").join("settings.json")
    }

    /// Fixed sender address for wake-ups (nothing listens there).
    fn sender_address(&self) -> String {
        let dir = self
            .env
            .node_socket
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        format!("uds:{}", dir.join("wake.sock").display())
    }

    pub fn settings(&self) -> Value {
        let cmd = format!(
            "{} hook claude",
            shell_quote(&self.env.amail_exe.to_string_lossy())
        );
        let hooks: serde_json::Map<String, Value> = EVENTS
            .iter()
            .map(|e| {
                (
                    e.to_string(),
                    json!([{"matcher": "*", "hooks": [{"type": "command", "command": cmd, "timeout": 5}]}]),
                )
            })
            .collect();
        json!({
            "hooks": hooks,
            "crossSessionInbound": "accept",
            "skipDangerousModePermissionPrompt": true,
        })
    }

    /// The wake-up line for the inbox socket.
    pub fn wake_line(&self, text: &str) -> String {
        let content = format!(
            "<cross-session-message from-name=\"amail\">\n{text}\n</cross-session-message>"
        );
        let mut line = json!({
            "type": "user",
            "message": {"role": "user", "content": content},
            "from": self.sender_address(),
        })
        .to_string();
        line.push('\n');
        line
    }
}

/// The error of a `StopFailure` hook: its type and message. Claude Code
/// 2.1.289 sends `error` (the type, e.g. `authentication_failed`) and
/// `last_assistant_message` (the text shown); its documentation names
/// `error_type` and `error_message`. All are read.
pub fn stop_failure_reason(input: &Value) -> String {
    let kind = str_field(input, "error_type").or_else(|| str_field(input, "error"));
    let msg = str_field(input, "error_message")
        .or_else(|| str_field(input, "last_assistant_message"))
        .or_else(|| str_field(input, "error_details"));
    match (kind, msg) {
        (Some(k), Some(m)) => format!("{k}: {m}"),
        (Some(k), None) => k.to_string(),
        (None, Some(m)) => m.to_string(),
        (None, None) => "the turn ended with an error".to_string(),
    }
}

#[async_trait::async_trait]
impl Adapter for Claude {
    fn harness(&self) -> Harness {
        Harness::Claude
    }

    fn parse(&self, input: &Value) -> Option<Hook> {
        if input.get("agent_id").is_some_and(|v| !v.is_null()) {
            return None;
        }
        let name = str_field(input, "hook_event_name")?.to_string();
        let event = match name.as_str() {
            "SessionStart" => Event::SessionStart {
                source: str_field(input, "source").unwrap_or("startup").to_string(),
            },
            "UserPromptSubmit" => Event::UserPromptSubmit {
                prompt: str_field(input, "prompt").unwrap_or_default().to_string(),
            },
            "PostToolUse" => Event::PostToolUse,
            "Stop" => Event::Stop,
            // A turn that ended in an API error (instead of `Stop`).
            "StopFailure" => Event::TurnFailed {
                reason: stop_failure_reason(input),
            },
            "PermissionRequest" => Event::Awaiting,
            "Notification" => match str_field(input, "notification_type") {
                Some("permission_prompt") => Event::Awaiting,
                _ => return None,
            },
            "SessionEnd" => Event::SessionEnd,
            _ => return None,
        };
        Some(Hook {
            name,
            event,
            session_id: str_field(input, "session_id").map(str::to_string),
            transcript_path: str_field(input, "transcript_path")
                .filter(|p| !p.is_empty())
                .map(str::to_string),
        })
    }

    fn seat_from_env(&self, env: &BTreeMap<String, String>) -> Option<String> {
        env.get("AMAIL_SEAT").filter(|s| !s.is_empty()).cloned()
    }

    fn session_hint(&self, env: &BTreeMap<String, String>, _hook: &Hook) -> SessionHint {
        let socket = env
            .get("CLAUDE_CODE_MESSAGING_SOCKET")
            .filter(|s| !s.is_empty())
            .cloned();
        // The socket is named after the claude pid.
        let pid = env
            .get("CLAUDE_PID")
            .and_then(|p| p.parse().ok())
            .or_else(|| {
                let s = socket.as_deref()?;
                std::path::Path::new(s).file_stem()?.to_str()?.parse().ok()
            });
        SessionHint {
            pid,
            wake_target: socket,
        }
    }

    fn session_start_state(&self, source: &str) -> Option<SeatState> {
        // A new, resumed or cleared session waits at the prompt; a session
        // compacting mid-turn keeps its state.
        match source {
            "compact" => None,
            _ => Some(SeatState::Idle),
        }
    }

    fn inject_budget(&self) -> usize {
        // Claude Code truncates hook context above 10,000 characters.
        9_000
    }

    fn claim_limit(&self) -> u32 {
        // Claimed mail is packed into `inject_budget` by its rendered size;
        // what does not fit waits for the next hook.
        8
    }

    fn target_alive(&self, target: &str) -> bool {
        std::path::Path::new(target).exists()
    }

    async fn wake(&self, target: &str, text: &str) -> Result<()> {
        let line = self.wake_line(text);
        let fut = async {
            let mut s = tokio::net::UnixStream::connect(target)
                .await
                .with_context(|| format!("connecting to {target}"))?;
            s.write_all(line.as_bytes()).await?;
            s.flush().await?;
            s.shutdown().await.ok();
            anyhow::Ok(())
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), fut)
            .await
            .context("timed out writing to the session socket")?
    }

    async fn prepare(&self) -> Result<()> {
        let path = self.settings_path();
        let dir = path.parent().context("settings path")?;
        tokio::fs::create_dir_all(dir).await?;
        let text = serde_json::to_string_pretty(&self.settings())?;
        crate::persist::write_atomic(&path, text.as_bytes(), 0o600)?;
        Ok(())
    }

    async fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan> {
        let info = ctx.info;
        let prompt = format!(
            "{}\n# Your seat\n\n{}\n",
            crate::inject::MAIL_INSTRUCTIONS,
            crate::inject::seat_intro(info)
        );
        let mut args = vec![
            "--settings".to_string(),
            self.settings_path().to_string_lossy().into_owned(),
            "--append-system-prompt".into(),
            prompt,
            "--name".into(),
            info.seat.clone(),
        ];
        match (ctx.resume, ctx.new_session_id) {
            (Some(id), _) => args.extend(["--resume".into(), id.to_string()]),
            (None, Some(id)) => args.extend(["--session-id".into(), id.to_string()]),
            (None, None) => {}
        }
        if let Some(m) = &info.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        if ctx.permission_mode == PermissionMode::Unattended {
            args.push("--dangerously-skip-permissions".into());
        }
        args.extend(ctx.extra.iter().cloned());
        if ctx.resume.is_none() && ctx.context_lost {
            args.push(CONTEXT_LOST.to_string());
        }
        let mut env = BTreeMap::from([
            ("AMAIL_SEAT".to_string(), info.seat.clone()),
            ("AMAIL_URL".to_string(), self.env.server_url.clone()),
            ("AMAIL_TOKEN".to_string(), info.token.clone()),
            (
                crate::paths::SOCK_ENV.to_string(),
                self.env.node_socket.to_string_lossy().into_owned(),
            ),
        ]);
        // Agents use `$TMPDIR` in their commands; with it unset such a path
        // becomes one in `/`.
        if std::env::var_os("TMPDIR").is_none_or(|t| t.is_empty()) {
            env.insert("TMPDIR".to_string(), "/tmp".to_string());
        }
        Ok(LaunchPlan {
            program: self.env.claude_bin.clone(),
            args,
            env,
            unset_env: UNSET.iter().map(|s| s.to_string()).collect(),
            path_prepend: self.env.amail_dir().into_iter().collect(),
            notice: None,
        })
    }
}
