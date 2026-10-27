//! Shared harness for node tests: an in-process server, an in-process node,
//! and the real `amail hook` binary driven with fixture hook inputs.

#![allow(dead_code)]

use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use reqwest::Method;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_amail");

/// Longest time one test may take. A test that waits on an event stream,
/// or on a request that never gets an answer, fails with this message
/// instead of hanging the run.
pub const TEST_DEADLINE: Duration = Duration::from_secs(300);

/// Run the test `f`, failing it if it is not done within [`TEST_DEADLINE`].
pub async fn deadline(name: &str, f: impl std::future::Future<Output = ()>) {
    if tokio::time::timeout(TEST_DEADLINE, f).await.is_err() {
        panic!("test {name} did not finish within {TEST_DEADLINE:?}");
    }
}

/// Variables the hook client forwards; cleared so the test's own
/// environment (which may be a real agent session) cannot leak in.
const CLEAR_ENV: &[&str] = &[
    "AMAIL_SEAT",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_PID",
    "CODEX_HOME",
    "AMAIL_HOOK_TIMEOUT_MS",
    "AMAIL_URL",
    "AMAIL_TOKEN",
    "AMAIL_PROFILE",
    "CODEX_THREAD_ID",
    "AMAIL_DSH_ENDPOINT",
    "DSH_HOME",
    "AMAIL_CODEX_PASS",
];

pub fn fixture(harness: &str, name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hooks")
        .join(harness)
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// Run `amail hook <harness>` with `input` on stdin.
pub fn run_hook(
    sock: &Path,
    harness: &str,
    env: &[(&str, &str)],
    input: &str,
    timeout_ms: Option<u64>,
) -> (String, Duration, bool) {
    let mut c = Command::new(BIN);
    c.args(["hook", harness]);
    for k in CLEAR_ENV {
        c.env_remove(k);
    }
    c.env("AMAIL_NODE_SOCK", sock);
    if let Some(t) = timeout_ms {
        c.env("AMAIL_HOOK_TIMEOUT_MS", t.to_string());
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let t0 = Instant::now();
    let mut child = c.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let dt = t0.elapsed();
    (
        String::from_utf8(out.stdout).unwrap(),
        dt,
        out.status.success() && out.stderr.is_empty(),
    )
}

#[derive(Clone)]
pub struct Api {
    pub base: String,
    pub token: String,
    pub http: reqwest::Client,
}

impl Api {
    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let mut req = self
            .http
            .request(method.clone(), format!("{}/v1{}", self.base, path))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        assert!(status.is_success(), "{method} {path} -> {status}: {text}");
        if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap()
        }
    }
    pub async fn get(&self, path: &str) -> Value {
        self.call(Method::GET, path, None).await
    }
    pub async fn post(&self, path: &str, body: Value) -> Value {
        self.call(Method::POST, path, Some(body)).await
    }
    /// Send as this token's seat; returns the message id.
    pub async fn send(&self, to: &[&str], cc: &[&str], subject: &str, urgent: bool) -> i64 {
        let v = self
            .post(
                "/messages",
                json!({"to": to, "cc": cc, "subject": subject, "body": format!("body of {subject}"),
                       "priority": if urgent {"urgent"} else {"normal"}}),
            )
            .await;
        v["id"].as_i64().unwrap()
    }
    pub fn with_token(&self, token: &str) -> Api {
        Api {
            token: token.to_string(),
            ..self.clone()
        }
    }
}

pub struct Harness {
    pub db: TestDb,
    pub api: Api,
    pub node_token: String,
    pub node: Option<amail_node::NodeHandle>,
    pub dir: tempfile::TempDir,
    pub sock: PathBuf,
    pub codex_log: PathBuf,
    pub cfg: amail_node::Config,
    /// Supervised sessions run here (with `Opts::supervise`).
    pub mux: Option<Arc<amail_node::mux::ProcessMux>>,
    /// The fake harness directory (with `Opts::supervise`).
    pub fake: PathBuf,
}

/// Which request paths [`Opts::slow_routes`] holds back.
pub type SlowPath = fn(&str) -> bool;

