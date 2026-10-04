//! Fault injection, without models: seats are fake harnesses (the real
//! `amail run` and `amail hook`) supervised by an in-process node; servers
//! are real `amail serve` processes. Each test breaks something while mail
//! is on its way and checks, from the database and from what every seat's
//! hooks injected, that no message is lost, none is injected twice, and the
//! seats catch up by themselves once the fault is over.
//!
//! The lost-claim-answer window is covered in `node.rs`
//! (`lost_claim_answer_is_recovered`) and in the store conformance tests
//! (`claim_keys_concurrent`).

mod common;
mod procs;

use amail_node::wire::{Request, SeatPlacement};
use amail_store::testutil::Backend;
use common::{FAKE_HARNESS, FakeInbox};
use procs::*;
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

macro_rules! fault_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(amail_store::testutil::Backend::Sqlite).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(amail_store::testutil::Backend::Postgres).await;
                }
            )*
        }
    };
}

fault_tests!(
    server_killed_with_mail_in_flight,
    node_killed_with_seats_busy_idle_asleep,
    network_partition_between_node_and_server,
    database_briefly_unavailable,
);

/// Postgres only: one of two instances is killed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_of_two_instances_killed() {
    instance_killed(Backend::Postgres).await;
}

/// A node (in this process) with fake-harness seats.
struct Env {
    dir: tempfile::TempDir,
    fake: PathBuf,
    cfg: amail_node::Config,
    mux: Arc<amail_node::mux::ProcessMux>,
    node: Option<amail_node::NodeHandle>,
    _inboxes: Vec<FakeInbox>,
    seats: Vec<String>,
    /// The org's user, talking to the server directly.
    user: Api,
}

impl Env {
    /// `server_url`: what the node uses (the server, or a proxy).
    async fn start(server_url: &str, user: Api, seats: &[&str], busy: &[&str]) -> Env {
        for s in seats {
            user.ok(
                Method::POST,
                "/seats",
                Some(json!({ "name": s, "harness": "claude", "model": "m", "permission_mode": "unattended" })),
            )
            .await;
        }
        let node_token = user
            .ok(Method::POST, "/tokens", Some(json!({ "scope": "node" })))
            .await["token"]
            .as_str()
            .unwrap()
            .to_string();
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake");
        std::fs::create_dir_all(fake.join("sock")).unwrap();
        let p = fake.join("claude");
        std::fs::write(&p, FAKE_HARNESS).unwrap();
        std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        for b in busy {
            std::fs::write(fake.join(format!("{b}.busy")), "").unwrap();
        }
        let mut cfg = amail_node::Config::new(server_url, &node_token);
        cfg.state_dir = dir.path().join("state");
        cfg.socket = dir.path().join("run").join("node.sock");
        cfg.label = Some("fault-node".into());
        cfg.amail_exe = PathBuf::from(BIN);
        cfg.claude_bin = p.to_string_lossy().into_owned();
        cfg.codex_bin = "/bin/false".into();
        cfg.codex_auth = dir.path().join("no-auth.json");
        cfg.wake_window = Duration::from_millis(200);
        cfg.wake_confirm = Duration::from_secs(3);
        cfg.wake_backoff = Duration::from_secs(2);
        cfg.heartbeat = Duration::from_secs(2);
        cfg.liveness_interval = Duration::from_millis(200);
        cfg.hook_server_timeout = Duration::from_secs(1);
        cfg.supervise_interval = Duration::from_millis(100);
        cfg.start_timeout = Duration::from_secs(5);
        cfg.probe_interval = Duration::from_millis(300);
        cfg.kill_grace = Duration::from_secs(2);
        cfg.restart_backoff = Duration::from_millis(200);
        let mux = Arc::new(amail_node::mux::ProcessMux::new(vec![
            ("FAKE_AMAIL".into(), BIN.into()),
            ("AMAIL_HOOK_TIMEOUT_MS".into(), "3000".into()),
        ]));
        let inboxes = seats
            .iter()
            .map(|s| {
                let wake = fake.join(format!("{s}.wake"));
                FakeInbox::start(
                    fake.join("sock").join(format!("{s}.sock")),
                    Some(Arc::new(move |_l: String| {
                        std::fs::write(&wake, "").unwrap();
                    })),
                )
            })
            .collect();
        let node = amail_node::start_with_mux(cfg.clone(), mux.clone())
            .await
            .unwrap();
        let env = Env {
            dir,
            fake,
            cfg,
            mux,
            node: Some(node),
            _inboxes: inboxes,
            seats: seats.iter().map(|s| s.to_string()).collect(),
            user,
        };
        env.wait_connected().await;
        let placements: Vec<SeatPlacement> = seats
            .iter()
            .map(|s| {
                let cwd = env.dir.path().join("work").join(s);
                std::fs::create_dir_all(&cwd).unwrap();
                SeatPlacement {
                    seat: s.to_string(),
                    group: None,
                    harness: "claude".into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    mux_session: "amail-acme-main".into(),
                    tab: s.to_string(),
                    paste_fallback: false,
                    permission_mode: None,
                    git_access: None,
                    dsh_jail: None,
                }
            })
            .collect();
        env.request(Request::Up {
            seats: placements,
            settings: Default::default(),
            start: true,
            only: None,
        })
        .await;
        for s in seats {
            let want = if busy.contains(s) { "busy" } else { "idle" };
            env.wait_state(s, want).await;
        }
        env
    }

