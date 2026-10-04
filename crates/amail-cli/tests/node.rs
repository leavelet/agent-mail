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
    injection_fits_harness_limit,
    transcript_mirror_follows_file,
    focus_holds_mail,
    digest_at_natural_points,
    lost_claim_answer_is_recovered,
    socket_checks_callers,
    transcript_paths_are_checked,
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

    // SessionStart (resume) lists unread mail without claiming it, except
    // mail still waiting to be notified: the next claim shows that in full.
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("SessionStart"), &[("source", json!("resume"))]),
        )
        .await;
    assert_eq!(v, Value::Null);
    h.wait_pending("w1", 1, 1, 0).await;
    // UserPromptSubmit claims every class.
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m_to, m_cc]);
    h.wait_pending("w1", 0, 0, 0).await;
    // Mail shown before but still unread is listed at the next start.
    let long = h
        .api
        .post(
            "/messages",
            json!({"to": ["w1"], "subject": "long one", "body": "x".repeat(1000)}),
        )
        .await["id"]
        .as_i64()
        .unwrap();
    h.wait_pending("w1", 1, 0, 0).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![long]);
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("SessionStart"), &[("source", json!("resume"))]),
        )
        .await;
    let text = hook_text(&v).unwrap();
    assert!(
        text.starts_with("[amail] w1 has 1 unread message(s):"),
        "{text}"
    );
    assert_eq!(injected_ids(&text), vec![long]);
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
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
    let opts = Opts {
        check_callers: Some(true),
        ..Opts::default()
    };
    let Some(h) = setup(b, "codex_registration_and_rows", opts).await else {
        return;
    };
    h.seat("cx", "codex").await;
    let sid = "00000000-0000-7000-8000-00000000c0de";
    let mut env: Vec<(String, String)> = vec![(
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
            pane: None,
            mux_session: None,
            permission_mode: Some(amail_proto::PermissionMode::Unattended),
            git_access: None,
        })
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    assert!(plan.get("notice").is_none(), "{plan}");
    let args: Vec<String> = serde_json::from_value(plan["args"].clone()).unwrap();
    // No `-s`: the sandbox comes from the home's `amail` permissions profile.
    assert_eq!(&args[..4], ["-m", "test-model", "-a", "never"]);
    assert!(!args.iter().any(|a| a == "-s"), "{args:?}");
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
    // Codex hooks run in the shared app-server, whose environment holds the
    // node's Codex pass (so does the launch environment). Without it the
    // node does not act on them.
    let pass = plan["env"]["AMAIL_CODEX_PASS"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(pass.len(), 64);
    let no_pass = with(
        codex("UserPromptSubmit"),
        &[("session_id", json!(sid)), ("prompt", json!(prompt))],
    );
    assert_eq!(h.hook("codex", env.clone(), no_pass).await, Value::Null);
    env.push(("AMAIL_CODEX_PASS".into(), pass.clone()));
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
            pane: None,
            mux_session: None,
            permission_mode: None,
            git_access: None,
        })
        .await;
    assert!(
        bad["error"].as_str().unwrap().contains("not allowed"),
        "{bad}"
    );
    // The server's seat settings say "unattended", but this machine did
    // not choose it: the session keeps its approval prompts.
    let prompting = h
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
            permission_mode: None,
            git_access: None,
        })
        .await;
    assert!(prompting.get("error").is_none(), "{prompting}");
    let args: Vec<String> = serde_json::from_value(prompting["args"].clone()).unwrap();
    assert_eq!(&args[..2], ["-m", "test-model"]);
    assert!(!args.iter().any(|a| a == "-a" || a == "never"), "{args:?}");
    assert!(
        prompting["notice"]
            .as_str()
            .unwrap()
            .contains("keeps the harness's permission prompts"),
        "{prompting}"
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

    // Tool commands in the session get the seat's credentials from the node,
    // with the Codex pass; the thread id alone is not enough.
    let cred = socket_request(
        &h.sock,
        amail_node::wire::Request::Credentials {
            codex_thread: sid.into(),
            pass: Some(pass.clone()),
        },
    )
    .await;
    assert_eq!(cred["seat"], "cx");
    for pass in [None, Some("0".repeat(64))] {
        let refused = socket_request(
            &h.sock,
            amail_node::wire::Request::Credentials {
                codex_thread: sid.into(),
                pass,
            },
        )
        .await;
        assert!(refused["token"].is_null(), "{refused}");
        assert!(refused["error"].is_string(), "{refused}");
    }
    // Node status does not name the thread.
    assert!(!h.status().await.to_string().contains(sid));
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
            pass: Some(pass.clone()),
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
    // (In a directory only the user can write to: clients refuse others.)
    let private = h.dir.path().join("private");
    std::fs::create_dir(&private).unwrap();
    std::fs::set_permissions(
        &private,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let silent = private.join("silent.sock");
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
            json!({"to": ["w1"], "subject": "long", "body": "x".repeat(1000)}),
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
        t.contains("\"long\" · thread")
            && t.contains(&format!(
                " · body not shown (1 line): run amail read {long}"
            )),
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

/// Bodies that render much longer than they are (here: all line breaks)
/// do not push a hook's answer over the harness's limit: what does not fit
/// is kept and shown, whole, by the next hook; every message once.
async fn injection_fits_harness_limit(b: Backend) {
    let Some(h) = setup(b, "injection_fits_harness_limit", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let env = claude_env("w1", &h.dir.path().join("cc.sock"));
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let mut sent = Vec::new();
    for i in 0..4 {
        let body = format!("m{i}{}", "\n".repeat(790));
        let v = h
            .api
            .post(
                "/messages",
                json!({"to": ["w1"], "subject": format!("s{i}"), "body": body}),
            )
            .await;
        sent.push(v["id"].as_i64().unwrap());
    }
    h.wait_pending("w1", 4, 0, 0).await;
    let mut seen = Vec::new();
    for _ in 0..4 {
        let v = h.hook("claude", env.clone(), claude("PostToolUse")).await;
        let Some(t) = hook_text(&v) else { break };
        // Claude Code cuts hook context above 10,000 characters.
        assert!(t.chars().count() <= 9000, "{}", t.chars().count());
        let ids = injected_ids(&t);
        assert!(!ids.is_empty() && ids.len() < 4, "{t}");
        for id in &ids {
            assert!(t.contains(&format!(
                "  > m{}",
                sent.iter().position(|s| s == id).unwrap()
            )));
        }
        seen.extend(ids);
        if seen.len() == 4 {
            break;
        }
        assert!(t.contains("More mail is waiting"), "{t}");
    }
    assert_eq!(seen, sent);
}

/// Server-sent events of a response, as `(event, data)`.
struct Sse {
    stream: futures::stream::BoxStream<'static, reqwest::Result<Vec<u8>>>,
    buf: String,
}

impl Sse {
    async fn open(api: &Api, path: &str) -> Sse {
        use futures::StreamExt;
        let r = api
            .http
            .get(format!("{}/v1{path}", api.base))
            .bearer_auth(&api.token)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "GET {path}: {}", r.status());
        Sse {
            stream: r.bytes_stream().map(|c| c.map(|b| b.to_vec())).boxed(),
            buf: String::new(),
        }
    }

    async fn next(&mut self, secs: u64) -> Option<(String, Value)> {
        use futures::StreamExt;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(i) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..i + 2).collect();
                let mut event = String::new();
                let mut data = None;
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data = serde_json::from_str(v.trim()).ok();
                    }
                }
                if let Some(d) = data {
                    return Some((event, d));
                }
                continue;
            }
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Ok(Some(Ok(c))) => self.buf.push_str(&String::from_utf8_lossy(&c)),
                _ => return None,
            }
        }
    }

    /// Texts of the `events` received until `n` have arrived; panics on
    /// timeout. A `reset` clears what was collected.
    async fn texts(&mut self, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        while out.len() < n {
            let (ev, d) = self.next(10).await.expect("timed out waiting for events");
            match ev.as_str() {
                "reset" => out.clear(),
                "events" => {
                    for e in d["events"].as_array().unwrap() {
                        out.push(format!(
                            "{}/{}:{}",
                            e["role"].as_str().unwrap(),
                            e["kind"].as_str().unwrap(),
                            e["text"].as_str().unwrap()
                        ));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

fn claude_line(v: Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

fn append(path: &std::path::Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// The conversation mirror end to end: a hook reports the transcript file,
/// a browser watches, the node backfills and follows the growing file
/// (including a line written in two parts), switches to the file of a new
/// session, and stops when the lease runs out after the browser left.
async fn transcript_mirror_follows_file(b: Backend) {
    let opts = Opts {
        tweak: Some(|c| c.mirror_poll = Duration::from_millis(100)),
        server_tweak: Some(|s| {
            s.mirror_config.lease = Duration::from_millis(1500);
            s.mirror_config.answer_timeout = Duration::from_millis(500);
        }),
        ..Opts::default()
    };
    let Some(h) = setup(b, "transcript_mirror_follows_file", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    let projects = h.dir.path().join("claude/projects/p");
    std::fs::create_dir_all(&projects).unwrap();
    let file = projects.join("session-1.jsonl");
    let user = |t: &str| {
        claude_line(json!({"type": "user", "timestamp": "2026-10-04T10:00:00Z",
        "message": {"role": "user", "content": t}}))
    };
    let say = |t: &str| {
        claude_line(
            json!({"type": "assistant", "timestamp": "2026-10-04T10:00:01Z",
        "message": {"role": "assistant", "content": [{"type": "text", "text": t}]}}),
        )
    };
    append(&file, &user("Run the tests."));
    append(&file, &say("Running them now."));
    append(&file, "not json at all\n");
    append(&file, &user("[amail] You have new mail (wake #1)."));

    // Before any hook no node knows the seat; the server says so.
    let mut w = Sse::open(&h.api, "/seats/w1/transcript").await;
    let mut states = Vec::new();
    while let Some((ev, d)) = w.next(5).await {
        if ev == "status" {
            states.push(d["state"].as_str().unwrap().to_string());
            if d["state"] == "unknown" {
                break;
            }
        }
    }
    assert_eq!(states, vec!["waiting", "unknown"]);

    // A hook reports the transcript: the watcher gets the backfill.
    h.hook(
        "claude",
        env.clone(),
        with(
            claude("UserPromptSubmit"),
            &[("transcript_path", json!(file))],
        ),
    )
    .await;
    assert_eq!(
        w.texts(3).await,
        vec![
            "user/text:Run the tests.",
            "assistant/text:Running them now.",
            "user/notice:[amail] You have new mail (wake #1).",
        ]
    );
    // Growth, including a line that arrives in two writes.
    let line = say("All 12 tests pass.");
    let (a, rest) = line.split_at(20);
    append(&file, a);
    tokio::time::sleep(Duration::from_millis(300)).await;
    append(&file, rest);
    assert_eq!(w.texts(1).await, vec!["assistant/text:All 12 tests pass."]);
    let st = h.status().await;
    assert_eq!(st["mirrors"], json!(["w1"]));

    // A new session (another transcript file): reset, then its backfill.
    let file2 = projects.join("session-2.jsonl");
    append(&file2, &user("Second session."));
    h.hook(
        "claude",
        env.clone(),
        with(
            claude("UserPromptSubmit"),
            &[
                ("transcript_path", json!(file2)),
                ("session_id", json!("00000000-0000-4000-8000-000000000002")),
            ],
        ),
    )
    .await;
    loop {
        let (ev, _) = w.next(10).await.expect("reset");
        if ev == "reset" {
            break;
        }
    }
    assert_eq!(w.texts(1).await, vec!["user/text:Second session."]);

    // The browser leaves: the node stops when the lease runs out.
    drop(w);
    h.wait("mirror stopped", || async {
        h.status().await["mirrors"] == json!([])
    })
    .await;
}

/// In focus only urgent mail is shown or wakes the seat; held mail comes
/// when the focus ends (or is ended).
async fn focus_holds_mail(b: Backend) {
    let opts = Opts {
        wake_window: Duration::from_millis(100),
        ..Default::default()
    };
    let Some(h) = setup(b, "focus_holds_mail", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    h.api
        .call(
            reqwest::Method::PUT,
            "/focus?seat=w1",
            Some(json!({ "for_s": 3 })),
        )
        .await;
    h.wait("focus known", || async {
        h.seat_status("w1").await["seat"] == "w1"
    })
    .await;
    let normal = h.api.send(&["w1"], &[], "can wait", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(inbox.count(), 0, "focus: no wake-up for normal mail");
    // Busy in focus: PostToolUse and Stop show nothing normal.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("PostToolUse")).await,
        Value::Null
    );
    let urgent = h.api.send(&[], &["w1"], "now", true).await;
    h.wait_pending("w1", 1, 1, 0).await;
    let v = h.hook("claude", env.clone(), claude("PostToolUse")).await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![urgent]);
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    assert_eq!(h.state("w1").await, "idle");
    // The focus ends: the held mail wakes the seat.
    h.wait("wake-up after focus", || async { inbox.count() == 1 })
        .await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![normal]);
    h.hook("claude", env.clone(), claude("Stop")).await;
    // Ending a focus early delivers at once.
    h.api
        .call(
            reqwest::Method::PUT,
            "/focus?seat=w1",
            Some(json!({ "for_s": 600 })),
        )
        .await;
    let held = h.api.send(&["w1"], &[], "held", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(inbox.count(), 1);
    h.api
        .call(reqwest::Method::DELETE, "/focus?seat=w1", None)
        .await;
    h.wait("wake-up after focus off", || async { inbox.count() == 2 })
        .await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![held]);
}

/// Digest mail is not shown on its own: the next UserPromptSubmit or Stop
/// shows a digest; an idle seat is woken for it once per interval.
async fn digest_at_natural_points(b: Backend) {
    let opts = Opts {
        wake_window: Duration::from_millis(100),
        tweak: Some(|c| c.digest_every = Some(Duration::from_secs(2))),
        ..Default::default()
    };
    let Some(h) = setup(b, "digest_at_natural_points", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    h.api
        .post("/lists", json!({ "name": "news", "post_policy": "anyone" }))
        .await;
    h.api
        .call(
            reqwest::Method::PUT,
            "/lists/news/members/w1",
            Some(json!({ "mode": "digest" })),
        )
        .await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let d1 = h.api.send(&["news"], &[], "Release notes", false).await;
    let d2 = h.api.send(&["news"], &[], "Re: Release notes", false).await;
    h.wait("digest pending", || async {
        h.seat_status("w1").await["pending"]["to"] == 0
            && h.api.get("/pending?seat=w1").await["seats"][0]["digest"] == 2
    })
    .await;
    // Not between tool calls...
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("PostToolUse")).await,
        Value::Null
    );
    // ...but at Stop, as one digest line per thread.
    let v = h.hook("claude", env.clone(), claude("Stop")).await;
    let t = hook_text(&v).unwrap();
    assert!(
        t.starts_with("[amail] Digest for w1: 2 message(s) in 2 thread(s):"),
        "{t}"
    );
    assert!(
        t.contains(&format!("latest #{d1}")) && t.contains(&format!("latest #{d2}")),
        "{t}"
    );
    assert_eq!(v["decision"], "block");
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    assert_eq!(h.state("w1").await, "idle");
    // Idle: no wake-up until the digest has waited an interval, then one.
    let t0 = Instant::now();
    let d3 = h.api.send(&["news"], &[], "Later", false).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(inbox.count(), 0);
    h.wait("digest wake-up", || async { inbox.count() == 1 })
        .await;
    assert!(
        t0.elapsed() >= Duration::from_millis(1800),
        "{:?}",
        t0.elapsed()
    );
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let t = hook_text(&v).unwrap();
    assert!(t.contains(&format!("latest #{d3}")), "{t}");
    h.hook("claude", env.clone(), claude("Stop")).await;
    // Digest mail stays unread (it was only summarized).
    let st = h.api.get(&format!("/messages/{d3}/status")).await;
    assert_eq!(st["recipients"][0]["state"], "notified");
}

/// A claim whose answer never reaches the node (it times out) is retried
/// with the same key: the mail it took is shown once, with the next hook.
async fn lost_claim_answer_is_recovered(b: Backend) {
    let opts = Opts {
        slow_claims: Some((1, Duration::from_millis(1500))),
        tweak: Some(|c| c.hook_server_timeout = Duration::from_millis(300)),
        ..Default::default()
    };
    let Some(h) = setup(b, "lost_claim_answer_is_recovered", opts).await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let m = h.api.send(&["w1"], &[], "slow answer", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    // The claim goes through on the server, its answer comes too late.
    let v = h.hook("claude", env.clone(), claude("PostToolUse")).await;
    assert_eq!(v, Value::Null);
    let st = h.api.get(&format!("/messages/{m}/status")).await;
    assert_eq!(st["recipients"][0]["notified_via"], "hook:PostToolUse");
    // The node asks again with the same key and keeps what it took.
    h.wait("kept", || async {
        h.seat_status("w1").await["stashed"] == 1
    })
    .await;
    let v = h.hook("claude", env.clone(), claude("PostToolUse")).await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m]);
    for _ in 0..3 {
        assert_eq!(
            h.hook("claude", env.clone(), claude("PostToolUse")).await,
            Value::Null
        );
    }
    assert_eq!(h.status().await["seats"][0]["stashed"], 0);
}

/// A node socket in a directory other users can write to (where another
/// user could have put it, such as a shared `/tmp` directory) is refused by
/// every client: the commands its answers name are never run.
#[test]
fn untrusted_socket_is_refused() {
    use std::io::{BufRead, Write};
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
    let sock = shared.join("node.sock");
    let marker = dir.path().join("ran");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let m = marker.display().to_string();
    std::thread::spawn(move || {
        for c in listener.incoming() {
            // Answers both node requests and hook requests.
            let Ok(mut c) = c else { return };
            let mut line = String::new();
            let _ = std::io::BufReader::new(&c).read_line(&mut line);
            let cmd = vec!["/bin/sh".to_string(), "-c".into(), format!("touch {m}")];
            let resp = json!({"commands": [cmd], "program": "/bin/sh",
                              "args": ["-c", format!("touch {m}")], "env": {}, "stdout": "x"});
            let _ = writeln!(c, "{resp}");
        }
    });
    for args in [
        vec!["attach", "w1"],
        vec!["run", "w1", "--harness", "claude"],
        vec!["ps"],
    ] {
        let out = std::process::Command::new(BIN)
            .args(&args)
            .env("AMAIL_NODE_SOCK", &sock)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("writable by other users"), "{args:?}: {err}");
    }
    // The hook client prints nothing.
    let (out, _, _) = run_hook(&sock, "claude", &[], "{}", None);
    assert_eq!(out, "");
    assert!(!marker.exists());
    // The same socket in a private directory is used.
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (out, _, _) = run_hook(&sock, "claude", &[], "{}", None);
    assert_eq!(out, "x");
}

/// The node socket serves a request according to the process that sends it
/// (the caller's process id comes from the socket). A seat's session gets
/// its own mail but not another seat's, and cannot launch seats or read
/// node status; a process the node did not launch cannot act as a seat;
/// Codex credentials need the node's Codex pass and are never given to a
/// Claude Code session; a launch records the process that asked.
async fn socket_checks_callers(b: Backend) {
    // Fake harness binaries; caller checks on.
    let opts = Opts {
        supervise: true,
        ..Opts::default()
    };
    let Some(h) = setup(b, "socket_checks_callers", opts).await else {
        return;
    };
    assert!(h.cfg.check_callers);
    h.seat("w1", "claude").await;
    h.seat("w2", "claude").await;
    h.seat("cx", "codex").await;
    let work = h.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();

    // Seat w1's session, started the way a person starts one.
    let mut session = std::process::Command::new(BIN);
    session
        .args(["run", "w1", "--socket"])
        .arg(&h.sock)
        .current_dir(&work)
        .env("FAKE_AMAIL", BIN)
        .env("AMAIL_HOOK_TIMEOUT_MS", "5000")
        // Its own Claude Code configuration directory (not the node's).
        .env("CLAUDE_CONFIG_DIR", h.dir.path().join("cc2"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for k in [
        "AMAIL_SEAT",
        "AMAIL_TOKEN",
        "AMAIL_PROFILE",
        "AMAIL_CODEX_PASS",
    ] {
        session.env_remove(k);
    }
    let mut session = session.spawn().unwrap();
    h.wait("w1 started", || async { h.state("w1").await == "idle" })
        .await;

    // A Codex seat with a registered thread. Its session process is a
    // stand-in that does nothing.
    let mut stand_in = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let plan = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Launch {
            seat: "cx".into(),
            cwd: work.to_string_lossy().into(),
            pid: stand_in.id(),
            harness: None,
            extra: vec![],
            pane: None,
            mux_session: None,
            permission_mode: None,
            git_access: None,
        })
        .await;
    let pass = plan["env"]["AMAIL_CODEX_PASS"]
        .as_str()
        .unwrap()
        .to_string();
    let prompt = plan["args"].as_array().unwrap().last().unwrap().clone();
    let tid = "00000000-0000-7000-8000-0000000000c1";
    let first = with(
        codex("UserPromptSubmit"),
        &[("session_id", json!(tid)), ("prompt", prompt)],
    );
    h.hook(
        "codex",
        vec![("AMAIL_CODEX_PASS".into(), pass.clone())],
        first,
    )
    .await;
    h.wait("cx registered", || async { h.state("cx").await == "busy" })
        .await;

    let m1 = h.api.send(&["w1"], &[], "mail for w1", false).await;
    h.api.send(&["w2"], &[], "mail for w2", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    h.wait_pending("w2", 1, 0, 0).await;

    // A process the node did not launch cannot act as w1.
    let up = claude("UserPromptSubmit");
    let env = vec![("AMAIL_SEAT".to_string(), "w1".to_string())];
    assert_eq!(h.hook("claude", env, up.clone()).await, Value::Null);

    // Inside w1's session, as an agent's tool command would.
    std::fs::write(h.dir.path().join("up.json"), up.to_string()).unwrap();
    let own = h.dir.path().join("cc2/projects/p/s.jsonl");
    let with_transcript = with(up.clone(), &[("transcript_path", json!(own))]);
    std::fs::write(h.dir.path().join("upt.json"), with_transcript.to_string()).unwrap();
    let script = format!(
        r#"B="$FAKE_AMAIL"
cd '{dir}'
mkdir -p nocfg
export AMAIL_CONFIG_DIR="$PWD/nocfg" XDG_CONFIG_HOME="$PWD/nocfg"
timeout 10 "$B" run w2 --harness claude; echo "run-exit=$?"
timeout 10 "$B" node status; echo "status-exit=$?"
timeout 10 "$B" ps; echo "ps-exit=$?"
AMAIL_SEAT=w2 "$B" hook claude < up.json > w2.out; echo "w2-hook-bytes=$(wc -c < w2.out)"
"$B" hook claude < up.json > w1.out; echo "w1-hook-bytes=$(wc -c < w1.out)"
"$B" hook claude < upt.json > /dev/null
env -u AMAIL_TOKEN CODEX_THREAD_ID={tid} AMAIL_CODEX_PASS={pass} timeout 10 "$B" inbox; echo "codex-cred-exit=$?"
"#,
        dir = h.dir.path().display(),
    );
    let fake = h.fake.clone();
    std::fs::write(fake.join("w1.run"), script).unwrap();
    h.wait("the session ran the commands", || async {
        fake.join("w1.ran").exists()
    })
    .await;
    let ran = std::fs::read_to_string(fake.join("w1.ran")).unwrap();
    for want in [
        "run-exit=1",
        "status-exit=1",
        "ps-exit=1",
        "w2-hook-bytes=0",
        "codex-cred-exit=1",
        "this request is for the person, not for agent sessions",
    ] {
        assert!(ran.contains(want), "missing {want:?} in:\n{ran}");
    }
    // No token was printed (tokens start with `amail_`).
    assert!(!ran.contains("amail_"), "{ran}");
    // w2's mail was not touched; w1's session got its own.
    let p = &h.seat_status("w2").await["pending"];
    assert_eq!(p["to"], 1, "{p}");
    let w1_out: Value =
        serde_json::from_str(&std::fs::read_to_string(h.dir.path().join("w1.out")).unwrap())
            .unwrap();
    assert_eq!(injected_ids(&hook_text(&w1_out).unwrap()), vec![m1]);

    // The session's transcript lies under its own Claude Code configuration
    // directory, which the node read from the session's environment.
    let state = h.dir.path().join("state/node-state.json");
    h.wait("w1's transcript is saved", || async {
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        saved["transcripts"]["w1"]["path"] == json!(own)
    })
    .await;

    // Outside the sessions: Codex credentials need the pass.
    let req = |pass: Option<String>| amail_node::wire::Request::Credentials {
        codex_thread: tid.into(),
        pass,
    };
    let refused = socket_request(&h.sock, req(None)).await;
    assert!(refused["token"].is_null(), "{refused}");
    let cred = socket_request(&h.sock, req(Some(pass.clone()))).await;
    assert_eq!(cred["seat"], "cx", "{cred}");

    // A process that runs with the node's Codex home (Codex runs agents'
    // commands in its app-server, under that home) is a session too.
    let out = std::process::Command::new(BIN)
        .args(["node", "status"])
        .env("AMAIL_NODE_SOCK", &h.sock)
        .env("CODEX_HOME", h.dir.path().join("state/codex-home"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("for the person"),
        "{out:?}"
    );
    // Without it, the same command is the person's.
    let out = std::process::Command::new(BIN)
        .args(["node", "status"])
        .env("AMAIL_NODE_SOCK", &h.sock)
        .env_remove("CODEX_HOME")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    // A person's launch: allowed; the process that asked becomes the
    // session, whatever pid the request names.
    let plan = socket_request(
        &h.sock,
        amail_node::wire::Request::Launch {
            seat: "w2".into(),
            cwd: work.to_string_lossy().into(),
            pid: 1,
            harness: Some("claude".into()),
            extra: vec![],
            pane: None,
            mux_session: None,
            permission_mode: None,
            git_access: None,
        },
    )
    .await;
    assert!(plan["env"]["AMAIL_TOKEN"].is_string(), "{plan}");
    h.wait("the launch is saved", || async {
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        saved["roots"]["w2"]["pid"] == std::process::id()
    })
    .await;
    // This process now counts as w2's session: requests for the person are
    // refused to it.
    let st = socket_request(&h.sock, amail_node::wire::Request::Status).await;
    assert!(
        st["error"]
            .as_str()
            .unwrap_or_default()
            .contains("for the person"),
        "{st}"
    );

    session.kill().ok();
    session.wait().ok();
    stand_in.kill().ok();
    stand_in.wait().ok();
}

/// The node follows a transcript only where the harness writes them: for
/// Claude Code under `projects` of its configuration directory, for Codex
/// under the dedicated home's `sessions`. Other paths in hooks are ignored.
async fn transcript_paths_are_checked(b: Backend) {
    let Some(h) = setup(b, "transcript_paths_are_checked", Opts::default()).await else {
        return;
    };
    h.seat("w1", "claude").await;
    h.seat("w2", "claude").await;
    h.seat("cx", "codex").await;
    let d = h.dir.path();
    let projects = d.join("claude/projects");
    std::fs::create_dir_all(projects.join("p")).unwrap();
    let outside = d.join("outside.jsonl");
    std::fs::write(&outside, "{}\n").unwrap();
    std::os::unix::fs::symlink(&outside, projects.join("p/link.jsonl")).unwrap();
    let claude_hook = |seat: &str, path: &std::path::Path| {
        let env = vec![("AMAIL_SEAT".to_string(), seat.to_string())];
        let input = with(
            claude("UserPromptSubmit"),
            &[("transcript_path", json!(path))],
        );
        h.hook("claude", env, input)
    };
    for bad in [
        outside.clone(),
        projects.join("p/link.jsonl"),
        projects.join("p/../../outside.jsonl"),
        std::path::PathBuf::from("/etc/passwd"),
    ] {
        claude_hook("w1", &bad).await;
    }
    let good = projects.join("p/s.jsonl");
    claude_hook("w2", &good).await;

    // A Codex session (registered by its marker) reports its rollout file.
    let stand_in = d.join("work");
    std::fs::create_dir_all(&stand_in).unwrap();
    let plan = h
        .node
        .as_ref()
        .unwrap()
        .node
        .handle(amail_node::wire::Request::Launch {
            seat: "cx".into(),
            cwd: stand_in.to_string_lossy().into(),
            pid: std::process::id(),
            harness: None,
            extra: vec![],
            pane: None,
            mux_session: None,
            permission_mode: None,
            git_access: None,
        })
        .await;
    let prompt = plan["args"].as_array().unwrap().last().unwrap().clone();
    let tid = "00000000-0000-7000-8000-0000000000c2";
    let rollout = d.join("state/codex-home/sessions/2026/10/04/rollout-x.jsonl");
    let first = with(
        codex("UserPromptSubmit"),
        &[
            ("session_id", json!(tid)),
            ("prompt", prompt),
            ("transcript_path", json!(rollout)),
        ],
    );
    h.hook("codex", vec![], first).await;
    // A Codex hook naming a Claude Code transcript is ignored.
    let other = with(
        codex("PostToolUse"),
        &[("session_id", json!(tid)), ("transcript_path", json!(good))],
    );
    h.hook("codex", vec![], other).await;

    let state = d.join("state/node-state.json");
    h.wait("the transcripts are saved", || async {
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap_or_default())
                .unwrap_or(Value::Null);
        saved["transcripts"]["w2"]["path"] == json!(good)
            && saved["transcripts"]["cx"]["path"] == json!(rollout)
    })
    .await;
    // Give a late write the chance to show up, then look again.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    assert!(
        saved["transcripts"]["w1"].is_null(),
        "{}",
        saved["transcripts"]
    );
    assert_eq!(saved["transcripts"]["cx"]["path"], json!(rollout));
}

/// A server that sends an endless line on the event stream and pending
/// counts for thousands of seats, some with names that are not seat names:
/// the node drops the stream and connects again, and keeps state for at
/// most `NODE_MAX_OTHER_SEATS` seats it does not host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_server_is_bounded() {
    use axum::body::{Body, Bytes};
    use axum::routing::{get, post};
    use futures::StreamExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let max = amail_proto::limits::NODE_MAX_OTHER_SEATS;
    let streams = Arc::new(AtomicUsize::new(0));
    let n = streams.clone();
    let mut seats: Vec<Value> = (0..max + 500)
        .map(|i| json!({"seat": format!("s{i}"), "to": 1, "cc": 0, "list": 0, "urgent": 0, "seq": 1}))
        .collect();
    for bad in ["Not A Seat", &"x".repeat(5000), "../x"] {
        seats.push(json!({"seat": bad, "to": 1, "cc": 0, "list": 0, "urgent": 0, "seq": 1}));
    }
    let pending = json!({ "seats": seats });
    let app = axum::Router::new()
        .route(
            "/v1/events",
            get(move || {
                let first = n.fetch_add(1, Ordering::SeqCst) == 0;
                async move {
                    if first {
                        // 64 MiB without a newline, and the stream stays
                        // open.
                        let chunk = Bytes::from(vec![b'x'; 64 * 1024]);
                        let s = futures::stream::repeat(chunk)
                            .take(1024)
                            .map(Ok::<_, std::io::Error>)
                            .chain(futures::stream::pending());
                        Body::from_stream(s)
                    } else {
                        // Mailbox events for more unknown seats, then quiet.
                        let events: String = (0..max + 500)
                            .map(|i| {
                                format!("event: mailbox\ndata: {{\"seat\":\"m{i}\",\"seq\":5}}\n\n")
                            })
                            .collect();
                        let s = futures::stream::once(async move {
                            Ok::<_, std::io::Error>(Bytes::from(events))
                        })
                        .chain(futures::stream::pending());
                        Body::from_stream(s)
                    }
                }
            }),
        )
        .route(
            "/v1/pending",
            get(move || {
                let p = pending.clone();
                async move { axum::Json(p) }
            }),
        )
        .route("/v1/presence", post(|| async { "" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = amail_node::Config::new(&base, "amail_test");
    cfg.state_dir = dir.path().join("state");
    cfg.socket = dir.path().join("run/node.sock");
    cfg.codex_bin = "/bin/false".into();
    cfg.claude_bin = "/bin/false".into();
    cfg.zellij_bin = "/bin/false".into();
    cfg.codex_auth = dir.path().join("no-auth.json");
    cfg.claude_config_dir = dir.path().join("claude");
    let node = amail_node::start(cfg).await.unwrap();
    let status = || node.node.handle(amail_node::wire::Request::Status);
    let t0 = Instant::now();
    loop {
        let st = status().await;
        let n = st["seats"].as_array().map_or(0, Vec::len);
        if streams.load(Ordering::SeqCst) >= 2 && st["connected"] == true && n >= max {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "{st}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Let the mailbox events arrive, then count.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let st = status().await;
    let names: Vec<&str> = st["seats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["seat"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), max);
    assert!(names.iter().all(|n| amail_proto::valid_name(n)));

    // A request on the socket longer than the limit is refused.
    let sock = dir.path().join("run/node.sock");
    let answer = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, Write};
        let s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        let mut w = s.try_clone().unwrap();
        let writer = std::thread::spawn(move || {
            let chunk = vec![b'x'; 1024 * 1024];
            for _ in 0..=amail_proto::limits::NODE_MAX_REQUEST_BYTES / chunk.len() {
                if w.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
        let mut line = String::new();
        std::io::BufReader::new(s).read_line(&mut line).unwrap();
        writer.join().unwrap();
        line
    })
    .await
    .unwrap();
    assert!(answer.contains("larger than"), "{answer}");
    node.shutdown();
}
