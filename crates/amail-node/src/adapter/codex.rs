//! Codex.
//!
//! - All Codex seats on a host share one dedicated `CODEX_HOME` (under the
//!   node's state directory) and its app-server daemon. Interactive sessions
//!   attach to that daemon, and hooks run inside it, so a hook cannot see the
//!   session's environment. A session is tied to its seat by a marker in its
//!   first prompt, `[amail:seat=<seat> launch=<nonce>]`, where the nonce was
//!   reserved by `amail run`; after that the hook's `session_id` identifies
//!   the seat.
//! - Hooks are defined in the home's `config.toml` and pre-trusted there
//!   (trust hashes cover the command string, which is fixed as
//!   `<amail> hook codex`).
//! - Wake-up: `codex queue --thread <session id> --message ...`, which starts
//!   a turn on an idle thread and fires `UserPromptSubmit`.
//! - Launch flags are limited to ones that keep the session attached to the
//!   shared daemon (`-m`, `-a`, `-s`, `-C`).

use super::*;
use amail_proto::PermissionMode;
use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::sync::Mutex;

/// Hook events: (config name, snake_case name used in trust keys).
const EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PostToolUse", "post_tool_use"),
    ("PermissionRequest", "permission_request"),
    ("Stop", "stop"),
    ("Interrupt", "interrupt"),
    ("SessionEnd", "session_end"),
];

const HOOK_TIMEOUT: i64 = 5;

/// The permissions profile Codex sessions run under (see `apply_managed`).
const PERMISSIONS_PROFILE: &str = "amail";

/// The git directory and common directory of `cwd` (absolute), if it is in
/// a git repository.
fn git_dirs(cwd: &str) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            cwd,
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Codex caps the timeout of some events at 3 s (and warns about larger ones).
fn hook_timeout(snake: &str) -> i64 {
    if matches!(snake, "session_end" | "interrupt") {
        HOOK_TIMEOUT.min(3)
    } else {
        HOOK_TIMEOUT
    }
}

/// Flags that detach the TUI from the shared daemon or bypass hook trust.
const FORBIDDEN_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-p",
    "--profile",
    "--dangerously-bypass-hook-trust",
    "--approve-for-me",
    "--remote",
];

const UNSET: &[&str] = &[
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "AMAIL_PROFILE",
];

pub struct Codex {
    env: AdapterEnv,
    /// Serializes edits of `config.toml` and daemon start-up.
    lock: Mutex<()>,
}

impl Codex {
    pub fn new(env: AdapterEnv) -> Self {
        Codex {
            env,
            lock: Mutex::new(()),
        }
    }

    pub fn home(&self) -> PathBuf {
        self.env.state_dir.join("codex-home")
    }

    fn config_path(&self) -> PathBuf {
        self.home().join("config.toml")
    }

    pub fn control_socket(&self) -> PathBuf {
        self.home()
            .join("app-server-control")
            .join("app-server-control.sock")
    }

    pub fn hook_command(&self) -> String {
        format!(
            "{} hook codex",
            shell_quote(&self.env.amail_exe.to_string_lossy())
        )
    }

    fn read_config(&self) -> Result<toml::Table> {
        match std::fs::read_to_string(self.config_path()) {
            Ok(s) => s
                .parse::<toml::Table>()
                .with_context(|| format!("parsing {}", self.config_path().display())),
            Err(_) => Ok(toml::Table::new()),
        }
    }

    fn write_config(&self, t: &toml::Table) -> Result<()> {
        let text = format!(
            "# Managed by amail. Keys amail does not manage are kept.\n{}",
            toml::to_string(t)?
        );
        crate::persist::write_atomic(&self.config_path(), text.as_bytes(), 0o600)
    }

    /// The trust key and hash Codex computes for our handler of `snake`.
    fn trust_entry(&self, snake: &str) -> (String, String) {
        let timeout = hook_timeout(snake);
        let canonical = serde_json::json!({
            "event_name": snake,
            "hooks": [{"async": false, "command": self.hook_command(), "timeout": timeout, "type": "command"}],
        });
        // serde_json maps are sorted, and to_string is compact.
        let hash = hex::encode(Sha256::digest(canonical.to_string().as_bytes()));
        (
            format!("{}:{snake}:0:0", self.config_path().display()),
            format!("sha256:{hash}"),
        )
    }

