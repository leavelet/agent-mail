//! DeepSeek Harness (`dsh`).
//!
//! dsh has no terminal interface: the surface a person uses is the browser
//! GUI of a `dsh web` host, and that host's HTTP API is also how amail
//! reaches a session from outside.
//!
//! - Launch: each seat gets its own `dsh web` host (`--port 0`), run under
//!   `amail dsh-host` (see [`crate::dsh_host`]). The wrapper reads the
//!   host's `dsh web: <url>?token=<token>` start-up line, writes the
//!   address and token to a private endpoint file, creates (or resumes) the
//!   seat's session through the API and sends the first prompt. All dsh
//!   seats of a node share one dsh home (`<state>/dsh-home`) with plain JSONL
//!   session logs; seat settings (system prompt, model) come from a
//!   per-seat `--patch` overlay.
//! - Hooks: dsh's Codex hook bridge runs `amail hook dsh` for
//!   `SessionStart`, `UserPromptSubmit`, `PostToolUse` and `Stop`. Hooks are
//!   children of the host, so `AMAIL_SEAT` and the endpoint file's path
//!   (`AMAIL_DSH_ENDPOINT`) come from the environment. The bridge has no
//!   `SessionEnd` or permission events, and no hook marks a turn that was
//!   cancelled or failed: the wrapper watches the session log for such a
//!   `turn/end` and reports a failed one (reason `error`, or `interrupted`)
//!   as a `TurnFailed` event with the error text, any other as an
//!   `Interrupt` event.
//! - Wake-up: `POST /api/session/prompt` with `mode: "queue"`. On an idle
//!   session that starts a turn (and `UserPromptSubmit`); on a busy one it
//!   runs as the next turn. A session that is not loaded is resumed by it.

use super::*;
use amail_proto::PermissionMode;
use anyhow::{Context, bail};
use std::path::Path;
use std::time::Duration;

/// Hook events registered with the bridge.
const EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"];

/// The synthetic event the wrapper sends for a turn that ended without
/// `Stop` and without an error (cancelled in the GUI, and other endings
/// that are not failures).
pub const INTERRUPT: &str = "Interrupt";

/// The synthetic event the wrapper sends for a turn that failed (a
/// `turn/end` with reason `error`, or `interrupted`: a turn whose host
/// ended during it). Its `reason` field holds the error text.
pub const TURN_FAILED: &str = "TurnFailed";

/// Environment variable naming the seat's endpoint file.
pub const ENDPOINT_ENV: &str = "AMAIL_DSH_ENDPOINT";

const HOOK_TIMEOUT_MS: u64 = 5000;

/// How long a wake-up waits for a starting host to publish its address.
const WAKE_WAIT: Duration = Duration::from_secs(30);

/// Where a running host can be reached; written by the wrapper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Endpoint {
    /// The wrapper process (the seat's process).
    pub pid: u32,
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    /// The host's launch token (exchanged for a session cookie).
    pub token: String,
    /// The seat's session, once created or resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub cwd: String,
}

impl Endpoint {
    pub fn read(path: &Path) -> Option<Endpoint> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        crate::persist::write_atomic(path, &serde_json::to_vec(self)?, 0o600)
    }

    pub fn alive(&self) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(self.pid as i32, 0) == 0 }
    }
}

/// The directory of the dsh homes of a node with state directory
/// `state_dir`.
pub fn home_in(state_dir: &Path) -> PathBuf {
    state_dir.join("dsh-home")
}

/// The dsh home of `seat`: each seat has its own, so that a seat cannot
/// read or change another seat's session logs or the secret that signs
/// another host's browser sessions.
pub fn seat_home_in(state_dir: &Path, seat: &str) -> PathBuf {
    home_in(state_dir).join(seat)
}

/// Read `NAME=value` lines (blank lines, `#` comments and a leading
/// `export ` are allowed; one pair of surrounding quotes is removed).
pub fn read_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            bail!("{}:{}: expected NAME=value", path.display(), i + 1);
        };
        let k = k.trim();
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            bail!("{}:{}: bad variable name", path.display(), i + 1);
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|x| x.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')))
            .unwrap_or(v);
        out.insert(k.to_string(), v.to_string());
    }
    Ok(out)
}

/// The directory that holds the `dsh` installation (the parent of the
/// `node_modules` that contains it), found from the launcher's real path.
pub fn dsh_install_root(dsh_bin: &str) -> Option<PathBuf> {
    let bin = if dsh_bin.contains('/') {
        PathBuf::from(dsh_bin)
    } else {
        std::env::var_os("PATH")?
            .to_string_lossy()
            .split(':')
            .map(|d| Path::new(d).join(dsh_bin))
            .find(|p| p.is_file())?
    };
    let real = std::fs::canonicalize(&bin).ok()?;
    let mut top = None;
    for a in real.ancestors() {
        if a.file_name().is_some_and(|n| n == "node_modules") {
            top = a.parent().map(Path::to_path_buf);
        }
    }
    top.or_else(|| real.parent().map(Path::to_path_buf))
}

