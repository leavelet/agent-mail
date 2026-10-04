//! Failed harness turns: the mail a failed turn showed goes back to "not
//! notified, not read" and is delivered again, at most a bounded number of
//! times; the seat is flagged. Fixture hook inputs drive the real `amail
//! hook` binary against an in-process node and server (no model).
//! Every test runs on SQLite and (with `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use amail_store::testutil::Backend;
use common::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

macro_rules! turn_tests {
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

turn_tests!(
    claude_failed_turn_is_delivered_again,
    claude_failures_are_bounded,
    session_ending_mid_turn_is_a_failure,
    codex_failed_turns_from_the_app_server,
);

fn claude(name: &str) -> Value {
    fixture("claude", name)
}

fn opts(backoff_ms: u64, notices: bool) -> Opts {
    let mut o = Opts {
        wake_window: Duration::from_millis(50),
        wake_confirm: Duration::from_secs(5),
        notices,
        ..Default::default()
    };
    o.tweak = Some(if backoff_ms == 300 {
        |c: &mut amail_node::Config| c.turn_retry_backoff = Duration::from_millis(300)
    } else {
        |c: &mut amail_node::Config| {
            c.turn_retry_backoff = Duration::from_millis(150);
            c.turn_quiet = Duration::from_millis(600);
            c.turn_check_delay = Duration::from_millis(100);
        }
    });
    o
}

/// The recipient row of `seat` in the delivery status of message `id`.
async fn delivery(h: &Harness, id: i64, seat: &str) -> Value {
    let s = h.api.get(&format!("/messages/{id}/status")).await;
    s["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["seat"] == seat)
        .cloned()
        .unwrap()
}

async fn released(h: &Harness, id: i64, seat: &str) -> bool {
    let d = delivery(h, id, seat).await;
    d["notified_at"].is_null() && d["read_at"].is_null()
}

/// Mail shown in a turn that ends with `StopFailure` is marked not read,
/// shown again (once) by the next turn after a wake-up, and a turn that
/// ends normally clears the seat's failure.
async fn claude_failed_turn_is_delivered_again(b: Backend) {
    let Some(h) =
        Harness::start(b, "claude_failed_turn_is_delivered_again", opts(300, false)).await
    else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    let m1 = h.api.send(&["w1"], &[], "please look", false).await;
    h.wait("wake-up", || async { inbox.count() == 1 }).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    // A short body is shown whole: read.
    assert!(!delivery(&h, m1, "w1").await["read_at"].is_null());

    // The model API fails: no Stop, a StopFailure.
    assert_eq!(
        h.hook("claude", env.clone(), claude("StopFailure")).await,
        Value::Null
    );
    let failed_at = Instant::now();
    assert_eq!(h.state("w1").await, "idle");
    let f = h.seat_status("w1").await["turn_failed"].clone();
    assert_eq!(f["count"], 1, "{f}");
    assert_eq!(f["retrying"], true);
    assert_eq!(
        f["reason"],
        "authentication_failed: Failed to authenticate. API Error: 401 API key is invalid."
    );
    h.wait("released on the server", || async {
        released(&h, m1, "w1").await
    })
    .await;
    // The sender sees the seat flagged; so does the person.
    h.wait("presence carries the failure", || async {
        h.api.get("/seats/w1").await["presence"]["turn_failed"]["count"] == 1
    })
    .await;
    let att = h.api.get("/attention").await;
    let item = att["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "seat" && i["seat"] == "w1")
        .cloned()
        .expect("a needs-you entry for the seat");
    assert_eq!(item["attention"], "turn-failed", "{item}");
    assert_eq!(item["turn_failed"]["count"], 1);
    // A sender is told the seat's turns fail.
    let sent = h
        .api
        .post(
            "/messages",
            json!({"to": ["w1"], "subject": "and this", "body": "x"}),
        )
        .await;
    let m2 = sent["id"].as_i64().unwrap();
    assert!(
        sent["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["seat"] == "w1" && n["state"] == "turn_failed"),
        "{sent}"
    );

    // Woken again after the backoff (not at once).
    h.wait("second wake-up", || async { inbox.count() == 2 })
        .await;
    assert!(
        failed_at.elapsed() >= Duration::from_millis(250),
        "{:?}",
        failed_at.elapsed()
    );
    // The next turn shows both, each once.
    let mut shown = Vec::new();
    for name in ["UserPromptSubmit", "PostToolUse", "PostToolUse"] {
        let v = h.hook("claude", env.clone(), claude(name)).await;
        shown.extend(injected_ids(&hook_text(&v).unwrap_or_default()));
    }
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    shown.sort();
    assert_eq!(shown, vec![m1, m2]);
    assert_eq!(h.state("w1").await, "idle");
    assert_eq!(h.seat_status("w1").await["turn_failed"], Value::Null);
    let d = delivery(&h, m1, "w1").await;
    assert!(
        !d["notified_at"].is_null() && !d["read_at"].is_null(),
        "{d}"
    );
    h.wait("failure cleared on the server", || async {
        h.api.get("/seats/w1").await["presence"]
            .get("turn_failed")
            .is_none()
    })
    .await;
    // Nothing more to deliver: no further wake-up.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(inbox.count(), 2);
}

/// After three failed turns in a row the node stops waking the seat, keeps
/// it flagged and tells the person once; a normal turn clears it.
async fn claude_failures_are_bounded(b: Backend) {
    let Some(h) = Harness::start(b, "claude_failures_are_bounded", opts(150, true)).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    let m1 = h.api.send(&["w1"], &[], "please look", false).await;
    for n in 1..=3u64 {
        h.wait(&format!("wake-up {n}"), || async {
            inbox.count() == n as usize
        })
        .await;
        let v = h
            .hook("claude", env.clone(), claude("UserPromptSubmit"))
            .await;
        assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1], "turn {n}");
        h.hook("claude", env.clone(), claude("StopFailure")).await;
        assert_eq!(h.seat_status("w1").await["turn_failed"]["count"], n);
        h.wait("released", || async { released(&h, m1, "w1").await })
            .await;
    }
    let f = h.seat_status("w1").await["turn_failed"].clone();
    assert_eq!(f["retrying"], false, "{f}");
    // Not woken any more, though the mail is pending again.
    h.wait_pending("w1", 1, 0, 0).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(inbox.count(), 3);
    // The person is told once, with the harness's error.
    h.wait("notice", || async {
        system_mail(&h)
            .await
            .iter()
            .any(|(s, _)| s.starts_with("Turns of seat w1 are failing"))
    })
    .await;
    let mail = system_mail(&h).await;
    let notices: Vec<_> = mail
        .iter()
        .filter(|(s, _)| s.starts_with("Turns of seat w1"))
        .collect();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0]
            .1
            .contains("The last 3 turns of seat w1 (claude)")
            && notices[0]
                .1
                .contains("authentication_failed: Failed to authenticate"),
        "{}",
        notices[0].1
    );
    assert_eq!(
        h.api.get("/seats/w1").await["presence"]["turn_failed"]["retrying"],
        false
    );
    let att = h.api.get("/overview").await;
    assert_eq!(att["turn_failed"], 1, "{att}");

    // The person types into the session: the turn shows the mail; it fails
    // again; still one notice.
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    h.hook("claude", env.clone(), claude("StopFailure")).await;
    assert_eq!(h.seat_status("w1").await["turn_failed"]["count"], 4);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n = system_mail(&h)
        .await
        .iter()
        .filter(|(s, _)| s.starts_with("Turns of seat w1"))
        .count();
    assert_eq!(n, 1);
    // A turn that ends normally clears it.
    h.wait("released", || async { released(&h, m1, "w1").await })
        .await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(h.seat_status("w1").await["turn_failed"], Value::Null);
    assert_eq!(inbox.count(), 3);
}