    /// Set the amail-managed keys of `config.toml`.
    fn apply_managed(&self, t: &mut toml::Table, trust: &BTreeMap<String, String>) {
        use toml::Value as V;
        t.insert(
            "developer_instructions".into(),
            V::String(crate::inject::MAIL_INSTRUCTIONS.to_string()),
        );
        let mut hooks = toml::Table::new();
        for (name, snake) in EVENTS {
            let mut handler = toml::Table::new();
            handler.insert("type".into(), V::String("command".into()));
            handler.insert("command".into(), V::String(self.hook_command()));
            handler.insert("timeout".into(), V::Integer(hook_timeout(snake)));
            let mut group = toml::Table::new();
            group.insert("hooks".into(), V::Array(vec![V::Table(handler)]));
            hooks.insert(name.to_string(), V::Array(vec![V::Table(group)]));
        }
        let mut state = toml::Table::new();
        for (key, hash) in trust {
            let mut e = toml::Table::new();
            e.insert("trusted_hash".into(), V::String(hash.clone()));
            state.insert(key.clone(), V::Table(e));
        }
        hooks.insert("state".into(), V::Table(state));
        t.insert("hooks".into(), V::Table(hooks));
        // Agents' `amail` commands reach the server over loopback. Without
        // network access the Codex sandbox refuses `socket(AF_INET)` and also
        // `connect()` on Unix sockets (EPERM, even inside the workspace), so
        // going through the node socket instead would not help.
        let sww = t
            .entry("sandbox_workspace_write")
            .or_insert_with(|| V::Table(toml::Table::new()));
        if let V::Table(sww) = sww {
            sww.insert("network_access".into(), V::Boolean(true));
        }
        // Sessions run under the `amail` permissions profile: the workspace
        // profile plus network access (as above) plus write access to the
        // git directories of the Codex seats' working directories (added at
        // launch, see `grant_git_dirs`).
        t.insert(
            "default_permissions".into(),
            V::String(PERMISSIONS_PROFILE.into()),
        );
        let perms = t
            .entry("permissions")
            .or_insert_with(|| V::Table(toml::Table::new()));
        if let V::Table(perms) = perms {
            let prof = perms
                .entry(PERMISSIONS_PROFILE)
                .or_insert_with(|| V::Table(toml::Table::new()));
            if let V::Table(prof) = prof {
                prof.insert("extends".into(), V::String(":workspace".into()));
                let mut net = toml::Table::new();
                net.insert("enabled".into(), V::Boolean(true));
                prof.insert("network".into(), V::Table(net));
                prof.entry("filesystem")
                    .or_insert_with(|| V::Table(toml::Table::new()));
            }
        }
    }

    /// Let sessions write the git directories of `cwd` (its git dir and
    /// the repository's common dir) so a seat can commit on its branch.
    ///
    /// The workspace sandbox keeps `.git` read-only, and a worktree's git
    /// directory lies outside the worktree. Only these directories are
    /// added; the rest of the repository and the file system stay as the
    /// workspace profile has them. For a worktree the grant covers the
    /// whole repository's refs and objects (every branch), which is what
    /// committing and rebasing need.
    fn grant_git_dirs(&self, cwd: &str) -> Result<Vec<String>> {
        let dirs = git_dirs(cwd);
        if dirs.is_empty() {
            return Ok(dirs);
        }
        let mut t = self.read_config()?;
        let mut changed = false;
        if let Some(toml::Value::Table(fs)) = t
            .get_mut("permissions")
            .and_then(|p| p.get_mut(PERMISSIONS_PROFILE))
            .and_then(|p| p.get_mut("filesystem"))
        {
            for d in &dirs {
                if fs.get(d).and_then(|v| v.as_str()) != Some("write") {
                    fs.insert(d.clone(), toml::Value::String("write".into()));
                    changed = true;
                }
            }
        }
        if changed {
            self.write_config(&t)?;
        }
        Ok(dirs)
    }