/// dsh's file name for a session id: safe characters stay, others become
/// `~XXXX` (UTF-16 code units).
pub fn encode_segment(raw: &str) -> String {
    if raw == "." {
        return "~002E".into();
    }
    if raw == ".." {
        return "~002E~002E".into();
    }
    let mut out = String::new();
    for unit in raw.encode_utf16() {
        match char::from_u32(unit as u32) {
            Some(c) if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') => out.push(c),
            _ => out.push_str(&format!("~{unit:04X}")),
        }
    }
    out
}

/// dsh's directory name for a project (`cwd`): runs of `/`, `\` and `:`
/// become one `-`, safe characters stay, others become `~XXXX`; leading
/// `-` are dropped, the result is cut to 251 characters and wrapped in `--`.
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut sep_run = false;
    for unit in cwd.encode_utf16() {
        let c = char::from_u32(unit as u32);
        match c {
            Some('/' | '\\' | ':') => {
                if !sep_run {
                    readable.push('-');
                }
                sep_run = true;
            }
            Some(c) if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') => {
                readable.push(c);
                sep_run = false;
            }
            _ => {
                readable.push_str(&format!("~{unit:04X}"));
                sep_run = false;
            }
        }
    }
    let trimmed = readable.trim_start_matches('-');
    let body = if trimmed.is_empty() { "root" } else { trimmed };
    // Only ASCII is left, so characters are bytes.
    let body = &body[..body.len().min(251)];
    format!("--{body}--")
}

/// The JSONL log of `session` (started in `cwd`) under a dsh home.
pub fn transcript_path(home: &Path, cwd: &str, session: &str) -> PathBuf {
    home.join("sessions")
        .join(project_key(cwd))
        .join(encode_segment(session))
        .join("session.v4.jsonl")
}

fn request_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// A small client of a `dsh web` host's API.
pub struct Api {
    url: String,
    cookie: String,
    http: reqwest::Client,
}

impl Api {
    /// Exchange the launch token for a session cookie.
    pub async fn login(url: &str, token: &str) -> Result<Api> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        let resp = http
            .get(format!("{url}/"))
            .query(&[("token", token)])
            .send()
            .await
            .with_context(|| format!("connecting to the dsh host at {url}"))?;
        let cookie = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter_map(|v| v.split(';').next())
            .map(str::trim)
            .filter(|v| v.contains('='))
            .collect::<Vec<_>>()
            .join("; ");
        if cookie.is_empty() {
            bail!(
                "the dsh host at {url} did not accept its launch token (HTTP {})",
                resp.status()
            );
        }
        Ok(Api {
            url: url.to_string(),
            cookie,
            http,
        })
    }

    /// Call `<endpoint>` (e.g. `session/prompt`) with named arguments.
    pub async fn call(&self, endpoint: &str, args: Value) -> Result<Value> {
        let rpc_id = request_id();
        let body = json!({
            "type": "client-request",
            "rpcId": rpc_id,
            "method": endpoint,
            "payload": {"args": args},
        });
        let resp = self
            .http
            .post(format!("{}/api/{endpoint}", self.url))
            .header(reqwest::header::COOKIE, &self.cookie)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            bail!("dsh {endpoint}: HTTP {status}");
        }
        let v: Value = resp.json().await?;
        let result = &v["result"];
        if result["ok"] == Value::Bool(true) {
            return Ok(result["value"].clone());
        }
        bail!(
            "dsh {endpoint}: {} ({})",
            result["error"]["message"].as_str().unwrap_or("failed"),
            result["error"]["code"].as_str().unwrap_or("?")
        )
    }

    pub async fn create(&self, cwd: &str, session: Option<&str>) -> Result<String> {
        let mut req = json!({"cwd": cwd});
        if let Some(s) = session {
            req["sessionId"] = json!(s);
        }
        let v = self.call("session/create", json!({"request": req})).await?;
        v["sessionId"]
            .as_str()
            .map(str::to_string)
            .context("dsh session/create returned no session id")
    }

    /// Make `session` use `provider`'s `model` from its next request (dsh
    /// refuses a model it does not offer).
    pub async fn select_model(&self, session: &str, provider: &str, model: &str) -> Result<()> {
        let req = json!({"sessionId": session, "provider": provider, "model": model});
        self.call("session/selectModel", json!({"request": req}))
            .await
            .map(drop)
    }

    /// Queue `text` for `session` (it starts a turn when the session is idle).
    pub async fn prompt(&self, session: &str, text: &str) -> Result<()> {
        let req = json!({
            "requestId": request_id(),
            "sessionId": session,
            "mode": "queue",
            "content": [{"type": "text", "text": text}],
        });
        self.call("session/prompt", json!({"request": req}))
            .await
            .map(drop)
    }
}

/// A seat's dsh model `provider/model`; a bare name is a model of the
/// DeepSeek API (`deepseek-official`).
pub fn split_model(m: &str) -> (&str, &str) {
    m.split_once('/').unwrap_or(("deepseek-official", m))
}

