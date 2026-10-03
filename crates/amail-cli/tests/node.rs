//! Node and hook behaviour without real models: fixture hook inputs drive
//! the real `amail hook` binary against an in-process node and server.
//! Every test runs on SQLite and (with `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use common::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

macro_rules! node_tests {
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

node_tests!(
    claude_state_machine,
    claude_stop_block_cap,
    claude_ignores_internal_agents,
    codex_registration_and_rows,
    wake_coalesces_and_skips_cc,
    wake_retry_then_failed,
    hook_and_wake_inject_once,
    server_down_answers_nothing,
    lost_answer_is_kept,
    restart_restores_idle_seat,
    inlined_mail_is_read,
);

use amail_store::testutil::Backend;

async fn setup(b: Backend, test: &str, opts: Opts) -> Option<Harness> {
    Harness::start(b, test, opts).await
}

fn claude(name: &str) -> Value {
    fixture("claude", name)
}

fn codex(name: &str) -> Value {
    fixture("codex", name)
}

/// Every row of the Claude Code state table, plus presence reporting.
async fn claude_state_machine(b: Backend) {
    let Some(h) = setup(b, "claude_state_machine", Opts::default()).await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);

    // SessionStart with no mail: idle (the session waits at its prompt), no output.
    assert_eq!(
        h.hook("claude", env.clone(), claude("SessionStart")).await,
        Value::Null
    );
    assert_eq!(h.state("w1").await, "idle");

    // Presence reaches the server.
    h.wait("presence idle", || async {
        h.api.get("/seats/w1").await["presence"]["state"] == "idle"
    })
    .await;

    // Mail: one `to`, one `cc`. Make the seat busy first so no wake-up fires.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(h.state("w1").await, "busy");
    let m_to = h.api.send(&["w1"], &[], "to mail", false).await;
    let m_cc = h.api.send(&[], &["w1"], "cc mail", false).await;
    h.wait_pending("w1", 1, 1, 0).await;

    // SessionStart (resume) lists unread mail without claiming it.
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("SessionStart"), &[("source", json!("resume"))]),
        )
        .await;
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let text = hook_text(&v).unwrap();
    assert!(
        text.starts_with("[amail] w1 has 2 unread message(s):"),
        "{text}"
    );
    assert_eq!(injected_ids(&text), vec![m_to, m_cc]);
    h.wait_pending("w1", 1, 1, 0).await;
    // UserPromptSubmit claims every class.
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m_to, m_cc]);
    h.wait_pending("w1", 0, 0, 0).await;
    let m_to = h.api.send(&["w1"], &[], "to mail 2", false).await;
    let m_cc = h.api.send(&[], &["w1"], "cc mail 2", false).await;
    h.wait_pending("w1", 1, 1, 0).await;

    // PostToolUse: only `to` (and urgent) mail, as additionalContext.
    let v = h.hook("claude", env.clone(), claude("PostToolUse")).await;
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let text = hook_text(&v).unwrap();
    assert!(
        text.starts_with("[amail] 1 new message(s) for w1:"),
        "{text}"
    );
    assert!(text.contains("from owner (USER) · to you"), "{text}");
    assert!(text.contains("  > body of to mail 2"), "{text}");
    assert_eq!(injected_ids(&text), vec![m_to]);
    assert_eq!(h.state("w1").await, "busy");
    // Nothing more for PostToolUse: cc waits.
    assert_eq!(
        h.hook("claude", env.clone(), claude("PostToolUse")).await,
        Value::Null
    );

    // Urgent cc is taken by PostToolUse.
    let m_urgent = h.api.send(&[], &["w1"], "urgent cc", true).await;
    h.wait_pending("w1", 0, 2, 0).await;
    let text = hook_text(&h.hook("claude", env.clone(), claude("PostToolUse")).await).unwrap();
    assert_eq!(injected_ids(&text), vec![m_urgent]);
    assert!(text.contains("URGENT"), "{text}");

    // UserPromptSubmit takes everything else.
    h.wait_pending("w1", 0, 1, 0).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m_cc]);

    // Stop with nothing pending: idle, no output.
    h.wait_pending("w1", 0, 0, 0).await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    assert_eq!(h.state("w1").await, "idle");

    // Stop with mail: block with the mail as reason, stay busy.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let m = h.api.send(&["w1"], &[], "late", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    let v = h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(v["decision"], "block");
    assert_eq!(injected_ids(v["reason"].as_str().unwrap()), vec![m]);
    assert_eq!(h.state("w1").await, "busy");
    // The continued turn ends; stop_hook_active is set but nothing is new.
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("Stop"), &[("stop_hook_active", json!(true))]),
        )
        .await;
    assert_eq!(v, Value::Null);
    assert_eq!(h.state("w1").await, "idle");

    // Permission prompts: awaiting. The idle notification is ignored.
    h.hook("claude", env.clone(), claude("Notification_idle"))
        .await;
    assert_eq!(h.state("w1").await, "idle");
    h.hook("claude", env.clone(), claude("Notification_permission"))
        .await;
    assert_eq!(h.state("w1").await, "awaiting");
    h.hook("claude", env.clone(), claude("PostToolUse")).await;
    assert_eq!(h.state("w1").await, "busy");
    h.hook("claude", env.clone(), claude("PermissionRequest"))
        .await;
    assert_eq!(h.state("w1").await, "awaiting");
    h.wait("presence awaiting", || async {
        h.api.get("/seats/w1").await["presence"]["state"] == "awaiting"
    })
    .await;

    // SessionEnd: offline.
    h.hook("claude", env.clone(), claude("SessionEnd")).await;
    assert_eq!(h.state("w1").await, "offline");
    h.wait("presence offline", || async {
        h.api.get("/seats/w1").await["presence"]["state"] == "offline"
    })
    .await;

    // Delivery status shows the hook path.
    let st = w1.get(&format!("/messages/{m}")).await;
    assert_eq!(st["delivery"]["notified_via"], "hook:Stop");
    assert_eq!(inbox.count(), 0, "no wake-up was needed");
}