    /// Ask a throwaway app-server which of our hooks it trusts. Returns the
    /// key and current hash of every hook defined in our `config.toml`.
    async fn hook_status(&self) -> Result<Vec<(String, String, String)>> {
        let home = self.home();
        let r = crate::codex_rpc::stdio_calls(
            &self.env.codex_bin,
            &home,
            &[(
                "hooks/list",
                serde_json::json!({"cwds": [home.to_string_lossy()]}),
            )],
        )
        .await?;
        let mut out = Vec::new();
        let config = self.config_path();
        for entry in r[0]["data"].as_array().into_iter().flatten() {
            for h in entry["hooks"].as_array().into_iter().flatten() {
                let src = h["sourcePath"].as_str().unwrap_or_default();
                if Path::new(src) != config {
                    continue;
                }
                out.push((
                    h["key"].as_str().unwrap_or_default().to_string(),
                    h["currentHash"].as_str().unwrap_or_default().to_string(),
                    h["trustStatus"].as_str().unwrap_or_default().to_string(),
                ));
            }
        }
        Ok(out)
    }

    async fn daemon_up(&self) -> bool {
        tokio::net::UnixStream::connect(self.control_socket())
            .await
            .is_ok()
    }

    /// Start the home's app-server daemon if it is not running. The daemon
    /// is the environment hooks and agent tool commands run in, so it gets
    /// the node socket and server URL and none of a seat's variables.
    pub async fn ensure_daemon(&self) -> Result<()> {
        let _g = self.lock.lock().await;
        if self.daemon_up().await {
            return Ok(());
        }
        let out = tokio::process::Command::new(&self.env.codex_bin)
            .args(["app-server", "daemon", "start"])
            .env("CODEX_HOME", self.home())
            .env(crate::paths::SOCK_ENV, &self.env.node_socket)
            .env("AMAIL_URL", &self.env.server_url)
            .env("PATH", self.env.path_with_amail())
            .env_remove("AMAIL_SEAT")
            .env_remove("AMAIL_TOKEN")
            .env_remove("AMAIL_PROFILE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        let out = tokio::time::timeout(Duration::from_secs(60), out)
            .await
            .context("codex daemon start timed out")?
            .with_context(|| format!("running {} app-server daemon start", self.env.codex_bin))?;
        if !out.status.success() {
            bail!(
                "codex app-server daemon start failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        for _ in 0..100 {
            if self.daemon_up().await {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!(
            "codex daemon did not come up at {}",
            self.control_socket().display()
        )
    }

    /// Mark `cwd` as a trusted project so the TUI starts without asking.
    fn trust_project(&self, cwd: &str) -> Result<()> {
        let mut t = self.read_config()?;
        let projects = t
            .entry("projects")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let toml::Value::Table(projects) = projects else {
            bail!("config.toml: projects is not a table");
        };
        let already = projects
            .get(cwd)
            .and_then(|p| p.get("trust_level"))
            .and_then(|v| v.as_str())
            == Some("trusted");
        if already {
            return Ok(());
        }
        let mut p = toml::Table::new();
        p.insert("trust_level".into(), toml::Value::String("trusted".into()));
        projects.insert(cwd.to_string(), toml::Value::Table(p));
        self.write_config(&t)
    }

    fn check_extra(extra: &[String]) -> Result<()> {
        for a in extra {
            let flag = a.split('=').next().unwrap_or(a);
            if FORBIDDEN_FLAGS.contains(&flag)
                || (a.starts_with("-c") && a.len() > 2 && !a.starts_with("--"))
            {
                bail!(
                    "codex flag {a:?} is not allowed: it would detach the session from the shared daemon \
                     or bypass hook trust (allowed: -m, -a, -s, -C and other flags that keep the daemon)"
                );
            }
        }
        Ok(())
    }

    pub fn marker(seat: &str, nonce: &str) -> String {
        format!("[amail:seat={seat} launch={nonce}]")
    }
}

/// Processes of this user whose environment contains `entry` exactly.
fn processes_with_env(entry: &str) -> Vec<u32> {
    let me = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .map(|env| env.split(|b| *b == 0).any(|kv| kv == entry.as_bytes()))
                .unwrap_or(false)
        })
        .collect()
}

/// Parse `[amail:seat=<seat> launch=<nonce>]` anywhere in `prompt`.
pub fn parse_marker(prompt: &str) -> Option<(String, String)> {
    let start = prompt.find("[amail:seat=")?;
    let rest = &prompt[start + "[amail:seat=".len()..];
    let end = rest.find(']')?;
    let (seat, nonce) = rest[..end].split_once(" launch=")?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    (ok(seat) && ok(nonce)).then(|| (seat.to_string(), nonce.to_string()))
}

#[async_trait::async_trait]
impl Adapter for Codex {
    fn harness(&self) -> Harness {
        Harness::Codex
    }

    fn parse(&self, input: &Value) -> Option<Hook> {
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
            "Interrupt" => Event::Interrupt,
            "PermissionRequest" => Event::Awaiting,
            "SessionEnd" => Event::SessionEnd,
            _ => return None,
        };
        Some(Hook {
            name,
            event,
            session_id: str_field(input, "session_id").map(str::to_string),
        })
    }

    fn seat_from_env(&self, _env: &BTreeMap<String, String>) -> Option<String> {
        // Hooks run in the shared daemon: its environment names no seat.
        None
    }

    fn registers_sessions(&self) -> bool {
        true
    }

    fn registration_marker(&self, prompt: &str) -> Option<(String, String)> {
        parse_marker(prompt)
    }

    fn session_hint(&self, _env: &BTreeMap<String, String>, hook: &Hook) -> SessionHint {
        SessionHint {
            pid: None,
            wake_target: hook.session_id.clone(),
        }
    }

    fn session_start_state(&self, _source: &str) -> Option<SeatState> {
        // SessionStart fires lazily, when the first turn starts.
        Some(SeatState::Busy)
    }

    fn inject_budget(&self) -> usize {
        // Stay well under Codex's hook context limit (about 2,500 tokens).
        6_000
    }

    fn claim_limit(&self) -> u32 {
        5
    }

    async fn wake(&self, target: &str, text: &str) -> Result<()> {
        let out = tokio::process::Command::new(&self.env.codex_bin)
            .args(["queue", "--thread", target, "--message", text])
            .env("CODEX_HOME", self.home())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        let out = tokio::time::timeout(Duration::from_secs(15), out)
            .await
            .context("codex queue timed out")??;
        if !out.status.success() {
            bail!(
                "codex queue failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    async fn prepare(&self) -> Result<()> {
        let _g = self.lock.lock().await;
        let home = self.home();
        std::fs::create_dir_all(&home)?;
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let auth = home.join("auth.json");
        if std::fs::symlink_metadata(&auth).is_err() {
            std::os::unix::fs::symlink(&self.env.codex_auth, &auth)
                .with_context(|| format!("linking {}", auth.display()))?;
        }
        let trust: BTreeMap<String, String> = EVENTS
            .iter()
            .map(|(_, snake)| self.trust_entry(snake))
            .collect();
        let mut t = self.read_config()?;
        self.apply_managed(&mut t, &trust);
        self.write_config(&t)?;
        // Check with Codex itself; adopt its keys and hashes if ours differ.
        match self.hook_status().await {
            Ok(status) => {
                if status.iter().all(|(_, _, s)| s == "trusted") && status.len() == EVENTS.len() {
                    return Ok(());
                }
                let trust: BTreeMap<String, String> = status
                    .iter()
                    .map(|(k, h, _)| (k.clone(), h.clone()))
                    .collect();
                self.apply_managed(&mut t, &trust);
                self.write_config(&t)?;
                let status = self.hook_status().await?;
                if let Some((k, _, s)) = status.iter().find(|(_, _, s)| s != "trusted") {
                    bail!("codex hook {k} is {s} after trusting it");
                }
            }
            Err(e) => tracing::warn!("could not verify codex hook trust: {e:#}"),
        }
        Ok(())
    }

    async fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan> {
        Self::check_extra(ctx.extra)?;
        {
            let _g = self.lock.lock().await;
            self.trust_project(ctx.cwd)?;
            self.grant_git_dirs(ctx.cwd)?;
        }
        if let Err(e) = self.ensure_daemon().await {
            // The TUI starts a daemon itself; it inherits the launch
            // environment, which carries the node socket, so hooks still work.
            tracing::warn!("{e:#}");
        }
        let info = ctx.info;
        let mut args = Vec::new();
        if ctx.resume.is_some() {
            // Queued messages for the thread run when it is resumed.
            args.push("resume".to_string());
        }
        if let Some(m) = &info.model {
            args.extend(["-m".to_string(), m.clone()]);
        }
        if info.permission_mode == Some(PermissionMode::Unattended) {
            // No `-s`: the sandbox comes from the `amail` permissions profile
            // (workspace + network + the seat's git directories).
            args.extend(["-a", "never"].map(String::from));
        }
        args.extend(ctx.extra.iter().cloned());
        if let Some(id) = ctx.resume {
            args.push(id.to_string());
        } else {
            let start = if ctx.context_lost {
                CONTEXT_LOST
            } else {
                "This is the start of your session. Handle any new mail shown to you. \
                 If there is none, say that you are ready and stop."
            };
            args.push(format!(
                "{}\n{}\n\n{start}",
                Self::marker(&info.seat, ctx.nonce),
                crate::inject::seat_intro(info)
            ));
        }
        let env = BTreeMap::from([
            (
                "CODEX_HOME".to_string(),
                self.home().to_string_lossy().into_owned(),
            ),
            ("AMAIL_SEAT".to_string(), info.seat.clone()),
            ("AMAIL_URL".to_string(), self.env.server_url.clone()),
            ("AMAIL_TOKEN".to_string(), info.token.clone()),
            (
                crate::paths::SOCK_ENV.to_string(),
                self.env.node_socket.to_string_lossy().into_owned(),
            ),
        ]);
        Ok(LaunchPlan {
            program: self.env.codex_bin.clone(),
            args,
            env,
            unset_env: UNSET.iter().map(|s| s.to_string()).collect(),
            path_prepend: self.env.amail_dir().into_iter().collect(),
        })
    }

    fn resume_is_lazy(&self) -> bool {
        true
    }

    async fn shutdown(&self) {
        let _g = self.lock.lock().await;
        let out = tokio::process::Command::new(&self.env.codex_bin)
            .args(["app-server", "daemon", "stop"])
            .env("CODEX_HOME", self.home())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = tokio::time::timeout(Duration::from_secs(20), out).await;
        // `daemon stop` leaves helpers (the pid-update loop) running: end
        // every remaining process that runs with our CODEX_HOME.
        let want = format!("CODEX_HOME={}", self.home().display());
        for pid in processes_with_env(&want) {
            tracing::info!("stopping leftover codex process {pid}");
            // SAFETY: plain signal to a process of ours.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }

    async fn registered(&self, session_id: &str, seat: &str) {
        let r = crate::codex_rpc::socket_calls(
            &self.control_socket(),
            &[(
                "thread/name/set",
                serde_json::json!({"threadId": session_id, "name": seat}),
            )],
        )
        .await;
        if let Err(e) = r {
            tracing::warn!("naming codex thread {session_id} as {seat}: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker() {
        let m = Codex::marker("w-1", "ab12");
        assert_eq!(
            parse_marker(&format!("{m}\nYou are...")),
            Some(("w-1".into(), "ab12".into()))
        );
        assert_eq!(parse_marker("hello"), None);
        assert_eq!(parse_marker("[amail:seat=a b launch=x]"), None);
        assert_eq!(parse_marker("[amail:seat=a launch=]"), None);
    }

    #[test]
    fn extra_flags() {
        assert!(Codex::check_extra(&["-m".into(), "x".into(), "-C".into(), "/w".into()]).is_ok());
        for bad in [
            "-c",
            "-ckey=v",
            "--config=x",
            "-p",
            "--remote",
            "--approve-for-me",
        ] {
            assert!(Codex::check_extra(&[bad.into()]).is_err(), "{bad}");
        }
    }
}