/// The paths of a seat's jail (see [`Dsh::jail_paths`]).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Jail {
    pub rw: Vec<String>,
    pub ro: Vec<String>,
    /// Paths that do not exist and must stay so: an empty read-only
    /// directory or file is mounted there.
    pub empty_dirs: Vec<String>,
    pub empty_files: Vec<String>,
}

pub struct Dsh {
    env: AdapterEnv,
}

impl Dsh {
    pub fn new(env: AdapterEnv) -> Self {
        Dsh { env }
    }

    /// The parent of the seats' dsh homes.
    pub fn home(&self) -> PathBuf {
        home_in(&self.env.state_dir)
    }

    /// The dsh home of `seat`.
    pub fn seat_home(&self, seat: &str) -> PathBuf {
        seat_home_in(&self.env.state_dir, seat)
    }

    fn run_dir(&self) -> PathBuf {
        self.env.state_dir.join("dsh").join("run")
    }

    /// The seat's endpoint file, in a directory of its own (the only one
    /// of the endpoint directories its jail shows).
    pub fn endpoint_path(&self, seat: &str) -> PathBuf {
        self.run_dir().join(seat).join("endpoint.json")
    }

    fn seat_patch_path(&self, seat: &str) -> PathBuf {
        self.env
            .state_dir
            .join("dsh")
            .join("seats")
            .join(format!("{seat}.patch.yml"))
    }

    /// The seat's `amail` profile (server URL and token).
    fn seat_profile_path(&self, seat: &str) -> PathBuf {
        self.env
            .state_dir
            .join("dsh")
            .join("seats")
            .join(format!("{seat}.toml"))
    }

    /// What an unattended seat's jail exposes, and a notice for the person
    /// when the seat's git access cannot be given in its working directory.
    ///
    /// Writable: the working directory and the dsh home (session logs). Git
    /// follows the seat's `git_access`, with the same rules as for Codex
    /// seats (`super::codex::seat_git_rules`): with `worktree` (the
    /// default) a seat in a linked worktree may write the repository's
    /// objects, refs and branch reflogs and its worktree's own git
    /// directory, while the root of the repository's git directory (config,
    /// hooks) stays read-only; with `full` (the default for the root of a
    /// staged-merge team, which merges in the main checkout) the main
    /// checkout and the whole git directory are writable except the entries
    /// git takes commands from; with `off` nothing. Read-only entries that
    /// do not exist are covered by empty read-only placeholders, so they
    /// cannot be created. The working directory's own `.git` stays
    /// read-only unless a rule makes it writable. Read-only: the `amail`
    /// and `dsh` installations, the seat's profile and patch, the endpoint
    /// files, and the node socket's directory. Everything else under the
    /// home directory is hidden; the rest of the system is read-only.
    pub fn jail_paths(
        &self,
        cwd: &str,
        seat: &str,
        access: crate::wire::GitAccess,
    ) -> Result<(Jail, Option<String>)> {
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        let cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| PathBuf::from(cwd));
        let mut j = Jail {
            rw: vec![s(cwd.clone()), s(self.seat_home(seat))],
            ..Jail::default()
        };
        let dirs = super::codex::git_dirs(&cwd);
        let (rules, notice) = super::codex::seat_git_rules(
            &cwd,
            dirs.as_ref(),
            seat,
            access,
            "dsh jail",
            "the DeepSeek Harness section of docs/harnesses.md",
        )?;
        // The repository's git directories are visible as they are (config,
        // HEAD, packed-refs, ...), read-only unless a rule below grants more:
        // the jail empties the home directory and `/tmp`, so otherwise only
        // the granted subdirectories would appear.
        if let Some(d) = &dirs {
            for g in [&d.common, &d.git_dir] {
                if !rules
                    .iter()
                    .any(|(p, m)| *m == "write" && Path::new(p) == g.as_path())
                {
                    j.ro.push(s(g.clone()));
                }
            }
        }
        let dot_git = cwd.join(".git");
        if dot_git.exists()
            && !rules
                .iter()
                .any(|(p, m)| *m == "write" && Path::new(p) == dot_git)
        {
            j.ro.push(s(dot_git));
        }
        for (path, mode) in rules {
            let p = Path::new(&path);
            match mode {
                "write" => j.rw.push(path),
                _ if p.exists() => j.ro.push(path),
                // Entries git reads as directories, and files.
                _ if matches!(
                    p.file_name().and_then(|n| n.to_str()),
                    Some("hooks" | "modules" | "worktrees" | "info")
                ) =>
                {
                    j.empty_dirs.push(path)
                }
                _ => j.empty_files.push(path),
            }
        }
        j.ro.extend(self.env.amail_dir());
        j.ro.extend(dsh_install_root(&self.env.dsh_bin).map(s));
        j.ro.push(s(self.seat_profile_path(seat)));
        j.ro.push(s(self.seat_patch_path(seat)));
        j.ro.extend(
            self.endpoint_path(seat)
                .parent()
                .map(|p| s(p.to_path_buf())),
        );
        j.ro.extend(self.env.node_socket.parent().map(|p| s(p.to_path_buf())));
        for v in [&mut j.rw, &mut j.ro, &mut j.empty_dirs, &mut j.empty_files] {
            v.sort();
            v.dedup();
        }
        Ok((j, notice))
    }

    fn hooks_path(&self, seat: &str) -> PathBuf {
        self.seat_home(seat).join("amail-hooks.json")
    }

    pub fn hook_command(&self) -> String {
        format!(
            "{} hook dsh",
            shell_quote(&self.env.amail_exe.to_string_lossy())
        )
    }

    /// The bridge's hook configuration (Codex format).
    pub fn hooks(&self) -> Value {
        let cmd = self.hook_command();
        let hooks: serde_json::Map<String, Value> = EVENTS
            .iter()
            .map(|e| {
                (
                    e.to_string(),
                    json!([{"hooks": [{"type": "command", "command": cmd, "timeout": HOOK_TIMEOUT_MS / 1000}]}]),
                )
            })
            .collect();
        json!({ "hooks": hooks })
    }

    /// The home-level patch: plain JSONL logs (readable by the mirror) and
    /// the hook bridge. YAML 1.2 accepts JSON, so it is written as JSON.
    pub fn home_patch(&self, seat: &str) -> Value {
        json!([
            {"id": "session-persistence-jsonl", "config": {
                "root": self.seat_home(seat).join("sessions").to_string_lossy(),
                "compression": "none",
            }},
            {"insert": [{
                "id": "amail-hooks",
                "name": "@deepseek-ai/dsh-hooks-codex",
                "config": {
                    "configPath": self.hooks_path(seat).to_string_lossy(),
                    "model": "dsh",
                    "defaultTimeoutMs": HOOK_TIMEOUT_MS,
                },
            }]},
        ])
    }

    /// The seat's overlay: system prompt (mail instructions and the seat's
    /// role) and model.
    pub fn seat_patch(info: &LaunchInfo) -> Value {
        // `{{name}}` is a template variable in these fields.
        let esc = |s: &str| s.replace("{{", "{ {").replace("}}", "} }");
        let suffix = format!(
            "Your working directory is {{{{cwd}}}}.\n\n{}\n# Your seat\n\n{}\n",
            esc(crate::inject::MAIL_INSTRUCTIONS),
            esc(&crate::inject::seat_intro(info))
        );
        let mut patch = vec![json!({"id": "system-prompt", "config": {
            "personaPrefix": "You are a coding agent powered by the {{model}} model.",
            "personaSuffix": suffix,
        }})];
        if let Some(m) = info.model.as_deref().filter(|m| !m.is_empty()) {
            let (provider, model) = split_model(m);
            patch.push(json!({"id": "agent-default-model", "config": {
                "provider": provider, "model": model,
            }}));
        }
        Value::Array(patch)
    }

    /// The endpoint a wake target names: an endpoint file, or a session id
    /// (a resumed seat before its first hook), looked up among the
    /// endpoint files of running hosts.
    fn resolve(&self, target: &str) -> Option<(Endpoint, String)> {
        let path = Path::new(target);
        if path.is_absolute() {
            let ep = Endpoint::read(path)?;
            let session = ep.session.clone()?;
            return Some((ep, session));
        }
        let dir = std::fs::read_dir(self.run_dir()).ok()?;
        dir.filter_map(|e| Endpoint::read(&e.ok()?.path().join("endpoint.json")))
            .find(|ep| ep.session.as_deref() == Some(target) && ep.alive())
            .map(|ep| (ep, target.to_string()))
    }
}

