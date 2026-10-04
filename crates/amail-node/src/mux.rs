//! Terminal multiplexing: where supervised sessions run.
//!
//! [`Mux`] is the small interface the supervisor uses. [`Zellij`] is the
//! real implementation: one zellij session per team group, one tab per seat
//! holding one pane named after the seat. [`ProcessMux`] runs "panes" as
//! plain child processes, for tests.
//!
//! # Pane width without a human attached
//!
//! A zellij session created in the background (`attach --create-background`)
//! has no client, and zellij then lays out its screen at a fallback size of
//! about 50x50: every pane gets at most 50 columns, and less once split.
//! Harness TUIs render (and wrap their transcripts) to that width. zellij
//! sizes a session to its attached clients (the smallest one wins) and keeps
//! the last size when the last client leaves.
//!
//! So the node keeps one *headless client* attached to every session it
//! manages: a `zellij attach` running on a pseudo-terminal the node owns,
//! with a fixed window size (200x60 by default) set on the pty before zellij
//! starts. Its output is read and discarded. The session therefore always
//! has a client of known size: panes are 200 columns wide while no one is
//! attached. When a person attaches, zellij shrinks the session to the
//! smaller of the two terminals, and restores 200 columns when they leave.
//! The headless client also creates the session (so the session's server
//! starts with the environment the node chose, not the caller's).
//!
//! A connected client is also needed for zellij 0.45 to apply layout
//! changes reliably: without one, `new-tab` starts its command but the pane
//! never appears in the layout.
//!
//! Every pane has a tab of its own, so it always gets the full width. (Not
//! several panes per tab: tiling would split the width, and stacked panes
//! are fragile in zellij 0.45: once the focused pane of a stack is closed,
//! panes added to the stack never appear, and moving the focus can crash
//! the session's server.) A pane whose command exits is kept ("held") with
//! its last output; the supervisor restarts the seat in the same pane
//! (zellij re-runs a held pane's command on Enter), so a seat keeps its
//! place. A re-run command starts on a 0x0 pty until the pane prints
//! something; `amail run` prints a line first and waits for the size.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What to run in a seat's pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpec {
    /// Pane name: the seat.
    pub name: String,
    pub cwd: PathBuf,
    pub argv: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub id: u32,
    pub name: String,
    pub tab: String,
    pub exited: bool,
    pub exit_status: Option<i32>,
    pub columns: u32,
}

#[async_trait::async_trait]
pub trait Mux: Send + Sync + 'static {
    /// Open a pane for `spec` in tab `tab` of `session`, creating the
    /// session and the tab as needed. Returns the pane id.
    async fn open(&self, session: &str, tab: &str, spec: &PaneSpec) -> Result<u32>;

    /// The panes of `session`; `None` if the session does not exist.
    async fn panes(&self, session: &str) -> Result<Option<Vec<PaneInfo>>>;

    /// Run an exited pane's command again, in place.
    async fn rerun(&self, session: &str, pane: u32) -> Result<()>;

    /// The visible text of a pane.
    async fn screen(&self, session: &str, pane: u32) -> Result<String>;

    /// Paste `text` into a pane (as a terminal paste: bracketed if the
    /// program asked for it). No Enter.
    async fn paste(&self, session: &str, pane: u32, text: &str) -> Result<()>;

    /// Press Enter in a pane.
    async fn enter(&self, session: &str, pane: u32) -> Result<()>;

    /// End a session and everything in it.
    async fn kill_session(&self, session: &str) -> Result<()>;

    /// Periodic upkeep of the sessions in use (headless clients).
    async fn maintain(&self, sessions: &[String]);

    /// Commands that attach the caller's terminal to `session`, focused on
    /// `pane`: run all but the last, then exec the last.
    fn attach_commands(&self, session: &str, pane: Option<u32>) -> Vec<Vec<String>>;
}

/// Environment variables of the node that must not reach sessions.
pub fn scrub_env(cmd: &mut std::process::Command) {
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy();
        if k == "CLAUDECODE"
            || k.starts_with("CLAUDE_CODE_")
            || k == "CLAUDE_PID"
            || k.starts_with("CODEX_THREAD")
            || k.starts_with("CODEX_SANDBOX")
            || k == "CODEX_HOME"
            || k.starts_with("ZELLIJ")
            || k.starts_with("TMUX")
            || k == "AMAIL_SEAT"
            || k == "AMAIL_TOKEN"
            || k == "AMAIL_PROFILE"
        {
            cmd.env_remove(k.as_ref());
        }
    }
}