pub struct Opts {
    pub wake_window: Duration,
    pub wake_confirm: Duration,
    pub wake_backoff: Duration,
    /// Point the node at a server that does not exist.
    pub server_down: bool,
    /// Run supervised sessions as processes of the fake harness
    /// (`fake_harness.sh`) instead of in zellij.
    pub supervise: bool,
    /// Adjust the node configuration.
    pub tweak: Option<fn(&mut amail_node::Config)>,
    /// Adjust the server state.
    pub server_tweak: Option<fn(&mut AppState)>,
    /// Hold back the server's answers to the first `n` claims by this long
    /// (the claim itself is done), like an answer lost on the way.
    pub slow_claims: Option<(usize, Duration)>,
    /// Hold back the server's answers to requests whose path (with `/v1`)
    /// the function accepts by this long, as a slow server would.
    pub slow_routes: Option<(SlowPath, Duration)>,
    /// Check who sends socket requests (default: only with `supervise`,
    /// where sessions are launched as they are in use; other tests send
    /// hooks from the test process).
    pub check_callers: Option<bool>,
    /// The node's local policy (default: none set, so the built-in default).
    pub local_policy: Option<amail_proto::LocalPolicy>,
    /// Send node notices (off by default: they would add system mail to the
    /// user's inbox in tests about other things).
    pub notices: bool,
    /// Serve under this URL path prefix (`--base-path`); the API client and
    /// the node use the URL with it.
    pub base_path: &'static str,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            wake_window: Duration::from_millis(400),
            wake_confirm: Duration::from_millis(1500),
            wake_backoff: Duration::from_secs(60),
            server_down: false,
            supervise: false,
            tweak: None,
            server_tweak: None,
            slow_claims: None,
            slow_routes: None,
            check_callers: None,
            local_policy: None,
            notices: false,
            base_path: "",
        }
    }
}

pub const FAKE_HARNESS: &str = include_str!("fake_harness.sh");
const FAKE_APP_SERVER: &str = include_str!("fake_app_server.py");