/// `Stop` blocks at most three times in a row; the rest is left to the waker.
async fn claude_stop_block_cap(b: Backend) {
    let Some(h) = setup(b, "claude_stop_block_cap", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    for i in 0..3 {
        h.api.send(&["w1"], &[], &format!("m{i}"), false).await;
        h.wait_pending("w1", 1, 0, 0).await;
        let v = h
            .hook(
                "claude",
                env.clone(),
                with(claude("Stop"), &[("stop_hook_active", json!(i > 0))]),
            )
            .await;
        assert_eq!(v["decision"], "block", "block {i}");
    }
    h.api.send(&["w1"], &[], "m3", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("Stop"), &[("stop_hook_active", json!(true))]),
        )
        .await;
    assert_eq!(v, Value::Null, "the fourth Stop must not block");
    assert_eq!(h.state("w1").await, "idle");
    // The remaining mail wakes the idle session instead.
    h.wait("wake-up", || async { inbox.count() == 1 }).await;
    let line: Value = serde_json::from_str(&inbox.lines.lock().unwrap()[0]).unwrap();
    assert_eq!(line["type"], "user");
    let content = line["message"]["content"].as_str().unwrap();
    assert!(
        content.contains("[amail] You have new mail (wake #"),
        "{content}"
    );
    // A new turn resets the counter.
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert!(hook_text(&v).unwrap().contains("\"m3\""));
    h.api.send(&["w1"], &[], "m4", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    assert_eq!(
        h.hook("claude", env, claude("Stop")).await["decision"],
        "block"
    );
}

/// Hooks of internal helper agents (`agent_id`) and unknown events are ignored.
async fn claude_ignores_internal_agents(b: Backend) {
    let Some(h) = setup(b, "claude_ignores_internal_agents", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    h.api.send(&["w1"], &[], "x", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    for name in ["PostToolUse_agent", "SubagentStop", "PreToolUse"] {
        assert_eq!(
            h.hook("claude", env.clone(), claude(name)).await,
            Value::Null,
            "{name}"
        );
    }
    // A hook without AMAIL_SEAT (a session amail did not start) is ignored too.
    let no_seat: Vec<(String, String)> = env[1..].to_vec();
    assert_eq!(
        h.hook("claude", no_seat, claude("PostToolUse")).await,
        Value::Null
    );
    // Garbage input is ignored.
    let (out, _, ok) = run_hook(
        &h.sock,
        "claude",
        &[("AMAIL_SEAT", "w1")],
        "not json",
        Some(5000),
    );
    assert!(ok && out.is_empty());
    h.wait_pending("w1", 1, 0, 0).await;
    assert_eq!(h.state("w1").await, "busy");
    // The real hook still gets it.
    let v = h.hook("claude", env, claude("PostToolUse")).await;
    assert!(hook_text(&v).unwrap().contains("\"x\""));
}

/// Codex: hooks of unregistered sessions are ignored; the launch marker in the
/// first prompt registers the session; then every row of the state table.
async fn codex_registration_and_rows(b: Backend) {
    let Some(h) = setup(b, "codex_registration_and_rows", Opts::default()).await else {
        return;
    };
    h.seat("cx", "codex").await;
    let sid = "00000000-0000-7000-8000-00000000c0de";
    let env: Vec<(String, String)> = vec![(
        "CODEX_HOME".into(),
        h.dir
            .path()
            .join("state/codex-home")
            .to_string_lossy()
            .into(),
    )];
    let m1 = h.api.send(&["cx"], &[], "before launch", false).await;
    h.wait_pending("cx", 1, 0, 0).await;

    // Unregistered session: ignored.
    let post = with(codex("PostToolUse"), &[("session_id", json!(sid))]);
    assert_eq!(
        h.hook("codex", env.clone(), post.clone()).await,
        Value::Null
    );
    // A marker with an unknown nonce does not register.
    let bogus = with(
        codex("UserPromptSubmit"),
        &[
            ("session_id", json!(sid)),
            (
                "prompt",
                json!("[amail:seat=cx launch=0123456789abcdef]\nhi"),
            ),
        ],
    );
    assert_eq!(h.hook("codex", env.clone(), bogus).await, Value::Null);

    // Launch through the node socket, as `amail run` does.
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
        })
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    let args: Vec<String> = serde_json::from_value(plan["args"].clone()).unwrap();
    assert_eq!(
        &args[..6],
        ["-m", "test-model", "-a", "never", "-s", "workspace-write"]
    );
    let prompt = args.last().unwrap().clone();
    assert!(prompt.starts_with("[amail:seat=cx launch="), "{prompt}");
    assert!(prompt.contains("You are cx."), "{prompt}");
    assert_eq!(plan["env"]["AMAIL_SEAT"], "cx");
    assert!(
        plan["env"]["CODEX_HOME"]
            .as_str()
            .unwrap()
            .ends_with("state/codex-home")
    );
    // Forbidden flags are refused.
    let bad = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Launch {
            seat: "cx".into(),
            cwd: cwd.to_string_lossy().into(),
            pid: std::process::id(),
            harness: None,
            extra: vec!["-c".into(), "model=x".into()],
        })
        .await;
    assert!(
        bad["error"].as_str().unwrap().contains("not allowed"),
        "{bad}"
    );
    // The home's config: hooks, trust entries, mail instructions, project trust.
    let config =
        std::fs::read_to_string(h.dir.path().join("state/codex-home/config.toml")).unwrap();
    assert!(config.contains("hook codex"), "{config}");
    assert!(config.contains("trusted_hash = \"sha256:"), "{config}");
    assert!(config.contains("developer_instructions"), "{config}");
    assert!(config.contains("trust_level = \"trusted\""), "{config}");

    // SessionStart comes before the first prompt and is still unregistered.
    let start = with(codex("SessionStart"), &[("session_id", json!(sid))]);
    assert_eq!(
        h.hook("codex", env.clone(), start.clone()).await,
        Value::Null
    );
    // The first prompt registers and claims pending mail.
    let first = with(
        codex("UserPromptSubmit"),
        &[("session_id", json!(sid)), ("prompt", json!(prompt))],
    );
    let v = h.hook("codex", env.clone(), first.clone()).await;
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    assert_eq!(h.state("cx").await, "busy");
    // The nonce is single-use: another session with the same prompt is ignored.
    let other = with(first.clone(), &[("session_id", json!("someone-else"))]);
    assert_eq!(h.hook("codex", env.clone(), other).await, Value::Null);

    // Registered rows.
    h.wait_pending("cx", 0, 0, 0).await;
    let m2 = h.api.send(&["cx"], &[], "busy mail", false).await;
    h.wait_pending("cx", 1, 0, 0).await;
    let v = h.hook("codex", env.clone(), post.clone()).await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m2]);
    let stop = with(codex("Stop"), &[("session_id", json!(sid))]);
    assert_eq!(
        h.hook("codex", env.clone(), stop.clone()).await,
        Value::Null
    );
    assert_eq!(h.state("cx").await, "idle");
    let up = with(codex("UserPromptSubmit"), &[("session_id", json!(sid))]);
    h.hook("codex", env.clone(), up.clone()).await;
    let perm = with(codex("PermissionRequest"), &[("session_id", json!(sid))]);
    h.hook("codex", env.clone(), perm).await;
    assert_eq!(h.state("cx").await, "awaiting");
    let int = with(codex("Interrupt"), &[("session_id", json!(sid))]);
    assert_eq!(h.hook("codex", env.clone(), int).await, Value::Null);
    assert_eq!(h.state("cx").await, "idle");

    // Idle + new `to` mail: woken with `codex queue` on the session's thread.
    h.api.send(&["cx"], &[], "wake me", false).await;
    h.wait("codex queue", || async {
        h.codex_calls().iter().any(|c| c.starts_with("queue"))
    })
    .await;
    let call = h
        .codex_calls()
        .into_iter()
        .find(|c| c.starts_with("queue"))
        .unwrap();
    assert!(
        call.starts_with(&format!(
            "queue --thread {sid} --message [amail] You have new mail (wake #"
        )),
        "{call}"
    );
    // The queued message starts a turn: UserPromptSubmit claims the mail.
    let v = h.hook("codex", env.clone(), up.clone()).await;
    assert!(hook_text(&v).unwrap().contains("\"wake me\""));
    // A lazy SessionStart (first turn) means busy.
    h.hook("codex", env.clone(), start).await;
    assert_eq!(h.state("cx").await, "busy");

    // Tool commands in the session get the seat's credentials from the node.
    let cred = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Credentials {
            codex_thread: sid.into(),
        })
        .await;
    assert_eq!(cred["seat"], "cx");
    let me = h
        .api
        .with_token(cred["token"].as_str().unwrap())
        .get("/whoami")
        .await;
    assert_eq!(me["seat"], "cx");
    let none = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Credentials {
            codex_thread: "nope".into(),
        })
        .await;
    assert!(none.get("error").is_some());

    // A late SessionEnd from another session does not take the seat offline.
    let end_other = with(
        codex("SessionEnd"),
        &[("session_id", json!("someone-else"))],
    );
    h.hook("codex", env.clone(), end_other).await;
    assert_eq!(h.state("cx").await, "busy");
    let end = with(codex("SessionEnd"), &[("session_id", json!(sid))]);
    h.hook("codex", env.clone(), end).await;
    assert_eq!(h.state("cx").await, "offline");

    // The registration is persisted for resumed sessions.
    let saved = std::fs::read_to_string(h.dir.path().join("state/node-state.json")).unwrap();
    assert!(saved.contains(sid), "{saved}");
}

