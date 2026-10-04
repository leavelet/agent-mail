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
    /// Check who sends socket requests (default: only with `supervise`,
    /// where sessions are launched as they are in use; other tests send
    /// hooks from the test process).
    pub check_callers: Option<bool>,
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
            check_callers: None,
        }
    }
}

pub const FAKE_HARNESS: &str = include_str!("fake_harness.sh");

impl Harness {
    pub async fn start(backend: Backend, test: &str, opts: Opts) -> Option<Harness> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut state = AppState::new(db.store.clone());
        if let Some(t) = opts.server_tweak {
            t(&mut state);
        }
        match opts.slow_claims {
            None => {
                tokio::spawn(amail_server::serve(listener, state, std::future::pending()));
            }
            Some((n, delay)) => {
                let left = Arc::new(std::sync::atomic::AtomicUsize::new(n));
                let _tasks = amail_server::start_background(&state);
                let app = amail_server::router(state).layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        let left = left.clone();
                        async move {
                            let claim = req.uri().path() == "/v1/claim";
                            let resp = next.run(req).await;
                            if claim
                                && left
                                    .fetch_update(
                                        std::sync::atomic::Ordering::SeqCst,
                                        std::sync::atomic::Ordering::SeqCst,
                                        |x| x.checked_sub(1),
                                    )
                                    .is_ok()
                            {
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
        let fake = dir.path().join("fake-codex");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\n# Stand-in for codex: records calls; no daemon, no app-server.\n\
                 printf '%s\\n' \"$*\" >> '{}'\n\
                 case \"$1\" in queue) exit 0;; esac\nexit 1\n",
                codex_log.display()
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
        // Claude Code transcripts are followed only under `projects` here.
        cfg.claude_config_dir = dir.path().join("claude");
        let fake_dir = dir.path().join("fake");
        let mut mux = None;
        if opts.supervise {
            std::fs::create_dir_all(fake_dir.join("sock")).unwrap();
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