/// The session process ends during a turn: a failed turn.
async fn session_ending_mid_turn_is_a_failure(b: Backend) {
    let Some(h) = Harness::start(b, "session_ending_mid_turn_is_a_failure", opts(300, false)).await
    else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let mut proc = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let mut env = claude_env("w1", &inbox.path);
    for (k, v) in env.iter_mut() {
        if k == "CLAUDE_PID" {
            *v = proc.id().to_string();
        }
    }
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    let m1 = h.api.send(&["w1"], &[], "please look", false).await;
    h.wait("wake-up", || async { inbox.count() == 1 }).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    proc.kill().unwrap();
    proc.wait().unwrap();
    h.wait("offline", || async { h.state("w1").await == "offline" })
        .await;
    let f = h.seat_status("w1").await["turn_failed"].clone();
    assert_eq!(f["count"], 1, "{f}");
    assert!(
        f["reason"]
            .as_str()
            .unwrap()
            .contains("ended during the turn"),
        "{f}"
    );
    h.wait("released", || async { released(&h, m1, "w1").await })
        .await;

    // A seat that exits while idle has no failure.
    h.seat("w2", "claude").await;
    let mut proc = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let env2: Vec<(String, String)> = vec![
        ("AMAIL_SEAT".into(), "w2".into()),
        (
            "CLAUDE_CODE_MESSAGING_SOCKET".into(),
            inbox.path.to_string_lossy().into(),
        ),
        ("CLAUDE_PID".into(), proc.id().to_string()),
    ];
    h.hook("claude", env2.clone(), claude("SessionStart")).await;
    h.hook("claude", env2.clone(), claude("UserPromptSubmit"))
        .await;
    h.hook("claude", env2.clone(), claude("Stop")).await;
    proc.kill().unwrap();
    proc.wait().unwrap();
    h.wait("offline", || async { h.state("w2").await == "offline" })
        .await;
    assert_eq!(h.seat_status("w2").await["turn_failed"], Value::Null);
}