/// Wake-ups: a burst of `to` mail gives one wake-up; cc never wakes; urgent
/// wakes without waiting for the window.
async fn wake_coalesces_and_skips_cc(b: Backend) {
    let opts = Opts {
        wake_window: Duration::from_millis(1500),
        wake_confirm: Duration::from_secs(10),
        ..Default::default()
    };
    let Some(h) = setup(b, "wake_coalesces_and_skips_cc", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    assert_eq!(h.state("w1").await, "idle");

    // cc mail: no wake-up.
    h.api.send(&[], &["w1"], "fyi", false).await;
    h.wait_pending("w1", 0, 1, 0).await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(inbox.count(), 0, "cc must not wake");

    // Five `to` messages in quick succession: one wake-up after the window.
    let t0 = Instant::now();
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(h.api.send(&["w1"], &[], &format!("burst {i}"), false).await);
    }
    h.wait("one wake-up", || async { inbox.count() == 1 }).await;
    assert!(
        t0.elapsed() >= Duration::from_millis(1400),
        "{:?}",
        t0.elapsed()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(inbox.count(), 1);
    // The turn it starts lists all five and the earlier cc.
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let got = injected_ids(&hook_text(&v).unwrap());
    assert_eq!(got.len(), 6, "{got:?}");
    for id in &ids {
        assert!(got.contains(id));
    }
    h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(h.state("w1").await, "idle");

    // Urgent cc wakes at once.
    let t0 = Instant::now();
    h.api.send(&[], &["w1"], "urgent", true).await;
    h.wait("urgent wake-up", || async { inbox.count() == 2 })
        .await;
    assert!(
        t0.elapsed() < Duration::from_millis(1000),
        "{:?}",
        t0.elapsed()
    );
    // Wake numbers differ (Claude drops a repeat of the previous message).
    let l = inbox.lines.lock().unwrap().clone();
    assert_ne!(l[0], l[1]);
}

/// An unanswered wake-up is retried once, then the seat is marked
/// wake_failed (reported to the server). It is tried again on a backoff, and
/// at once when new mail arrives; a turn clears it.
async fn wake_retry_then_failed(b: Backend) {
    let opts = Opts {
        wake_window: Duration::from_millis(100),
        wake_confirm: Duration::from_millis(600),
        wake_backoff: Duration::from_millis(1500),
        ..Default::default()
    };
    let Some(h) = setup(b, "wake_retry_then_failed", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    h.api.send(&["w1"], &[], "hello?", false).await;
    h.wait("two attempts", || async { inbox.count() == 2 })
        .await;
    h.wait("wake_failed", || async {
        h.seat_status("w1").await["wake_failed"] == true
    })
    .await;
    let failed_at = Instant::now();
    h.wait("wake_failed reported", || async {
        h.api.get("/seats/w1").await["presence"]["wake_failed"] == true
    })
    .await;
    // Backoff: one more attempt after 1.5 s, then the next after 3 s.
    h.wait("backoff attempt", || async { inbox.count() == 3 })
        .await;
    let dt = failed_at.elapsed();
    assert!(dt >= Duration::from_millis(1300), "{dt:?}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(inbox.count(), 3, "the backoff doubles");
    assert_eq!(h.seat_status("w1").await["wake_failed"], true);
    // New mail clears the failure and wakes at once.
    h.api.send(&["w1"], &[], "anyone?", false).await;
    h.wait("wake for new mail", || async { inbox.count() == 4 })
        .await;
    assert_eq!(h.seat_status("w1").await["wake_failed"], false);
    h.wait("wake_failed cleared on the server", || async {
        h.api.get("/seats/w1").await["presence"]["wake_failed"] != true
    })
    .await;
    // A turn delivers both.
    let v = h.hook("claude", env, claude("UserPromptSubmit")).await;
    let t = hook_text(&v).unwrap();
    assert!(t.contains("hello?") && t.contains("anyone?"), "{t}");
    assert_eq!(h.seat_status("w1").await["wake_failed"], false);
}

/// Hooks and wake-ups racing: every message is injected exactly once.
async fn hook_and_wake_inject_once(b: Backend) {
    let opts = Opts {
        wake_window: Duration::from_millis(50),
        wake_confirm: Duration::from_secs(3),
        ..Default::default()
    };
    let Some(h) = setup(b, "hook_and_wake_inject_once", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let seen: Arc<std::sync::Mutex<Vec<i64>>> = Arc::default();
    let sock = h.dir.path().join("cc.sock");
    let env = claude_env("w1", &sock);
    // The fake session answers every wake-up like Claude Code does: a
    // UserPromptSubmit hook, then a Stop hook.
    let on_line: Arc<dyn Fn(String) + Send + Sync> = {
        let (node_sock, env, seen) = (h.sock.clone(), env.clone(), seen.clone());
        Arc::new(move |_line| {
            let e: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            for name in ["UserPromptSubmit", "Stop"] {
                let (out, _, _) = run_hook(
                    &node_sock,
                    "claude",
                    &e,
                    &fixture("claude", name).to_string(),
                    Some(10_000),
                );
                if let Ok(v) = serde_json::from_str::<Value>(&out) {
                    seen.lock()
                        .unwrap()
                        .extend(injected_ids(&hook_text(&v).unwrap_or_default()));
                }
            }
        })
    };
    let inbox = FakeInbox::start(sock.clone(), Some(on_line));
    h.hook("claude", env.clone(), claude("SessionStart")).await;

    // Meanwhile the "agent" runs turns of its own (prompt, tool calls, stop,
    // then idle for a while), and a second thread adds parallel tool calls.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut workers = Vec::new();
    for w in 0..2u64 {
        let (node_sock, env, seen, stop) =
            (h.sock.clone(), env.clone(), seen.clone(), stop.clone());
        workers.push(std::thread::spawn(move || {
            let e: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let mut i = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let turn: &[&str] = if w == 0 {
                    &["UserPromptSubmit", "PostToolUse", "PostToolUse", "Stop"]
                } else {
                    &["PostToolUse"]
                };
                for name in turn {
                    let (out, _, _) = run_hook(
                        &node_sock,
                        "claude",
                        &e,
                        &fixture("claude", name).to_string(),
                        Some(10_000),
                    );
                    if let Ok(v) = serde_json::from_str::<Value>(&out) {
                        seen.lock()
                            .unwrap()
                            .extend(injected_ids(&hook_text(&v).unwrap_or_default()));
                    }
                }
                i += 1;
                // Idle between turns: long enough for wake-ups to happen.
                std::thread::sleep(Duration::from_millis(if w == 0 {
                    60 + (i * 37) % 160
                } else {
                    90
                }));
            }
        }));
    }
    let mut sent = Vec::new();
    for i in 0..40 {
        let (to, cc): (&[&str], &[&str]) = if i % 3 == 0 {
            (&[], &["w1"])
        } else {
            (&["w1"], &[])
        };
        sent.push(h.api.send(to, cc, &format!("race {i}"), i % 7 == 0).await);
        tokio::time::sleep(Duration::from_millis(25 + (i % 5) * 20)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for w in workers {
        w.join().unwrap();
    }
    // Sweep whatever is still pending, then let in-flight wake-ups finish.
    for _ in 0..50 {
        let v = h
            .hook("claude", env.clone(), claude("UserPromptSubmit"))
            .await;
        seen.lock()
            .unwrap()
            .extend(injected_ids(&hook_text(&v).unwrap_or_default()));
        let p = &h.seat_status("w1").await["pending"];
        if p["to"] == 0 && p["cc"] == 0 && p["list"] == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut got = seen.lock().unwrap().clone();
    let n = got.len();
    got.sort();
    got.dedup();
    eprintln!(
        "race: {} messages, {} injections, {} wake-ups",
        sent.len(),
        n,
        inbox.count()
    );
    assert!(inbox.count() > 0, "the race never involved a wake-up");
    assert_eq!(n, got.len(), "a message was injected twice");
    sent.sort();
    assert_eq!(got, sent, "every message injected exactly once");
}

/// Server unreachable: hooks get empty answers fast; local state is kept.
/// Node unreachable or silent: the hook client exits 0 with no output.
async fn server_down_answers_nothing(b: Backend) {
    let opts = Opts {
        server_down: true,
        ..Default::default()
    };
    let Some(h) = setup(b, "server_down_answers_nothing", opts).await else {
        return;
    };
    let inbox_path = h.dir.path().join("cc.sock");
    let env = claude_env("w1", &inbox_path);
    for name in ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"] {
        assert_eq!(
            h.hook("claude", env.clone(), claude(name)).await,
            Value::Null,
            "{name}"
        );
    }
    assert_eq!(h.state("w1").await, "idle");
    assert_eq!(h.status().await["connected"], false);

    // No node at all.
    let missing = h.dir.path().join("nope.sock");
    let (out, dt, ok) = run_hook(&missing, "claude", &[("AMAIL_SEAT", "w1")], "{}", None);
    assert!(ok && out.is_empty());
    assert!(dt < Duration::from_millis(1000), "{dt:?}");
    // A node that accepts and never answers: the client gives up at its budget.
    let silent = h.dir.path().join("silent.sock");
    let l = std::os::unix::net::UnixListener::bind(&silent).unwrap();
    let hold = std::thread::spawn(move || {
        let c = l.accept();
        std::thread::sleep(Duration::from_secs(2));
        drop(c);
    });
    let (out, dt, ok) = run_hook(
        &silent,
        "claude",
        &[("AMAIL_SEAT", "w1")],
        &claude("PostToolUse").to_string(),
        Some(300),
    );
    assert!(ok && out.is_empty());
    assert!(
        dt >= Duration::from_millis(250) && dt < Duration::from_millis(1500),
        "{dt:?}"
    );
    hold.join().unwrap();
}

/// A hook answer the client never reads (it went away) is kept and shown by
/// the seat's next injecting hook; while kept, it wakes an idle seat.
async fn lost_answer_is_kept(b: Backend) {
    let Some(h) = setup(b, "lost_answer_is_kept", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let m = h.api.send(&["w1"], &[], "do not lose me", false).await;
    h.wait_pending("w1", 1, 0, 0).await;

    // A client that sends a PostToolUse hook and disconnects before reading.
    let req = amail_node::wire::Request::Hook {
        harness: "claude".into(),
        env: env.iter().cloned().collect(),
        input: claude("PostToolUse").to_string(),
        budget_ms: Some(10_000),
    };
    {
        use std::io::Write;
        let mut c = std::os::unix::net::UnixStream::connect(&h.sock).unwrap();
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        c.write_all(&line).unwrap();
    } // dropped: closed before the answer
    h.wait("stashed", || async {
        h.seat_status("w1").await["stashed"] == 1
    })
    .await;
    // The mail was claimed (and, being short, read) but never shown...
    let st = h.api.get(&format!("/messages/{m}/status")).await;
    assert_eq!(st["recipients"][0]["state"], "read");
    // ...so the next injecting hook shows it.
    let v = h.hook("claude", env.clone(), claude("PostToolUse")).await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m]);
    assert_eq!(h.seat_status("w1").await["stashed"], 0);
    assert_eq!(
        h.hook("claude", env.clone(), claude("PostToolUse")).await,
        Value::Null
    );

    // Kept mail of an idle seat wakes it; Stop delivers it too.
    let m2 = h.api.send(&["w1"], &[], "second", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    let req = amail_node::wire::Request::Hook {
        harness: "claude".into(),
        env: env.iter().cloned().collect(),
        input: claude("PostToolUse").to_string(),
        budget_ms: Some(10_000),
    };
    {
        use std::io::Write;
        let mut c = std::os::unix::net::UnixStream::connect(&h.sock).unwrap();
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        c.write_all(&line).unwrap();
    }
    h.wait("stashed", || async {
        h.seat_status("w1").await["stashed"] == 1
    })
    .await;
    let v = h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(v["decision"], "block");
    assert_eq!(injected_ids(v["reason"].as_str().unwrap()), vec![m2]);
    h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(h.state("w1").await, "idle");
    assert_eq!(inbox.count(), 0);

    // A Stop block that is never delivered means the session did stop: the
    // seat goes idle and the kept mail wakes it.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let m3 = h.api.send(&["w1"], &[], "third", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    let req = amail_node::wire::Request::Hook {
        harness: "claude".into(),
        env: env.iter().cloned().collect(),
        input: claude("Stop").to_string(),
        budget_ms: Some(10_000),
    };
    {
        use std::io::Write;
        let mut c = std::os::unix::net::UnixStream::connect(&h.sock).unwrap();
        let mut line = serde_json::to_vec(&req).unwrap();
        line.push(b'\n');
        c.write_all(&line).unwrap();
    }
    h.wait("idle after lost stop block", || async {
        h.state("w1").await == "idle"
    })
    .await;
    h.wait("wake for kept mail", || async { inbox.count() == 1 })
        .await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m3]);
}

/// After a node restart, a live idle Claude seat is still woken.
async fn restart_restores_idle_seat(b: Backend) {
    let Some(mut h) = setup(b, "restart_restores_idle_seat", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    assert_eq!(h.state("w1").await, "idle");
    h.restart_node().await;
    assert_eq!(h.state("w1").await, "idle");
    h.api.send(&["w1"], &[], "after restart", false).await;
    h.wait("wake after restart", || async { inbox.count() == 1 })
        .await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert!(hook_text(&v).unwrap().contains("after restart"));

    // A session whose socket is gone is not restored.
    h.hook("claude", env.clone(), claude("Stop")).await;
    drop(inbox);
    std::fs::remove_file(h.dir.path().join("cc.sock")).unwrap();
    h.restart_node().await;
    assert_ne!(h.state("w1").await, "idle");
    assert!(h.seat_status("w1").await["session"].is_null());
}

/// Mail injected with its whole body counts as read; longer mail as notified.
async fn inlined_mail_is_read(b: Backend) {
    let Some(h) = setup(b, "inlined_mail_is_read", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let env = claude_env("w1", &h.dir.path().join("cc.sock"));
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let short = h.api.send(&["w1"], &[], "short", false).await;
    let long = h
        .api
        .post(
            "/messages",
            json!({"to": ["w1"], "subject": "long", "body": "x".repeat(500)}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    h.wait_pending("w1", 2, 0, 0).await;
    // With the client's default budget (300 ms), as in production.
    let (sock, input) = (h.sock.clone(), claude("PostToolUse").to_string());
    let (out, _, ok) = tokio::task::spawn_blocking(move || {
        let e: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        run_hook(&sock, "claude", &e, &input, None)
    })
    .await
    .unwrap();
    assert!(ok);
    let v: Value = serde_json::from_str(&out).unwrap();
    let t = hook_text(&v).unwrap();
    assert_eq!(injected_ids(&t), vec![short, long]);
    assert!(t.contains("  > body of short"), "{t}");
    assert!(
        t.contains("\"long\" · thread") && t.contains(" · 1 line"),
        "{t}"
    );
    let state = |id: i64| {
        let api = h.api.clone();
        async move {
            api.get(&format!("/messages/{id}/status")).await["recipients"][0]["state"]
                .as_str()
                .unwrap()
                .to_string()
        }
    };
    assert_eq!(state(short).await, "read");
    assert_eq!(state(long).await, "notified");
}