    async fn request(&self, r: Request) -> Value {
        self.node.as_ref().unwrap().node.handle(r).await
    }

    async fn wait_connected(&self) {
        let t0 = Instant::now();
        while self.request(Request::Status).await["connected"] != true {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "node did not connect"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_state(&self, seat: &str, state: &str) {
        let t0 = Instant::now();
        loop {
            let v = self.request(Request::Ps).await;
            if v["seats"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["seat"] == seat && r["state"] == state)
            {
                return;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "{seat} never became {state}: {v}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Kill the node as a crash would, then start a new one on the same
    /// state (the seats' processes live on, like zellij panes).
    async fn crash_node(&mut self) -> amail_node::Config {
        self.node.take().unwrap().crash();
        self.cfg.clone()
    }

    async fn start_node(&mut self, cfg: amail_node::Config) {
        self.node = Some(
            amail_node::start_with_mux(cfg, self.mux.clone())
                .await
                .unwrap(),
        );
        self.wait_connected().await;
    }

    fn end_busy(&self) {
        for s in &self.seats {
            let _ = std::fs::remove_file(self.fake.join(format!("{s}.busy")));
        }
    }

    /// Message ids shown to `seat` as new mail, with how often.
    fn injected(&self, seat: &str) -> BTreeMap<i64, u32> {
        let text =
            std::fs::read_to_string(self.fake.join(format!("{seat}.injected"))).unwrap_or_default();
        let mut out = BTreeMap::new();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(t) = common::hook_text(&v) else {
                continue;
            };
            let mut new_mail = false;
            for l in t.lines() {
                if l.starts_with("[amail]") {
                    new_mail = l.contains("new message(s)");
                } else if new_mail && let Some(id) = common::injected_ids(l).first() {
                    *out.entry(*id).or_insert(0) += 1;
                }
            }
        }
        out
    }

    /// Every message the database holds for each seat is injected exactly
    /// once (waiting up to `within` for seats to catch up).
    async fn check_every_message_once(&self, within: Duration) -> usize {
        let t0 = Instant::now();
        loop {
            let mut missing = Vec::new();
            let mut total = 0;
            for s in &self.seats {
                let msgs = self
                    .user
                    .ok(
                        Method::GET,
                        &format!("/archive/messages?to={s}&limit=500"),
                        None,
                    )
                    .await;
                let ids: Vec<i64> = msgs["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m["source"] != "system")
                    .map(|m| m["id"].as_i64().unwrap())
                    .collect();
                total += ids.len();
                let got = self.injected(s);
                for (id, n) in &got {
                    assert_eq!(*n, 1, "#{id} was injected {n} times into {s}");
                    assert!(
                        ids.contains(id),
                        "#{id} injected into {s} but not addressed to it"
                    );
                }
                for id in ids {
                    if !got.contains_key(&id) {
                        missing.push((s.clone(), id));
                    }
                }
            }
            if missing.is_empty() {
                for s in &self.seats {
                    eprintln!(
                        "{s}: {} message(s), each injected once",
                        self.injected(s).len()
                    );
                }
                return total;
            }
            if t0.elapsed() > within {
                panic!(
                    "not injected after {within:?}: {missing:?}\nnode: {}",
                    self.request(Request::Status).await
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if let Some(n) = self.node.take() {
            n.shutdown();
        }
        for pid in self.mux.pids() {
            // SAFETY: signalling a child process group of this test.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
    }
}

/// Send `n` messages, one every `every`, to the seats in turn; errors (the
/// server is down) are expected and ignored: the database decides what was
/// sent.
fn sender(
    api: Api,
    seats: Vec<String>,
    n: usize,
    every: Duration,
) -> tokio::task::JoinHandle<usize> {
    tokio::spawn(async move {
        let mut ok = 0;
        for i in 0..n {
            let to = &seats[i % seats.len()];
            let (s, _) = api
                .call(
                    Method::POST,
                    "/messages",
                    Some(json!({ "to": [to], "subject": format!("m{i}"), "body": format!("body {i}") })),
                )
                .await;
            if s.is_success() {
                ok += 1;
            }
            tokio::time::sleep(every).await;
        }
        ok
    })
}

async fn server_killed_with_mail_in_flight(b: Backend) {
    let Some(db) = Db::new(b, "server_killed_with_mail_in_flight").await else {
        return;
    };
    let token = db.create_org("acme").await;
    let mut srv = ServerProc::start(&db.url, None).await;
    let user = Api::new(&srv.base, &token);
    let env = Env::start(&srv.base, user.clone(), &["idle1", "busy1"], &["busy1"]).await;
    let send = sender(
        user.clone(),
        env.seats.clone(),
        40,
        Duration::from_millis(50),
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    srv.kill();
    tokio::time::sleep(Duration::from_millis(1000)).await;
    srv.restart().await;
    let ok = send.await.unwrap();
    assert!(ok > 10 && ok < 40, "{ok} sends succeeded");
    env.end_busy();
    let n = env.check_every_message_once(Duration::from_secs(40)).await;
    assert!(n >= ok, "{n} delivered, {ok} acknowledged");
}

async fn node_killed_with_seats_busy_idle_asleep(b: Backend) {
    let Some(db) = Db::new(b, "node_killed_with_seats_busy_idle_asleep").await else {
        return;
    };
    let token = db.create_org("acme").await;
    let srv = ServerProc::start(&db.url, None).await;
    let user = Api::new(&srv.base, &token);
    let mut env = Env::start(
        &srv.base,
        user.clone(),
        &["idle1", "busy1", "sleepy"],
        &["busy1"],
    )
    .await;
    env.request(Request::Sleep {
        seat: "sleepy".into(),
    })
    .await;
    env.wait_state("sleepy", "dormant").await;
    // Mail before, during and after the node is down.
    let send = sender(
        user.clone(),
        env.seats.clone(),
        30,
        Duration::from_millis(60),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    let cfg = env.crash_node().await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    env.start_node(cfg).await;
    let ok = send.await.unwrap();
    assert_eq!(ok, 30);
    tokio::time::sleep(Duration::from_millis(500)).await;
    env.end_busy();
    let n = env.check_every_message_once(Duration::from_secs(40)).await;
    assert_eq!(n, 30);
}

async fn network_partition_between_node_and_server(b: Backend) {
    let Some(db) = Db::new(b, "network_partition_between_node_and_server").await else {
        return;
    };
    let token = db.create_org("acme").await;
    let srv = ServerProc::start(&db.url, None).await;
    let proxy = Proxy::start(&[format!("127.0.0.1:{}", srv.port)]).await;
    let user = Api::new(&srv.base, &token);
    let env = Env::start(&proxy.url(), user.clone(), &["idle1", "busy1"], &["busy1"]).await;
    let send = sender(
        user.clone(),
        env.seats.clone(),
        30,
        Duration::from_millis(80),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    proxy.cut();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    proxy.heal();
    let ok = send.await.unwrap();
    assert_eq!(ok, 30);
    env.end_busy();
    let n = env.check_every_message_once(Duration::from_secs(40)).await;
    assert_eq!(n, 30);
}

async fn database_briefly_unavailable(b: Backend) {
    let Some(db) = Db::new(b, "database_briefly_unavailable").await else {
        return;
    };
    let token = db.create_org("acme").await;
    // Postgres: the server reaches the database through a proxy that is cut
    // for a while. SQLite: another process holds an exclusive lock.
    let (db_url, proxy) = match b {
        Backend::Postgres => {
            let (pre, rest) = db.url.split_once('@').unwrap();
            let (hostport, path) = rest.split_once('/').unwrap();
            let p = Proxy::start(&[hostport.to_string()]).await;
            (format!("{pre}@127.0.0.1:{}/{path}", p.port), Some(p))
        }
        Backend::Sqlite => (db.url.clone(), None),
    };
    let srv = ServerProc::start(&db_url, None).await;
    let user = Api::new(&srv.base, &token);
    let env = Env::start(&srv.base, user.clone(), &["idle1", "busy1"], &["busy1"]).await;
    let send = sender(
        user.clone(),
        env.seats.clone(),
        60,
        Duration::from_millis(80),
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    match &proxy {
        Some(p) => {
            p.cut();
            tokio::time::sleep(Duration::from_millis(2500)).await;
            p.heal();
        }
        None => {
            use sqlx::Connection;
            let mut c = sqlx::SqliteConnection::connect(&db.url).await.unwrap();
            sqlx::query("BEGIN EXCLUSIVE")
                .execute(&mut c)
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(2500)).await;
            sqlx::query("ROLLBACK").execute(&mut c).await.unwrap();
        }
    }
    let ok = send.await.unwrap();
    assert!(
        ok >= 25,
        "{ok} sends succeeded; server log:\n{}",
        srv.log_text()
    );
    env.end_busy();
    env.check_every_message_once(Duration::from_secs(45)).await;
}

async fn instance_killed(b: Backend) {
    let Some(db) = Db::new(b, "instance_killed").await else {
        return;
    };
    let token = db.create_org("acme").await;
    let mut a = ServerProc::start(&db.url, None).await;
    let bsrv = ServerProc::start(&db.url, None).await;
    // A load balancer in front of both (A first); the node and the sender
    // go through it.
    let lb = Proxy::start(&[
        format!("127.0.0.1:{}", a.port),
        format!("127.0.0.1:{}", bsrv.port),
    ])
    .await;
    let user = Api::new(&lb.url(), &token);
    let env = Env::start(&lb.url(), user.clone(), &["idle1", "busy1"], &["busy1"]).await;
    let send = sender(
        user.clone(),
        env.seats.clone(),
        40,
        Duration::from_millis(60),
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    a.kill();
    lb.set_upstreams(&[format!("127.0.0.1:{}", bsrv.port)]);
    let ok = send.await.unwrap();
    assert!(ok >= 35, "{ok} sends succeeded");
    env.end_busy();
    env.check_every_message_once(Duration::from_secs(45)).await;
}