#[async_trait::async_trait]
impl Adapter for Dsh {
    fn harness(&self) -> Harness {
        Harness::Dsh
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
            INTERRUPT => Event::Interrupt,
            TURN_FAILED => Event::TurnFailed {
                reason: str_field(input, "reason")
                    .unwrap_or("the turn ended with an error")
                    .to_string(),
            },
            _ => return None,
        };
        let session_id = str_field(input, "session_id").map(str::to_string);
        // The bridge reports no transcript path; the log's place follows
        // from the session's directory and id, under the seat's dsh home
        // (see `transcript`): this is the path relative to that home.
        let transcript_path = match (&session_id, str_field(input, "cwd")) {
            (Some(s), Some(cwd)) if !cwd.is_empty() => Some(
                transcript_path(Path::new(""), cwd, s)
                    .to_string_lossy()
                    .into_owned(),
            ),
            _ => None,
        };
        Some(Hook {
            name,
            event,
            session_id,
            transcript_path,
            facts: HookFacts::from_input(input),
        })
    }

    fn seat_from_env(&self, env: &BTreeMap<String, String>) -> Option<String> {
        env.get("AMAIL_SEAT").filter(|s| !s.is_empty()).cloned()
    }

    fn transcript(&self, hook: &Hook, seat: &str) -> Option<String> {
        let rel = hook.transcript_path.as_deref()?;
        Some(
            self.seat_home(seat)
                .join(rel)
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn accepts(&self, env: &BTreeMap<String, String>, hook: &Hook) -> bool {
        // A host serves every session a person opens in its GUI (and its
        // subagents); only the seat's own session is connected to mail.
        let Some(ep) = env
            .get(ENDPOINT_ENV)
            .and_then(|p| Endpoint::read(Path::new(p)))
        else {
            return true;
        };
        match (&ep.session, &hook.session_id) {
            (Some(mine), Some(theirs)) => mine == theirs,
            _ => true,
        }
    }

    fn session_hint(&self, env: &BTreeMap<String, String>, _hook: &Hook) -> SessionHint {
        let path = env.get(ENDPOINT_ENV).filter(|p| !p.is_empty()).cloned();
        let pid = path
            .as_deref()
            .and_then(|p| Endpoint::read(Path::new(p)))
            .map(|ep| ep.pid);
        SessionHint {
            pid,
            wake_target: path,
        }
    }

    fn session_start_state(&self, _source: &str) -> Option<SeatState> {
        // SessionStart fires when the session is created or resumed, before
        // any prompt; a prompt then fires UserPromptSubmit.
        Some(SeatState::Idle)
    }

    fn inject_budget(&self) -> usize {
        9_000
    }

    fn claim_limit(&self) -> u32 {
        8
    }

    fn target_alive(&self, target: &str) -> bool {
        self.resolve(target).is_some_and(|(ep, _)| ep.alive())
    }

    async fn wake(&self, target: &str, text: &str) -> Result<()> {
        // A host that is starting (a cold start) publishes its address and
        // session a few seconds after launch.
        let until = tokio::time::Instant::now() + WAKE_WAIT;
        let (ep, session) = loop {
            match self.resolve(target) {
                Some((ep, s)) if ep.alive() => break (ep, s),
                _ if tokio::time::Instant::now() >= until => {
                    bail!("no running dsh host for {target}")
                }
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        };
        let api = Api::login(&ep.url, &ep.token).await?;
        api.prompt(&session, text).await
    }

    async fn prepare(&self) -> Result<()> {
        for dir in [self.home(), self.run_dir()] {
            std::fs::create_dir_all(&dir)?;
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        }
        Ok(())
    }

    async fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan> {
        let info = ctx.info;
        let patch = self.seat_patch_path(&info.seat);
        if let Some(dir) = patch.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = format!(
            "# Managed by amail.\n{}\n",
            serde_json::to_string_pretty(&Self::seat_patch(info))?
        );
        crate::persist::write_atomic(&patch, text.as_bytes(), 0o600)?;
        // The seat's own dsh home: plain JSONL logs and the hook bridge.
        let home = self.seat_home(&info.seat);
        let run = self.endpoint_path(&info.seat);
        for dir in [Some(home.as_path()), run.parent()].into_iter().flatten() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        }
        crate::persist::write_atomic(
            &self.hooks_path(&info.seat),
            serde_json::to_string_pretty(&self.hooks())?.as_bytes(),
            0o600,
        )?;
        let text = format!(
            "# Managed by amail.\n{}\n",
            serde_json::to_string_pretty(&self.home_patch(&info.seat))?
        );
        crate::persist::write_atomic(&home.join("cordis.patch.yml"), text.as_bytes(), 0o600)?;
        // dsh removes variables with credential-shaped names (`*TOKEN*`,
        // `*KEY*`, ...) from the environment of the agent's commands, so
        // `AMAIL_TOKEN` would not reach them; a profile file does.
        let profile = self.seat_profile_path(&info.seat);
        let mut t = toml::Table::new();
        t.insert("url".into(), self.env.server_url.clone().into());
        t.insert("token".into(), info.token.clone().into());
        crate::persist::write_atomic(&profile, toml::to_string(&t)?.as_bytes(), 0o600)?;
        let endpoint = self.endpoint_path(&info.seat);
        let mut args = vec![
            "dsh-host".to_string(),
            "--seat".into(),
            info.seat.clone(),
            "--endpoint".into(),
            endpoint.to_string_lossy().into_owned(),
        ];
        let unattended = ctx.permission_mode == PermissionMode::Unattended;
        let mut notice = None;
        if unattended && !ctx.dsh_jail {
            notice = Some(format!(
                "seat {}: dsh_jail is off, so this unattended dsh seat runs without the \
                 jail: its agent can read every file this user can read (saved logins and \
                 keys included) and write every file this user can write.",
                info.seat
            ));
        }
        if unattended && ctx.dsh_jail {
            // The whole host runs in a jail (see `crate::dsh_host`): dsh's
            // own sandbox limits writes only, and its file tools run inside
            // the host without any sandbox.
            args.push("--jail".into());
            let (j, git_notice) = self.jail_paths(ctx.cwd, &info.seat, ctx.git_access)?;
            notice = git_notice;
            for (flag, list) in [
                ("--rw", j.rw),
                ("--ro", j.ro),
                ("--ro-empty-dir", j.empty_dirs),
                ("--ro-empty-file", j.empty_files),
            ] {
                for p in list {
                    args.extend([flag.to_string(), p]);
                }
            }
        }
        if let Some(m) = info.model.as_deref().filter(|m| !m.is_empty()) {
            args.extend(["--model".into(), m.to_string()]);
        }
        match ctx.resume {
            Some(id) => {
                args.extend(["--resume".into(), id.to_string()]);
                // The host sends this prompt to the loaded session.
                if ctx.adopted {
                    args.extend(["--prompt".into(), adopted_prompt(info)]);
                }
            }
            None => {
                let start = if ctx.context_lost {
                    CONTEXT_LOST
                } else {
                    "[amail] This is the start of your session. `amail` is ready to use \
                     (`amail inbox`). Handle any new mail shown to you. If there is none, say \
                     that you are ready and stop; do not explore outside your working \
                     directory."
                };
                args.extend(["--prompt".into(), start.to_string()]);
            }
        }
        args.extend([
            "--".into(),
            self.env.dsh_bin.clone(),
            "web".into(),
            // A launcher flag: it must come before the app's own flags.
            "--patch".into(),
            patch.to_string_lossy().into_owned(),
            "--no-open".into(),
        ]);
        if !ctx
            .extra
            .iter()
            .any(|a| a == "--port" || a.starts_with("--port="))
        {
            args.extend(["--port".into(), "0".into()]);
        }
        args.extend(ctx.extra.iter().cloned());
        let mut env = BTreeMap::from([
            ("DSH_HOME".to_string(), home.to_string_lossy().into_owned()),
            ("AMAIL_SEAT".to_string(), info.seat.clone()),
            ("AMAIL_URL".to_string(), self.env.server_url.clone()),
            ("AMAIL_TOKEN".to_string(), info.token.clone()),
            (
                "AMAIL_PROFILE".to_string(),
                profile.to_string_lossy().into_owned(),
            ),
            (
                crate::paths::SOCK_ENV.to_string(),
                self.env.node_socket.to_string_lossy().into_owned(),
            ),
            (
                ENDPOINT_ENV.to_string(),
                endpoint.to_string_lossy().into_owned(),
            ),
        ]);
        // The model provider's credentials, for this host only.
        for (k, v) in &self.env.dsh_env {
            env.insert(k.clone(), v.clone());
        }
        if unattended {
            // Inside the jail; dsh's own sandbox would only repeat it, and
            // without the repository's git directories (a worktree commits
            // into the common one).
            env.insert("DSH_PERMISSION_MODE".into(), "danger-full-access".into());
        }
        Ok(LaunchPlan {
            program: self.env.amail_exe.to_string_lossy().into_owned(),
            args,
            env,
            unset_env: vec!["DSH_PERMISSION_MODE".into()],
            path_prepend: self.env.amail_dir().into_iter().collect(),
            notice,
        })
    }

    fn resume_is_lazy(&self) -> bool {
        // A resumed session's wake target is its id; the host publishes it.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dsh(dir: &Path) -> Dsh {
        Dsh::new(AdapterEnv {
            state_dir: dir.to_path_buf(),
            amail_exe: PathBuf::from("/opt/amail/bin/amail"),
            node_socket: dir.join("node.sock"),
            server_url: "http://127.0.0.1:1".into(),
            claude_bin: "claude".into(),
            codex_bin: "codex".into(),
            codex_sandbox: None,
            dsh_bin: "dsh".into(),
            dsh_env: BTreeMap::from([("DEEPSEEK_API_KEY".to_string(), "k".to_string())]),
            codex_auth: dir.join("auth.json"),
            codex_pass: String::new(),
        })
    }

    #[test]
    fn project_keys() {
        assert_eq!(project_key("/home/u/work/w2"), "--home-u-work-w2--");
        assert_eq!(project_key("/a//b/"), "--a-b---");
        assert_eq!(project_key("/a b/é~"), "--a~0020b-~00E9~007E--");
        assert_eq!(project_key("/"), "--root--");
        assert_eq!(project_key(&format!("/{}", "x".repeat(300))).len(), 255);
        assert_eq!(encode_segment("session-1a2b"), "session-1a2b");
        assert_eq!(encode_segment("a/b"), "a~002Fb");
        assert_eq!(encode_segment(".."), "~002E~002E");
    }

    #[test]
    fn parses_bridge_events() {
        let d = dsh(Path::new("/s"));
        let h = d
            .parse(
                &json!({"hook_event_name": "SessionStart", "source": "resume",
                "session_id": "session-1", "cwd": "/w/a", "transcript_path": null,
                "model": "dsh", "permission_mode": "default"}),
            )
            .unwrap();
        assert_eq!(
            h.event,
            Event::SessionStart {
                source: "resume".into()
            }
        );
        assert_eq!(
            h.transcript_path.as_deref(),
            Some("sessions/--w-a--/session-1/session.v4.jsonl")
        );
        // Under the seat's own dsh home.
        assert_eq!(
            d.transcript(&h, "w").as_deref(),
            Some("/s/dsh-home/w/sessions/--w-a--/session-1/session.v4.jsonl")
        );
        for (name, ev) in [
            ("PostToolUse", Event::PostToolUse),
            ("Stop", Event::Stop),
            ("Interrupt", Event::Interrupt),
        ] {
            let h = d
                .parse(&json!({"hook_event_name": name, "session_id": "s"}))
                .unwrap();
            assert_eq!(h.event, ev);
            assert_eq!(h.transcript_path, None);
        }
        assert!(d.parse(&json!({"hook_event_name": "PreToolUse"})).is_none());
        let h = d
            .parse(&json!({"hook_event_name": "UserPromptSubmit", "prompt": "hi", "session_id": "s", "turn_id": "1"}))
            .unwrap();
        assert_eq!(
            h.event,
            Event::UserPromptSubmit {
                prompt: "hi".into()
            }
        );
    }

    #[test]
    fn endpoint_filter_and_hint() {
        let dir = tempfile::tempdir().unwrap();
        let d = dsh(dir.path());
        let path = dir.path().join("w.json");
        let env = BTreeMap::from([
            ("AMAIL_SEAT".to_string(), "w".to_string()),
            (
                ENDPOINT_ENV.to_string(),
                path.to_string_lossy().into_owned(),
            ),
        ]);
        let hook = |sid: &str| Hook {
            name: "Stop".into(),
            event: Event::Stop,
            session_id: Some(sid.into()),
            transcript_path: None,
            facts: Default::default(),
        };
        // No endpoint file yet (the session is being created): accepted.
        assert!(d.accepts(&env, &hook("s1")));
        let ep = Endpoint {
            pid: std::process::id(),
            url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            session: Some("s1".into()),
            cwd: "/w".into(),
        };
        ep.write(&path).unwrap();
        assert!(d.accepts(&env, &hook("s1")));
        assert!(!d.accepts(&env, &hook("s2")));
        let hint = d.session_hint(&env, &hook("s1"));
        assert_eq!(hint.pid, Some(std::process::id()));
        assert_eq!(hint.wake_target.as_deref(), path.to_str());
        assert_eq!(d.seat_from_env(&env).as_deref(), Some("w"));
        assert!(d.target_alive(path.to_str().unwrap()));
    }

    #[test]
    fn resolves_session_ids_among_running_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let d = dsh(dir.path());
        std::fs::create_dir_all(d.endpoint_path("w").parent().unwrap()).unwrap();
        let ep = Endpoint {
            pid: std::process::id(),
            url: "http://127.0.0.1:9".into(),
            token: "t".into(),
            session: Some("session-x".into()),
            cwd: "/w".into(),
        };
        ep.write(&d.endpoint_path("w")).unwrap();
        assert_eq!(d.resolve("session-x").unwrap().1, "session-x");
        assert!(d.resolve("session-y").is_none());
        assert!(d.target_alive("session-x"));
    }

    #[test]
    fn env_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("e");
        std::fs::write(&p, "# c\n\nexport A_KEY=\"x y\"\nB=plain\nC='q'\n").unwrap();
        let m = read_env_file(&p).unwrap();
        assert_eq!(m["A_KEY"], "x y");
        assert_eq!(m["B"], "plain");
        assert_eq!(m["C"], "q");
        std::fs::write(&p, "not a pair\n").unwrap();
        assert!(read_env_file(&p).is_err());
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn bwrap_ok() -> bool {
        std::process::Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "--", "true"])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Run `script` in the jail `j`; returns its stdout.
    fn in_jail(j: &Jail, script: &str) -> String {
        let empty = tempfile::NamedTempFile::new().unwrap();
        let args = crate::dsh_host::jail_args(
            "",
            "",
            &j.rw,
            &j.ro,
            &[],
            &j.empty_dirs,
            &j.empty_files,
            &empty.path().to_string_lossy(),
        );
        let out = std::process::Command::new("bwrap")
            .args(&args)
            .args(["--", "sh", "-c", script])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    const ID: &str = "-c user.name=t -c user.email=t@t";

    /// A repository with a commit on `main` and a linked worktree `wt` on
    /// branch `w`; returns (root, repo, worktree).
    fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "i",
            ],
        );
        let wt = root.join("wt");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "w", wt.to_str().unwrap()],
        );
        (dir, repo, wt)
    }

    /// `worktree` (the default) in a linked worktree: objects, refs, branch
    /// reflogs and the worktree's own git directory are writable; the root
    /// of the repository's git directory (config, hooks, commondir) and the
    /// worktree's `.git` file are not. Committing, tagging and rebasing
    /// work; deleting a ref does not (as for Codex seats).
    #[test]
    fn jail_git_worktree() {
        let (dir, repo, wt) = repo_with_worktree();
        let d = dsh(&dir.path().canonicalize().unwrap().join("state"));
        let (j, notice) = d
            .jail_paths(wt.to_str().unwrap(), "w", crate::wire::GitAccess::Worktree)
            .unwrap();
        assert_eq!(notice, None);
        let c = repo.join(".git");
        let l = c.join("worktrees").join("wt");
        let p = |x: &Path| x.to_string_lossy().into_owned();
        for w in [
            c.join("objects"),
            c.join("refs"),
            c.join("logs/refs"),
            l.clone(),
        ] {
            assert!(j.rw.contains(&p(&w)), "{w:?} {j:?}");
        }
        assert!(!j.rw.contains(&p(&c)), "{j:?}");
        assert!(j.ro.contains(&p(&l.join("commondir"))), "{j:?}");
        assert!(j.ro.contains(&p(&wt.join(".git"))), "{j:?}");
        if !bwrap_ok() {
            eprintln!("bwrap cannot run here: jail check skipped");
            return;
        }
        let out = in_jail(
            &j,
            &format!(
                "cd {wt}; \
                 (echo x >> {c}/config) 2>/dev/null && echo config-writable; \
                 (mkdir -p {c}/hooks && touch {c}/hooks/pre-commit) 2>/dev/null && echo hooks-writable; \
                 (echo x > {l}/commondir) 2>/dev/null && echo commondir-writable; \
                 (echo 'gitdir: /tmp/x' > {wt}/.git) 2>/dev/null && echo dotgit-writable; \
                 git {ID} commit -q --allow-empty -m j && echo commit-ok; \
                 git tag t1 && echo tag-ok; \
                 git {ID} rebase -q main 2>/dev/null && echo rebase-ok; \
                 git tag -d t1 >/dev/null 2>&1 || echo no-ref-delete",
                wt = wt.display(),
                c = c.display(),
                l = l.display(),
            ),
        );
        // As for Codex seats: deleting a branch or tag rewrites
        // `packed-refs`, which stays read-only.
        assert_eq!(out, "commit-ok\ntag-ok\nrebase-ok\nno-ref-delete");
    }

    /// `full` for a maintainer in the main checkout: commits and merges
    /// there; config and hooks stay read-only.
    #[test]
    fn jail_git_full() {
        let (dir, repo, _wt) = repo_with_worktree();
        let d = dsh(&dir.path().canonicalize().unwrap().join("state"));
        let (j, notice) = d
            .jail_paths(repo.to_str().unwrap(), "root", crate::wire::GitAccess::Full)
            .unwrap();
        assert_eq!(notice, None);
        let c = repo.join(".git");
        let p = |x: &Path| x.to_string_lossy().into_owned();
        assert!(j.rw.contains(&p(&c)), "{j:?}");
        assert!(j.ro.contains(&p(&c.join("config"))), "{j:?}");
        if !bwrap_ok() {
            return;
        }
        let out = in_jail(
            &j,
            &format!(
                "cd {r}; \
                 (echo x >> {c}/config) 2>/dev/null && echo config-writable; \
                 (touch {c}/hooks/pre-commit) 2>/dev/null && echo hooks-writable; \
                 git {ID} merge -q --no-ff -m m w && echo merge-ok; \
                 git {ID} commit -q --allow-empty -m j && echo commit-ok",
                r = repo.display(),
                c = c.display(),
            ),
        );
        assert_eq!(out, "merge-ok\ncommit-ok");
    }

    /// `off`, and `worktree` in a main checkout (with a notice): git stays
    /// read-only.
    #[test]
    fn jail_git_off_and_main_checkout() {
        let (dir, repo, wt) = repo_with_worktree();
        let d = dsh(&dir.path().canonicalize().unwrap().join("state"));
        let (off, n) = d
            .jail_paths(wt.to_str().unwrap(), "w", crate::wire::GitAccess::Off)
            .unwrap();
        assert_eq!(n, None);
        let (main, n) = d
            .jail_paths(
                repo.to_str().unwrap(),
                "m",
                crate::wire::GitAccess::Worktree,
            )
            .unwrap();
        assert!(n.unwrap().contains("main checkout"));
        if !bwrap_ok() {
            return;
        }
        for (j, cwd) in [(&off, &wt), (&main, &repo)] {
            let out = in_jail(
                j,
                &format!(
                    "cd {}; git {ID} commit -q --allow-empty -m j 2>/dev/null && echo commit-ok; \
                     touch file && echo workdir-ok",
                    cwd.display()
                ),
            );
            assert_eq!(out, "workdir-ok", "{cwd:?}");
        }
    }

    #[test]
    fn redirected_git_dir_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join(".git").join("commondir"), ".\n").unwrap();
        let d = dsh(&root.join("state"));
        let e = d
            .jail_paths(repo.to_str().unwrap(), "w", crate::wire::GitAccess::Full)
            .unwrap_err();
        assert!(e.to_string().contains("commondir"), "{e}");
    }

    #[test]
    fn config_files() {
        let d = dsh(Path::new("/s"));
        let hooks = d.hooks();
        assert_eq!(
            hooks["hooks"]["Stop"][0]["hooks"][0]["command"],
            "/opt/amail/bin/amail hook dsh"
        );
        assert_eq!(hooks["hooks"].as_object().unwrap().len(), EVENTS.len());
        let p = d.home_patch("w");
        assert_eq!(p[0]["config"]["compression"], "none");
        assert_eq!(p[0]["config"]["root"], "/s/dsh-home/w/sessions");
        assert_eq!(
            p[1]["insert"][0]["config"]["configPath"],
            "/s/dsh-home/w/amail-hooks.json"
        );
    }
}