impl Harness {
    pub async fn start(backend: Backend, test: &str, opts: Opts) -> Option<Harness> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}{}",
            listener.local_addr().unwrap(),
            opts.base_path
        );
        let mut state = AppState::new(db.store.clone());
        if !opts.base_path.is_empty() {
            state.set_config(amail_server::config::Config {
                base_path: opts.base_path.into(),
                ..amail_server::config::Config::default()
            });
        }
        if let Some(t) = opts.server_tweak {
            t(&mut state);
        }
        match (opts.slow_claims, opts.slow_routes) {
            (None, Some((slow, delay))) => {
                let _tasks = amail_server::start_background(&state);
                let app = amail_server::router(state).layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| async move {
                        let hold = slow(req.uri().path());
                        let resp = next.run(req).await;
                        if hold {
                            tokio::time::sleep(delay).await;
                        }
                        resp
                    },
                ));
                tokio::spawn(async move { axum::serve(listener, app).await });
            }
            (None, None) => {
                tokio::spawn(amail_server::serve(listener, state, std::future::pending()));
            }
            (Some((n, delay)), _) => {
                let left = Arc::new(std::sync::atomic::AtomicUsize::new(n));
                let _tasks = amail_server::start_background(&state);
                let app = amail_server::router(state).layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        let left = left.clone();
                        async move {
                            let claim = req.uri().path() == "/v1/claim";
                            let resp = next.run(req).await;
                            if claim && take_one(&left) {
                                tokio::time::sleep(delay).await;
                            }
                            resp
                        }
                    },
                ));
                tokio::spawn(async move { axum::serve(listener, app).await });
            }
        }
        let token = new_token();
        db.store
            .create_org("acme", "owner", &hash_token(&token))
            .await
            .unwrap();
        let api = Api {
            base: base.clone(),
            token,
            http: reqwest::Client::new(),
        };
        let node_token = api.post("/tokens", json!({"scope": "node"})).await["token"]
            .as_str()
            .unwrap()
            .to_string();
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("run").join("node.sock");
        let codex_log = dir.path().join("codex.log");
        // Both fake codex binaries answer `app-server` with this script, so
        // that the node's check of the managed hooks' trust passes.
        let app_server = dir.path().join("fake_app_server.py");
        std::fs::write(&app_server, FAKE_APP_SERVER).unwrap();
        let fake = dir.path().join("fake-codex");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n# Stand-in for codex: records calls; no daemon; `app-server` only lists hooks.\n\
                 printf '%s\\n' \"$*\" >> '{}'\n\
                 case \"$1\" in queue) exit 0;; app-server) [ $# -eq 1 ] && exec python3 '{}';; esac\nexit 1\n",
                codex_log.display(),
                app_server.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let url = if opts.server_down {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            format!("http://127.0.0.1:{port}")
        } else {
            base
        };
        let mut cfg = amail_node::Config::new(&url, &node_token);
        cfg.state_dir = dir.path().join("state");
        cfg.socket = sock.clone();
        cfg.label = Some("test-node".into());
        cfg.amail_exe = PathBuf::from(BIN);
        cfg.claude_bin = "claude".into();
        cfg.codex_bin = fake.to_string_lossy().into_owned();
        cfg.codex_auth = dir.path().join("no-auth.json");
        cfg.wake_window = opts.wake_window;
        cfg.wake_confirm = opts.wake_confirm;
        cfg.wake_backoff = opts.wake_backoff;
        cfg.heartbeat = Duration::from_secs(2);
        cfg.liveness_interval = Duration::from_millis(200);
        cfg.hook_server_timeout = Duration::from_secs(5);
        cfg.supervise_interval = Duration::from_millis(100);
        cfg.start_timeout = Duration::from_secs(3);
        cfg.probe_interval = Duration::from_millis(300);
        cfg.kill_grace = Duration::from_secs(2);
        cfg.restart_backoff = Duration::from_millis(200);
        cfg.resume_grace = Duration::from_secs(3);
        cfg.check_callers = opts.check_callers.unwrap_or(opts.supervise);
        cfg.local_policy = opts.local_policy;
        cfg.notices = opts.notices;
        cfg.notice_window = Duration::from_secs(3);
        cfg.notice_tick = Duration::from_millis(100);
        // Claude Code transcripts are followed only under `projects` here.
        cfg.claude_config_dir = dir.path().join("claude");
        let fake_dir = dir.path().join("fake");
        let mut mux = None;
        if opts.supervise {
            std::fs::create_dir_all(fake_dir.join("sock")).unwrap();
            std::fs::write(fake_dir.join("fake_app_server.py"), FAKE_APP_SERVER).unwrap();
            for name in ["claude", "codex"] {
                let p = fake_dir.join(name);
                std::fs::write(&p, FAKE_HARNESS).unwrap();
                std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                    .unwrap();
            }
            cfg.claude_bin = fake_dir.join("claude").to_string_lossy().into_owned();
            cfg.codex_bin = fake_dir.join("codex").to_string_lossy().into_owned();
            mux = Some(Arc::new(amail_node::mux::ProcessMux::new(vec![
                ("FAKE_AMAIL".into(), BIN.into()),
                ("AMAIL_HOOK_TIMEOUT_MS".into(), "5000".into()),
            ])));
        }
        if let Some(t) = opts.tweak {
            t(&mut cfg);
        }
        let node = match &mux {
            Some(m) => amail_node::start_with_mux(cfg.clone(), m.clone())
                .await
                .unwrap(),
            None => amail_node::start(cfg.clone()).await.unwrap(),
        };
        let h = Harness {
            db,
            api,
            node_token,
            node: Some(node),
            dir,
            sock,
            codex_log,
            cfg,
            mux,
            fake: fake_dir,
        };
        if !opts.server_down {
            h.wait("node connected", || async {
                h.status().await["connected"].as_bool() == Some(true)
            })
            .await;
        }
        Some(h)
    }

    pub async fn seat(&self, name: &str, harness: &str) -> Api {
        self.api
            .post(
                "/seats",
                json!({"name": name, "harness": harness, "model": "test-model",
                       "permission_mode": "unattended", "role": format!("You are {name}.")}),
            )
            .await;
        let t = self
            .api
            .post(&format!("/seats/{name}/tokens"), json!({}))
            .await;
        self.api.with_token(t["token"].as_str().unwrap())
    }

    /// Stop the node and start a new one on the same state directory.
    pub async fn restart_node(&mut self) {
        self.node.take().unwrap().shutdown();
        self.node = Some(match &self.mux {
            Some(m) => amail_node::start_with_mux(self.cfg.clone(), m.clone())
                .await
                .unwrap(),
            None => amail_node::start(self.cfg.clone()).await.unwrap(),
        });
        self.wait("node connected", || async {
            self.status().await["connected"].as_bool() == Some(true)
        })
        .await;
    }

    pub async fn status(&self) -> Value {
        self.node
            .as_ref()
            .unwrap()
            .node
            .handle(amail_node::wire::Request::Status)
            .await
    }

    pub async fn seat_status(&self, seat: &str) -> Value {
        let s = self.status().await;
        s["seats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["seat"] == seat)
            .cloned()
            .unwrap_or(Value::Null)
    }

    pub async fn state(&self, seat: &str) -> String {
        self.seat_status(seat).await["state"]
            .as_str()
            .unwrap_or("none")
            .to_string()
    }

    /// Wait until the node's view of pending `to`/`cc`/`list` mail matches.
    pub async fn wait_pending(&self, seat: &str, to: i64, cc: i64, list: i64) {
        self.wait(
            &format!("{seat} pending to={to} cc={cc} list={list}"),
            || async {
                let p = &self.seat_status(seat).await["pending"];
                p["to"] == to && p["cc"] == cc && p["list"] == list
            },
        )
        .await;
    }

    pub async fn wait<F, Fut>(&self, what: &str, mut f: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let t0 = Instant::now();
        while !f().await {
            if t0.elapsed() > Duration::from_secs(15) {
                panic!(
                    "timed out waiting for {what}; node status: {}",
                    self.status().await
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Run a hook for `harness` on the blocking pool; returns stdout parsed
    /// as JSON (`Null` when empty).
    pub async fn hook(&self, harness: &str, env: Vec<(String, String)>, input: Value) -> Value {
        let sock = self.sock.clone();
        let harness = harness.to_string();
        let (out, _, ok) = tokio::task::spawn_blocking(move || {
            let env: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            run_hook(&sock, &harness, &env, &input.to_string(), Some(10_000))
        })
        .await
        .unwrap();
        assert!(ok, "hook failed");
        if out.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&out).unwrap_or_else(|e| panic!("bad hook output {out:?}: {e}"))
        }
    }

    pub fn codex_calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.codex_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(n) = self.node.take() {
            n.shutdown();
        }
        // Leave no fake harness processes behind.
        if let Some(m) = &self.mux {
            for pid in m.pids() {
                // SAFETY: signalling a child process group of this test.
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
        }
    }
}

/// Message ids listed in an injection text (`#<id> from ...` lines).
pub fn injected_ids(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|l| l.strip_prefix('#'))
        .filter(|l| l.contains(" from "))
        .filter_map(|l| l.split(' ').next()?.parse().ok())
        .collect()
}

/// The text of a hook answer: `additionalContext` or a Stop `reason`.
pub fn hook_text(v: &Value) -> Option<String> {
    v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .or_else(|| v["reason"].as_str())
        .map(str::to_string)
}

/// A fake Claude inbox socket that records every line written to it.
pub struct FakeInbox {
    pub path: PathBuf,
    pub lines: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl FakeInbox {
    /// `on_line` runs (on the blocking pool) for every received line.
    pub fn start(path: PathBuf, on_line: Option<Arc<dyn Fn(String) + Send + Sync>>) -> FakeInbox {
        use tokio::io::AsyncBufReadExt;
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l2 = lines.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((s, _)) = listener.accept().await else {
                    return;
                };
                let lines = l2.clone();
                let on_line = on_line.clone();
                tokio::spawn(async move {
                    let mut r = tokio::io::BufReader::new(s).lines();
                    while let Ok(Some(l)) = r.next_line().await {
                        lines.lock().unwrap().push(l.clone());
                        if let Some(f) = &on_line {
                            let f = f.clone();
                            tokio::task::spawn_blocking(move || f(l));
                        }
                    }
                });
            }
        });
        FakeInbox { path, lines, task }
    }

    pub fn count(&self) -> usize {
        self.lines.lock().unwrap().len()
    }
}

