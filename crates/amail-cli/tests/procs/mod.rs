//! Real server processes, TCP proxies and event-stream readers for the
//! multi-instance and fault-injection tests.

#![allow(dead_code)]

use amail_store::testutil::Backend;
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A fresh database for one test.
pub struct Db {
    pub url: String,
    pub backend: Backend,
    _dir: Option<tempfile::TempDir>,
}

impl Db {
    pub async fn new(backend: Backend, test: &str) -> Option<Db> {
        let (url, dir) = amail_store::testutil::fresh_url(backend, test).await?;
        Some(Db {
            url,
            backend,
            _dir: dir,
        })
    }

    /// Create an org (running migrations); returns its user token.
    pub async fn create_org(&self, name: &str) -> String {
        let store = amail_store::open(&self.url, &amail_store::OpenOptions { max_connections: 2 })
            .await
            .unwrap();
        let token = amail_server::new_token();
        store
            .create_org(name, "owner", &amail_server::hash_token(&token))
            .await
            .unwrap();
        token
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An `amail serve` process. Killed (SIGKILL) when dropped.
pub struct ServerProc {
    pub port: u16,
    /// The server's URL, with its path prefix if it has one.
    pub base: String,
    db: String,
    /// `--base-path`, or empty.
    base_path: String,
    child: Option<Child>,
    pub log: PathBuf,
    _dir: tempfile::TempDir,
}

impl ServerProc {
    pub async fn start(db: &str, port: Option<u16>) -> ServerProc {
        Self::start_under(db, port, "").await
    }

    /// Start with `--base-path base_path` (none when empty).
    #[allow(dead_code)]
    pub async fn start_under(db: &str, port: Option<u16>, base_path: &str) -> ServerProc {
        let dir = tempfile::tempdir().unwrap();
        let port = port.unwrap_or_else(free_port);
        let mut s = ServerProc {
            port,
            base: format!("http://127.0.0.1:{port}{base_path}"),
            db: db.to_string(),
            base_path: base_path.to_string(),
            child: None,
            log: dir.path().join("server.log"),
            _dir: dir,
        };
        s.restart().await;
        s
    }

    /// Start the process (again, on the same port).
    pub async fn restart(&mut self) {
        self.kill();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .unwrap();
        let child = Command::new(BIN)
            .args([
                "serve",
                "--db",
                &self.db,
                "--listen",
                &format!("127.0.0.1:{}", self.port),
                "--max-connections",
                "8",
            ])
            .args(if self.base_path.is_empty() {
                vec![]
            } else {
                vec!["--base-path", self.base_path.as_str()]
            })
            .env("RUST_LOG", "info,sqlx=warn")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        self.child = Some(child);
        let http = reqwest::Client::new();
        let t0 = Instant::now();
        loop {
            if let Ok(r) = http.get(format!("{}/healthz", self.base)).send().await
                && r.status().is_success()
            {
                return;
            }
            if let Some(c) = self.child.as_mut()
                && let Ok(Some(st)) = c.try_wait()
            {
                panic!(
                    "amail serve exited ({st}): {}",
                    std::fs::read_to_string(&self.log).unwrap_or_default()
                );
            }
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "server did not come up"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// SIGKILL: no shutdown work at all.
    pub fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        self.kill();
    }
}

#[derive(Clone)]
pub struct Api {
    pub base: String,
    pub token: String,
    pub http: reqwest::Client,
}

impl Api {
    pub fn new(base: &str, token: &str) -> Api {
        Api {
            base: base.to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap(),
        }
    }

    pub async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}/v1{}", self.base, path))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
                (status, v)
            }
            Err(e) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Value::String(e.to_string()),
            ),
        }
    }

    pub async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method.clone(), path, body).await;
        assert!(s.is_success(), "{method} {path} -> {s}: {v}");
        v
    }
}

pub async fn wait_until<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let t0 = Instant::now();
    while !f().await {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A server-sent event stream read in the background.
pub struct Sse {
    rx: tokio::sync::mpsc::UnboundedReceiver<(String, Value)>,
    task: tokio::task::JoinHandle<()>,
}

impl Sse {
    pub async fn open(api: &Api, path: &str) -> Sse {
        let resp = reqwest::Client::new()
            .get(format!("{}/v1{}", api.base, path))
            .bearer_auth(&api.token)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success(), "{path}: {}", resp.status());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            let (mut event, mut data) = (String::new(), String::new());
            while let Some(Ok(chunk)) = stream.next().await {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(i) = buf.find('\n') {
                    let line: String = buf.drain(..=i).collect();
                    let line = line.trim_end_matches(['\n', '\r']);
                    if line.is_empty() {
                        if !event.is_empty() {
                            let v = serde_json::from_str(&data).unwrap_or(Value::Null);
                            let _ = tx.send((std::mem::take(&mut event), v));
                        }
                        data.clear();
                    } else if let Some(e) = line.strip_prefix("event:") {
                        event = e.trim().to_string();
                    } else if let Some(d) = line.strip_prefix("data:") {
                        data.push_str(d.trim_start());
                    }
                }
            }
        });
        Sse { rx, task }
    }

    /// The next event of `kind` whose data satisfies `pred` (others are
    /// skipped), within 10 s.
    pub async fn next_of(&mut self, kind: &str, pred: impl Fn(&Value) -> bool) -> Option<Value> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, self.rx.recv()).await {
                Ok(Some((k, v))) if k == kind && pred(&v) => return Some(v),
                Ok(Some(_)) => continue,
                _ => return None,
            }
        }
    }
}

impl Drop for Sse {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A TCP proxy in front of one or more upstreams (tried in order, like a
/// load balancer that fails over). `cut` closes every connection and
/// refuses new ones until `heal`.
pub struct Proxy {
    pub port: u16,
    upstreams: Arc<Mutex<Vec<String>>>,
    down: Arc<AtomicBool>,
    epoch: Arc<tokio::sync::watch::Sender<u64>>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    /// Upstreams as `host:port`.
    pub async fn start(upstreams: &[String]) -> Proxy {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let ups = Arc::new(Mutex::new(upstreams.to_vec()));
        let down = Arc::new(AtomicBool::new(false));
        let (etx, _) = tokio::sync::watch::channel(0u64);
        let epoch = Arc::new(etx);
        let (u2, d2, e2) = (ups.clone(), down.clone(), epoch.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if d2.load(Ordering::SeqCst) {
                    drop(inbound);
                    continue;
                }
                let list = u2.lock().unwrap().clone();
                let mut erx = e2.subscribe();
                tokio::spawn(async move {
                    let mut out = None;
                    for p in list {
                        if let Ok(s) = tokio::net::TcpStream::connect(p.as_str()).await {
                            out = Some(s);
                            break;
                        }
                    }
                    let Some(mut out) = out else { return };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut inbound, &mut out) => {}
                        _ = erx.changed() => {}
                    }
                });
            }
        });
        Proxy {
            port,
            upstreams: ups,
            down,
            epoch,
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn cut(&self) {
        self.down.store(true, Ordering::SeqCst);
        self.epoch.send_modify(|e| *e += 1);
    }

    pub fn heal(&self) {
        self.down.store(false, Ordering::SeqCst);
    }

    /// Close existing connections (they reconnect through the new list).
    pub fn set_upstreams(&self, ups: &[String]) {
        *self.upstreams.lock().unwrap() = ups.to_vec();
        self.epoch.send_modify(|e| *e += 1);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