// --- zellij -------------------------------------------------------------------

/// A client process on a pseudo-terminal of fixed size, output discarded.
struct Headless {
    child: std::process::Child,
}

impl Headless {
    fn spawn(cmd: &mut std::process::Command, cols: u16, rows: u16) -> Result<Headless> {
        use std::os::unix::process::CommandExt;
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty writes two descriptors; name and termios may be null.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &ws,
            )
        };
        if rc != 0 {
            bail!("openpty: {}", std::io::Error::last_os_error());
        }
        // SAFETY: both descriptors are fresh and owned here.
        let (master, slave) = unsafe {
            use std::os::fd::FromRawFd;
            (
                std::fs::File::from_raw_fd(master),
                std::os::fd::OwnedFd::from_raw_fd(slave),
            )
        };
        let set_cloexec = |fd: libc::c_int| {
            // SAFETY: fcntl on a descriptor we own.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        };
        use std::os::fd::AsRawFd;
        set_cloexec(master.as_raw_fd());
        cmd.stdin(std::process::Stdio::from(slave.try_clone()?))
            .stdout(std::process::Stdio::from(slave.try_clone()?))
            .stderr(std::process::Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().context("starting a headless zellij client")?;
        // Drain the client's output until it exits.
        std::thread::spawn(move || {
            use std::io::Read;
            let mut m = master;
            let mut buf = [0u8; 65536];
            while matches!(m.read(&mut buf), Ok(n) if n > 0) {}
        });
        Ok(Headless { child })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The real multiplexer: zellij sessions with headless clients.
pub struct Zellij {
    bin: String,
    /// Where layout files are written.
    dir: PathBuf,
    cols: u16,
    rows: u16,
    clients: Mutex<HashMap<String, Headless>>,
    /// Serializes structural changes (sessions, tabs, panes).
    op: tokio::sync::Mutex<()>,
    /// Create sessions that a `zellij web` server may serve.
    web_sharing: bool,
}

fn kdl(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Zellij {
    pub fn new(bin: &str, state_dir: &Path, cols: u16, rows: u16) -> Zellij {
        Zellij {
            bin: bin.to_string(),
            dir: state_dir.join("zellij"),
            cols: cols.max(120),
            rows: rows.max(24),
            clients: Mutex::new(HashMap::new()),
            op: tokio::sync::Mutex::new(()),
            web_sharing: false,
        }
    }

    /// Let browsers attach to the sessions this node creates (through the
    /// node's own `zellij web` server). Set per session at creation, so the
    /// user's zellij configuration is not involved.
    pub fn with_web_sharing(mut self, on: bool) -> Zellij {
        self.web_sharing = on;
        self
    }

    fn command(&self) -> std::process::Command {
        let mut c = std::process::Command::new(&self.bin);
        scrub_env(&mut c);
        c.env("TERM", "xterm-256color");
        c
    }

    async fn run(&self, args: &[&str]) -> Result<String> {
        let mut c = tokio::process::Command::from(self.command());
        c.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let out = tokio::time::timeout(Duration::from_secs(20), c.output())
            .await
            .with_context(|| format!("zellij {} timed out", args.join(" ")))?
            .with_context(|| format!("running {}", self.bin))?;
        if !out.status.success() {
            bail!(
                "zellij {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    async fn session_alive(&self, session: &str) -> Result<bool> {
        let out = self.run(&["list-sessions", "--no-formatting"]).await;
        let out = match out {
            Ok(o) => o,
            // No server at all: zellij reports "No active zellij sessions found."
            Err(_) => return Ok(false),
        };
        Ok(out
            .lines()
            .any(|l| l.split_whitespace().next() == Some(session) && !l.contains("EXITED")))
    }

    fn layout_text(&self, tab: &str, spec: &PaneSpec) -> String {
        let (cmd, args) = spec
            .argv
            .split_first()
            .map(|(c, a)| (c.clone(), a))
            .unwrap_or_default();
        let args: Vec<String> = args.iter().map(|a| kdl(a)).collect();
        format!(
            "layout {{\n    tab name={tab} {{\n        pane size=1 borderless=true {{\n            \
             plugin location=\"zellij:tab-bar\"\n        }}\n        \
             pane name={name} command={cmd} cwd={cwd} {{\n            args {args}\n        }}\n        \
             pane size=2 borderless=true {{\n            plugin location=\"zellij:status-bar\"\n        \
             }}\n    }}\n}}\n",
            tab = kdl(tab),
            name = kdl(&spec.name),
            cmd = kdl(&cmd),
            cwd = kdl(&spec.cwd.to_string_lossy()),
            args = args.join(" "),
        )
    }

    fn write_layout(&self, session: &str, tab: &str, spec: &PaneSpec) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{session}.{tab}.kdl"));
        crate::persist::write_atomic(&path, self.layout_text(tab, spec).as_bytes(), 0o600)?;
        Ok(path)
    }

    /// Start (or replace) the headless client of `session`; with `layout`,
    /// it creates the session.
    fn spawn_client(&self, session: &str, layout: Option<&Path>) -> Result<()> {
        let mut c = self.command();
        if let Some(l) = layout {
            c.arg("--layout").arg(l);
        }
        c.args(["attach", "--create", session, "options"])
            .args(["--session-serialization", "false"])
            .args(["--show-startup-tips", "false"])
            .args(["--show-release-notes", "false"]);
        if self.web_sharing {
            c.args(["--web-sharing", "on"]);
        }
        let h = Headless::spawn(&mut c, self.cols, self.rows)?;
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), h);
        Ok(())
    }

    fn client_running(&self, session: &str) -> bool {
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        let running = clients.get_mut(session).map(|h| h.running());
        if running == Some(false) {
            clients.remove(session);
        }
        running == Some(true)
    }

    /// Wait until the session has a client (layout changes need one).
    async fn wait_client(&self, session: &str) -> Result<()> {
        let t0 = Instant::now();
        loop {
            if let Ok(out) = self
                .run(&["--session", session, "action", "list-clients"])
                .await
                && out.lines().skip(1).any(|l| !l.trim().is_empty())
            {
                return Ok(());
            }
            if t0.elapsed() > Duration::from_secs(15) {
                bail!("zellij session {session} has no client after 15 s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn has_client(&self, session: &str) -> bool {
        matches!(
            self.run(&["--session", session, "action", "list-clients"]).await,
            Ok(out) if out.lines().skip(1).any(|l| !l.trim().is_empty())
        )
    }

    /// Make sure our headless client is attached. One that zellij no longer
    /// lists (a client can lose its focus when panes are closed under it)
    /// is replaced.
    async fn ensure_client(&self, session: &str) -> Result<()> {
        if self.client_running(session) {
            if self.has_client(session).await {
                return Ok(());
            }
            if let Some(mut h) = self
                .clients
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(session)
            {
                tracing::info!("replacing the headless client of zellij session {session}");
                h.kill();
            }
        }
        self.spawn_client(session, None)?;
        self.wait_client(session).await
    }

    async fn find_pane(&self, session: &str, tab: &str, name: &str) -> Result<u32> {
        let t0 = Instant::now();
        loop {
            if let Some(panes) = self.panes(session).await? {
                let id = panes
                    .iter()
                    .filter(|p| p.name == name && p.tab == tab && !p.exited)
                    .map(|p| p.id)
                    .max();
                if let Some(id) = id {
                    return Ok(id);
                }
            }
            if t0.elapsed() > Duration::from_secs(15) {
                bail!("pane {name} did not appear in zellij session {session}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[async_trait::async_trait]
impl Mux for Zellij {
    async fn open(&self, session: &str, tab: &str, spec: &PaneSpec) -> Result<u32> {
        let _g = self.op.lock().await;
        if !self.session_alive(session).await? {
            // A dead (serialized) session of that name would be resurrected.
            let _ = self.run(&["delete-session", "--force", session]).await;
            let layout = self.write_layout(session, tab, spec)?;
            self.spawn_client(session, Some(&layout))?;
            self.wait_client(session).await?;
            return self.find_pane(session, tab, &spec.name).await;
        }
        self.ensure_client(session).await?;
        // The seat's tab may still be there (e.g. after a node restart).
        if let Some(p) = self
            .panes(session)
            .await?
            .into_iter()
            .flatten()
            .find(|p| p.tab == tab && p.name == spec.name)
        {
            if !p.exited {
                bail!("tab {tab} of {session} already runs pane terminal_{}", p.id);
            }
            let id = format!("terminal_{}", p.id);
            self.run(&[
                "--session",
                session,
                "action",
                "send-keys",
                "--pane-id",
                &id,
                "Enter",
            ])
            .await?;
            return Ok(p.id);
        }
        let layout = self.write_layout(session, tab, spec)?;
        let l = layout.to_string_lossy().into_owned();
        self.run(&[
            "--session",
            session,
            "action",
            "new-tab",
            "--name",
            tab,
            "--layout",
            &l,
        ])
        .await?;
        self.find_pane(session, tab, &spec.name).await
    }

    async fn panes(&self, session: &str) -> Result<Option<Vec<PaneInfo>>> {
        if !self.session_alive(session).await? {
            return Ok(None);
        }
        let out = self
            .run(&[
                "--session",
                session,
                "action",
                "list-panes",
                "--all",
                "--json",
            ])
            .await?;
        let v: serde_json::Value = serde_json::from_str(&out).context("parsing list-panes")?;
        Ok(Some(
            v.as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["is_plugin"].as_bool() != Some(true))
                .filter_map(|p| {
                    Some(PaneInfo {
                        id: p["id"].as_u64()? as u32,
                        name: p["title"].as_str().unwrap_or_default().to_string(),
                        tab: p["tab_name"].as_str().unwrap_or_default().to_string(),
                        exited: p["exited"].as_bool().unwrap_or(false),
                        exit_status: p["exit_status"].as_i64().map(|s| s as i32),
                        columns: p["pane_columns"].as_u64().unwrap_or(0) as u32,
                    })
                })
                .collect(),
        ))
    }

    async fn rerun(&self, session: &str, pane: u32) -> Result<()> {
        let _g = self.op.lock().await;
        self.ensure_client(session).await?;
        let id = format!("terminal_{pane}");
        // Enter on a held pane re-runs its command. Only on a held pane:
        // anywhere else it would be typed into a program.
        let held = self
            .panes(session)
            .await?
            .into_iter()
            .flatten()
            .any(|p| p.id == pane && p.exited);
        if !held {
            bail!("pane {id} of {session} has not exited");
        }
        self.run(&[
            "--session",
            session,
            "action",
            "send-keys",
            "--pane-id",
            &id,
            "Enter",
        ])
        .await?;
        Ok(())
    }

    async fn screen(&self, session: &str, pane: u32) -> Result<String> {
        let id = format!("terminal_{pane}");
        self.run(&[
            "--session",
            session,
            "action",
            "dump-screen",
            "--pane-id",
            &id,
        ])
        .await
    }

    async fn paste(&self, session: &str, pane: u32, text: &str) -> Result<()> {
        let id = format!("terminal_{pane}");
        self.run(&[
            "--session",
            session,
            "action",
            "paste",
            "--pane-id",
            &id,
            text,
        ])
        .await
        .map(drop)
    }

    async fn enter(&self, session: &str, pane: u32) -> Result<()> {
        let id = format!("terminal_{pane}");
        self.run(&[
            "--session",
            session,
            "action",
            "send-keys",
            "--pane-id",
            &id,
            "Enter",
        ])
        .await
        .map(drop)
    }

    async fn kill_session(&self, session: &str) -> Result<()> {
        let _g = self.op.lock().await;
        if let Some(mut h) = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session)
        {
            h.kill();
        }
        let _ = self.run(&["kill-session", session]).await;
        let _ = self.run(&["delete-session", "--force", session]).await;
        Ok(())
    }

    async fn maintain(&self, sessions: &[String]) {
        for s in sessions {
            if self.client_running(s) {
                continue;
            }
            if matches!(self.session_alive(s).await, Ok(true)) {
                let _g = self.op.lock().await;
                if !self.client_running(s) {
                    tracing::info!("attaching a headless client to zellij session {s}");
                    if let Err(e) = self.spawn_client(s, None) {
                        tracing::warn!("{e:#}");
                    }
                }
            }
        }
    }

    fn attach_commands(&self, session: &str, pane: Option<u32>) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        if let Some(p) = pane {
            out.push(
                [&self.bin, "--session", session, "action", "focus-pane-id"]
                    .iter()
                    .map(|s| s.to_string())
                    .chain([format!("terminal_{p}")])
                    .collect(),
            );
        }
        out.push(vec![self.bin.clone(), "attach".into(), session.into()]);
        out
    }
}

// --- processes, for tests -----------------------------------------------------

struct FakePane {
    name: String,
    tab: String,
    spec: PaneSpec,
    pid: Option<u32>,
    /// `Some(code)` once exited (`code` is `None` when killed by a signal).
    status: Arc<Mutex<Option<Option<i32>>>>,
    output: Arc<Mutex<Vec<u8>>>,
}

type Reader = Box<dyn tokio::io::AsyncRead + Send + Unpin>;

/// Runs each "pane" as a child process with piped output: no terminal and
/// no zellij. For tests of the supervisor.
pub struct ProcessMux {
    env: Vec<(String, String)>,
    next: Mutex<u32>,
    sessions: Mutex<HashMap<String, std::collections::BTreeMap<u32, FakePane>>>,
    /// What was typed into panes: `(session, pane, text)`, Enter as `"\n"`.
    pub typed: Mutex<Vec<(String, u32, String)>>,
}

impl ProcessMux {
    /// `env` is added to every pane's environment.
    pub fn new(env: Vec<(String, String)>) -> ProcessMux {
        ProcessMux {
            env,
            next: Mutex::new(0),
            sessions: Mutex::new(HashMap::new()),
            typed: Mutex::new(Vec::new()),
        }
    }

    fn spawn(&self, session: &str, id: u32, p: &mut FakePane) -> Result<()> {
        use tokio::io::AsyncReadExt;
        let mut c = tokio::process::Command::new(&p.spec.argv[0]);
        c.args(&p.spec.argv[1..])
            .current_dir(&p.spec.cwd)
            .env("ZELLIJ_PANE_ID", id.to_string())
            .env("ZELLIJ_SESSION_NAME", session)
            .envs(self.env.iter().cloned())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0);
        let mut child = c
            .spawn()
            .with_context(|| format!("starting {:?}", p.spec.argv))?;
        p.pid = child.id();
        *p.status.lock().unwrap() = None;
        let readers: Vec<Reader> = [
            child.stdout.take().map(|o| Box::new(o) as Reader),
            child.stderr.take().map(|o| Box::new(o) as Reader),
        ]
        .into_iter()
        .flatten()
        .collect();
        for mut r in readers {
            let out = p.output.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = r.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let mut o = out.lock().unwrap();
                    o.extend_from_slice(&buf[..n]);
                    let len = o.len();
                    if len > 16384 {
                        o.drain(..len - 16384);
                    }
                }
            });
        }
        let status = p.status.clone();
        tokio::spawn(async move {
            let s = child.wait().await.ok().and_then(|s| s.code());
            *status.lock().unwrap() = Some(s);
        });
        Ok(())
    }

    fn edit_prompt(&self, session: &str, pane: u32, f: impl FnOnce(&mut String)) {
        let sessions = self.sessions.lock().unwrap();
        let Some(p) = sessions.get(session).and_then(|m| m.get(&pane)) else {
            return;
        };
        let mut out = p.output.lock().unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        let Some(start) = text.rfind("\n> ").map(|i| i + 1) else {
            return;
        };
        let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
        let mut line = text[start..end].to_string();
        f(&mut line);
        let new = format!("{}{line}{}", &text[..start], &text[end..]);
        *out = new.into_bytes();
    }

    /// The pids of all live panes (for tests).
    pub fn pids(&self) -> Vec<u32> {
        let sessions = self.sessions.lock().unwrap();
        sessions
            .values()
            .flat_map(|m| m.values())
            .filter(|p| p.status.lock().unwrap().is_none())
            .filter_map(|p| p.pid)
            .collect()
    }

    pub fn session_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.sessions.lock().unwrap().keys().cloned().collect();
        v.sort();
        v
    }
}

#[async_trait::async_trait]
impl Mux for ProcessMux {
    async fn open(&self, session: &str, tab: &str, spec: &PaneSpec) -> Result<u32> {
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            *n
        };
        let mut p = FakePane {
            name: spec.name.clone(),
            tab: tab.to_string(),
            spec: spec.clone(),
            pid: None,
            status: Arc::new(Mutex::new(None)),
            output: Arc::new(Mutex::new(Vec::new())),
        };
        self.spawn(session, id, &mut p)?;
        self.sessions
            .lock()
            .unwrap()
            .entry(session.to_string())
            .or_default()
            .insert(id, p);
        Ok(id)
    }

    async fn panes(&self, session: &str) -> Result<Option<Vec<PaneInfo>>> {
        let sessions = self.sessions.lock().unwrap();
        let Some(panes) = sessions.get(session) else {
            return Ok(None);
        };
        Ok(Some(
            panes
                .iter()
                .map(|(id, p)| {
                    let st = *p.status.lock().unwrap();
                    PaneInfo {
                        id: *id,
                        name: p.name.clone(),
                        tab: p.tab.clone(),
                        exited: st.is_some(),
                        exit_status: st.flatten(),
                        columns: 200,
                    }
                })
                .collect(),
        ))
    }

    async fn rerun(&self, session: &str, pane: u32) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let p = sessions
            .get_mut(session)
            .and_then(|m| m.get_mut(&pane))
            .context("no such pane")?;
        if p.status.lock().unwrap().is_none() {
            bail!("pane {pane} has not exited");
        }
        let mut fresh = FakePane {
            name: p.name.clone(),
            tab: p.tab.clone(),
            spec: p.spec.clone(),
            pid: None,
            status: p.status.clone(),
            output: p.output.clone(),
        };
        self.spawn(session, pane, &mut fresh)?;
        p.pid = fresh.pid;
        Ok(())
    }

    async fn screen(&self, session: &str, pane: u32) -> Result<String> {
        let sessions = self.sessions.lock().unwrap();
        let p = sessions
            .get(session)
            .and_then(|m| m.get(&pane))
            .context("no such pane")?;
        let out = p.output.lock().unwrap();
        let text = String::from_utf8_lossy(&out).into_owned();
        // Like a terminal: a clear-screen sequence hides what came before.
        Ok(match text.rfind("\x1b[2J") {
            Some(i) => text[i + 4..].to_string(),
            None => text,
        })
    }

    /// Recorded; like a TUI's input box, the last line that starts with
    /// `> ` shows what was typed (until Enter).
    async fn paste(&self, session: &str, pane: u32, text: &str) -> Result<()> {
        self.typed
            .lock()
            .unwrap()
            .push((session.to_string(), pane, text.to_string()));
        self.edit_prompt(session, pane, |line| line.push_str(text));
        Ok(())
    }

    async fn enter(&self, session: &str, pane: u32) -> Result<()> {
        self.typed
            .lock()
            .unwrap()
            .push((session.to_string(), pane, "\n".to_string()));
        self.edit_prompt(session, pane, |line| line.truncate(2));
        Ok(())
    }

    async fn kill_session(&self, session: &str) -> Result<()> {
        let panes = self.sessions.lock().unwrap().remove(session);
        for p in panes.into_iter().flat_map(|m| m.into_values()) {
            if let Some(pid) = p.pid
                && p.status.lock().unwrap().is_none()
            {
                // SAFETY: signalling our own child's process group.
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
        }
        Ok(())
    }

    async fn maintain(&self, _sessions: &[String]) {}

    fn attach_commands(&self, session: &str, pane: Option<u32>) -> Vec<Vec<String>> {
        vec![vec![
            "true".into(),
            session.into(),
            pane.map(|p| p.to_string()).unwrap_or_default(),
        ]]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_valid_kdl_shape() {
        let z = Zellij::new("zellij", Path::new("/tmp/x"), 200, 60);
        let t = z.layout_text(
            "backend",
            &PaneSpec {
                name: "be-1".into(),
                cwd: "/w/be \"1\"".into(),
                argv: vec!["/bin/amail".into(), "run".into(), "be-1".into()],
            },
        );
        assert!(t.contains("tab name=\"backend\""), "{t}");
        assert!(!t.contains("stacked"), "{t}");
        assert!(
            t.contains("pane name=\"be-1\" command=\"/bin/amail\" cwd=\"/w/be \\\"1\\\"\""),
            "{t}"
        );
        assert!(t.contains("args \"run\" \"be-1\""), "{t}");
    }
}