impl Drop for FakeInbox {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Environment of a Claude hook for `seat` whose inbox socket is `sock`.
pub fn claude_env(seat: &str, sock: &Path) -> Vec<(String, String)> {
    vec![
        ("AMAIL_SEAT".into(), seat.into()),
        (
            "CLAUDE_CODE_MESSAGING_SOCKET".into(),
            sock.to_string_lossy().into_owned(),
        ),
        ("CLAUDE_PID".into(), std::process::id().to_string()),
    ]
}

/// A fixture with fields replaced.
pub fn with(mut v: Value, fields: &[(&str, Value)]) -> Value {
    for (k, x) in fields {
        v[*k] = x.clone();
    }
    v
}

/// Send one request on the node socket from this process, as the command
/// line client does; the answer, or `{"error": ...}`.
pub async fn socket_request(sock: &Path, req: amail_node::wire::Request) -> Value {
    let sock = sock.to_path_buf();
    tokio::task::spawn_blocking(move || {
        match amail_node::request(&sock, &req, Duration::from_secs(10)) {
            Ok(v) => v,
            Err(e) => json!({"error": format!("{e:#}")}),
        }
    })
    .await
    .unwrap()
}

/// Take one from `left` unless it is 0 (a compare-and-swap loop, so that it
/// builds without the method renamed in later Rust versions).
fn take_one(left: &std::sync::atomic::AtomicUsize) -> bool {
    use std::sync::atomic::Ordering::SeqCst;
    let mut cur = left.load(SeqCst);
    while cur > 0 {
        match left.compare_exchange(cur, cur - 1, SeqCst, SeqCst) {
            Ok(_) => return true,
            Err(now) => cur = now,
        }
    }
    false
}

/// zellij's directories for a test: fresh ones under `dir`, never the
/// person's own (their sessions, sockets, cache and configuration).
pub fn zellij_env(dir: &std::path::Path) -> Vec<(String, std::ffi::OsString)> {
    let mut env = Vec::new();
    for (var, sub) in [
        ("XDG_RUNTIME_DIR", "run"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
    ] {
        let p = dir.join("zellij-xdg").join(sub);
        std::fs::create_dir_all(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        env.push((var.to_string(), p.into_os_string()));
    }
    assert_zellij_isolated(&env, dir);
    env
}

/// The guard: refuse to start zellij unless each of its four directories
/// is set and lies under the test's own temporary directory `dir` (so it is
/// neither the person's runtime directory nor their cache, configuration or
/// data).
pub fn assert_zellij_isolated(env: &[(String, std::ffi::OsString)], dir: &std::path::Path) {
    for var in [
        "XDG_RUNTIME_DIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
    ] {
        let v = env
            .iter()
            .find(|(k, _)| k == var)
            .map(|(_, v)| std::path::PathBuf::from(v));
        let ok = v.as_ref().is_some_and(|p| p.starts_with(dir) && p.is_dir())
            && std::env::var_os(var)
                .is_none_or(|own| v.as_deref() != Some(std::path::Path::new(&own)));
        assert!(
            ok,
            "refusing to start zellij: {var} must be a fresh directory under the test's temporary directory {} (got {v:?})",
            dir.display()
        );
    }
}

/// A zellij for a test, with isolated directories (see [`zellij_env`]).
pub fn test_zellij(
    bin: &str,
    dir: &std::path::Path,
    cols: u16,
    rows: u16,
) -> amail_node::mux::Zellij {
    amail_node::mux::Zellij::new(bin, dir, cols, rows).with_env(zellij_env(dir))
}

/// Kills a test's zellij session by name when dropped (also when the test
/// fails), with the test's isolated directories.
pub struct ZellijSession {
    pub bin: String,
    pub env: Vec<(String, std::ffi::OsString)>,
    pub name: String,
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        for args in [
            vec!["kill-session", self.name.as_str()],
            vec!["delete-session", "--force", self.name.as_str()],
        ] {
            let mut c = std::process::Command::new(&self.bin);
            c.args(&args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            for (k, v) in &self.env {
                c.env(k, v);
            }
            let _ = c.status();
        }
    }
}

/// `amail mcp` as a child process, driven over stdio with
/// newline-delimited JSON-RPC.
pub struct McpProc {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    next_id: i64,
    _home: tempfile::TempDir,
}

impl McpProc {
    /// Start `amail mcp <args>` with only `env`, `PATH` and an empty
    /// `HOME` (so no saved login of the person running the tests is used).
    pub fn start(args: &[&str], env: &[(&str, &str)]) -> McpProc {
        let home = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(BIN);
        cmd.arg("mcp")
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("AMAIL_CONFIG_DIR", home.path().join("cfg"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        McpProc {
            child,
            stdin,
            lines: rx,
            next_id: 1,
            _home: home,
        }
    }

    /// Write one raw line.
    pub fn write_line(&mut self, line: &str) {
        self.stdin.write_all(line.as_bytes()).unwrap();
        self.stdin.write_all(b"\n").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Wait for the answer with this id (other lines are skipped).
    pub fn answer(&mut self, id: i64) -> Value {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("no answer to request {id}"));
            let v: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("not JSON from amail mcp: {e}: {line}"));
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    /// Send a request and wait for its answer.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_line(&line.to_string());
        self.answer(id)
    }

    /// `initialize` and the `initialized` notification.
    pub fn initialize(&mut self) -> Value {
        let v = self.request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "amail-test", "version": "1"}}),
        );
        self.write_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        v
    }

    /// `tools/call`: the result (panics on a JSON-RPC error).
    pub fn call(&mut self, tool: &str, args: Value) -> Value {
        self.call_meta(tool, args, None)
    }

    /// `tools/call` with `_meta`.
    pub fn call_meta(&mut self, tool: &str, args: Value, meta: Option<Value>) -> Value {
        let mut params = json!({"name": tool, "arguments": args});
        if let Some(m) = meta {
            params["_meta"] = m;
        }
        let v = self.request("tools/call", params);
        assert!(v.get("error").is_none(), "{tool}: {v}");
        v["result"].clone()
    }
}

impl Drop for McpProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