/// A stand-in for the Codex app-server's control socket that answers
/// `thread/turns/list` with the turns set by the test.
struct FakeAppServer {
    turns: Arc<Mutex<Value>>,
    calls: Arc<Mutex<usize>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeAppServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeAppServer {
    fn start(path: &std::path::Path) -> FakeAppServer {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        let turns = Arc::new(Mutex::new(json!([])));
        let calls = Arc::new(Mutex::new(0));
        let (t2, c2) = (turns.clone(), calls.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                let (turns, calls) = (t2.clone(), c2.clone());
                tokio::spawn(async move {
                    // Handshake: read the request, answer 101.
                    let mut hdr = Vec::new();
                    while !hdr.ends_with(b"\r\n\r\n") {
                        let mut b = [0u8; 1];
                        if s.read(&mut b).await.unwrap_or(0) == 0 {
                            return;
                        }
                        hdr.push(b[0]);
                    }
                    s.write_all(
                        b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                          Connection: Upgrade\r\n\r\n",
                    )
                    .await
                    .unwrap();
                    loop {
                        // A masked client frame (small).
                        let mut h = [0u8; 2];
                        if s.read_exact(&mut h).await.is_err() {
                            return;
                        }
                        let mut len = (h[1] & 0x7f) as usize;
                        if len == 126 {
                            let mut b = [0u8; 2];
                            s.read_exact(&mut b).await.unwrap();
                            len = u16::from_be_bytes(b) as usize;
                        }
                        let mut m = [0u8; 4];
                        s.read_exact(&mut m).await.unwrap();
                        let mut data = vec![0u8; len];
                        s.read_exact(&mut data).await.unwrap();
                        for (i, b) in data.iter_mut().enumerate() {
                            *b ^= m[i % 4];
                        }
                        if h[0] & 0x0f == 8 {
                            return;
                        }
                        let Ok(req) = serde_json::from_slice::<Value>(&data) else {
                            continue;
                        };
                        let Some(id) = req.get("id").cloned() else {
                            continue;
                        };
                        let result = if req["method"] == "thread/turns/list" {
                            *calls.lock().unwrap() += 1;
                            json!({"data": turns.lock().unwrap().clone()})
                        } else {
                            json!({})
                        };
                        let out = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
                        let mut f = vec![0x81u8];
                        if out.len() < 126 {
                            f.push(out.len() as u8);
                        } else {
                            f.push(126);
                            f.extend_from_slice(&(out.len() as u16).to_be_bytes());
                        }
                        f.extend_from_slice(out.as_bytes());
                        if s.write_all(&f).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        FakeAppServer { turns, calls, task }
    }

    fn set(&self, turns: Value) {
        *self.turns.lock().unwrap() = turns;
    }
}

/// Codex hooks report no failed turn: the node asks the app-server how the
/// turn ended, after `Stop` and when a busy seat sends no hooks for a while.
async fn codex_failed_turns_from_the_app_server(b: Backend) {
    let Some(h) = Harness::start(
        b,
        "codex_failed_turns_from_the_app_server",
        opts(150, false),
    )
    .await
    else {
        return;
    };
    h.seat("cx", "codex").await;
    let home = h.dir.path().join("state/codex-home");
    let app = FakeAppServer::start(&home.join("app-server-control/app-server-control.sock"));
    let now_s = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    };
    let m1 = h.api.send(&["cx"], &[], "before launch", false).await;
    let cwd = h.dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let plan = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Launch {
            seat: "cx".into(),
            cwd: cwd.to_string_lossy().into(),
            pid: std::process::id(),
            harness: None,
            extra: vec![],
            pane: None,
            mux_session: None,
            permission_mode: Some(amail_proto::PermissionMode::Unattended),
            git_access: None,
            dsh_jail: None,
        })
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    let args: Vec<String> = serde_json::from_value(plan["args"].clone()).unwrap();
    let prompt = args.last().unwrap().clone();
    let env: Vec<(String, String)> = vec![
        ("CODEX_HOME".into(), home.to_string_lossy().into()),
        (
            "AMAIL_CODEX_PASS".into(),
            plan["env"]["AMAIL_CODEX_PASS"].as_str().unwrap().into(),
        ),
    ];
    let sid = "00000000-0000-7000-8000-00000000c0de";
    let codex = |name: &str, extra: &[(&str, Value)]| {
        let mut fields = vec![("session_id", json!(sid))];
        fields.extend(extra.iter().cloned());
        with(fixture("codex", name), &fields)
    };
    h.wait_pending("cx", 1, 0, 0).await;
    // The first turn (it registers the session) shows the mail.
    app.set(json!([{"id": "t1", "status": "inProgress", "items": [], "startedAt": now_s()}]));
    let v = h
        .hook(
            "codex",
            env.clone(),
            codex("UserPromptSubmit", &[("prompt", json!(prompt))]),
        )
        .await;
    assert!(injected_ids(&hook_text(&v).unwrap()).contains(&m1), "{v}");
    // Stop comes, but the app-server says the turn failed.
    app.set(
        json!([{"id": "t1", "status": "failed", "items": [], "startedAt": now_s(),
                    "error": {"message": "unexpected status 400: model not supported"}}]),
    );
    h.hook("codex", env.clone(), codex("Stop", &[])).await;
    h.wait("failure found", || async {
        h.seat_status("cx").await["turn_failed"]["count"] == 1
    })
    .await;
    assert_eq!(
        h.seat_status("cx").await["turn_failed"]["reason"],
        "unexpected status 400: model not supported"
    );
    h.wait("released", || async { released(&h, m1, "cx").await })
        .await;
    // Woken again (codex queue) after the backoff.
    h.wait("wake-up", || async {
        h.codex_calls().iter().any(|c| c.starts_with("queue"))
    })
    .await;

    // The next turn shows it again, then the turn fails without any hook:
    // found once the seat has been quiet for a while.
    app.set(json!([
        {"id": "t2", "status": "inProgress", "items": [], "startedAt": now_s()},
        {"id": "t1", "status": "failed", "items": [], "error": {"message": "x"}},
    ]));
    let v = h
        .hook("codex", env.clone(), codex("UserPromptSubmit", &[]))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    assert_eq!(h.state("cx").await, "busy");
    app.set(json!([
        {"id": "t2", "status": "failed", "items": [], "error": {"message": "stream disconnected"}},
    ]));
    h.wait("quiet failure found", || async {
        h.seat_status("cx").await["turn_failed"]["count"] == 2
    })
    .await;
    assert_eq!(h.state("cx").await, "idle");
    h.wait("released", || async { released(&h, m1, "cx").await })
        .await;

    // A turn that completes clears it.
    h.wait_pending("cx", 1, 0, 0).await;
    app.set(json!([{"id": "t3", "status": "inProgress", "items": [], "startedAt": now_s()}]));
    let v = h
        .hook("codex", env.clone(), codex("UserPromptSubmit", &[]))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    app.set(json!([{"id": "t3", "status": "completed", "items": [], "startedAt": now_s()}]));
    h.hook("codex", env.clone(), codex("Stop", &[])).await;
    h.wait("cleared", || async {
        h.seat_status("cx").await["turn_failed"].is_null()
    })
    .await;
    assert!(*app.calls.lock().unwrap() >= 3);
    assert!(!released(&h, m1, "cx").await);
}

/// `(subject, body)` of the system mail in the user's inbox.
async fn system_mail(h: &Harness) -> Vec<(String, String)> {
    let inbox = h.api.get("/inbox?limit=100").await;
    let mut out = Vec::new();
    for e in inbox["entries"].as_array().into_iter().flatten() {
        if e["message"]["source"] != "system" {
            continue;
        }
        let id = e["message"]["id"].as_i64().unwrap();
        let m = h.api.get(&format!("/messages/{id}?peek=1")).await;
        out.push((
            e["message"]["subject"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            m["message"]["body"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }
    out
}
