//! Documents and seat handover on the node's side (M9): the document
//! index at session start and when it changed, the worklog entries the
//! node writes from hooks, the `Stop` that asks for the handover note,
//! compaction drafts, and the note shown at session start. Fixture hook
//! inputs drive the real `amail hook` binary against an in-process node and
//! server. Every test runs on SQLite and (with `AMAIL_TEST_PG_URL`)
//! Postgres.

mod common;

use amail_store::testutil::Backend;
use common::*;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;

macro_rules! handover_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    crate::common::deadline(stringify!($name), super::$name(amail_store::testutil::Backend::Sqlite)).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    crate::common::deadline(stringify!($name), super::$name(amail_store::testutil::Backend::Postgres)).await;
                }
            )*
        }
    };
}

handover_tests!(
    index_at_start_and_when_changed,
    worklog_of_a_session,
    stop_asks_for_the_note_over_the_threshold,
    stop_asks_for_the_note_after_a_closed_task,
    stop_asks_for_a_requested_note,
    post_compact_files_a_draft,
    note_shown_at_session_start,
    idle_seat_with_work_is_asked,
    codex_index_and_compaction,
    slow_index_and_note_leave_mail_on_time,
    ps_knows_the_note_from_the_start,
);

const ASK: &str = "Write your handover note now with `amail handover write`; then stop.";

fn tweak(c: &mut amail_node::Config) {
    c.index_recheck = Duration::ZERO;
    c.worklog_every = Duration::from_millis(100);
    c.handover_scan = Duration::from_millis(200);
}

async fn setup(b: Backend, test: &str) -> Option<Harness> {
    Harness::start(
        b,
        test,
        Opts {
            tweak: Some(tweak),
            ..Default::default()
        },
    )
    .await
}

fn claude(name: &str) -> Value {
    fixture("claude", name)
}

/// A document of kind `index` (in the per-turn index, no required
/// sections), made active.
async fn active_doc(h: &Harness, title: &str, description: &str) -> i64 {
    let d = h
        .api
        .post(
            "/docs",
            json!({"title": title, "kind": "index", "description": description,
                   "body": format!("# {title}\n\nSee d1.\n")}),
        )
        .await;
    let num = d["doc"]["num"].as_i64().unwrap();
    h.api
        .call(
            reqwest::Method::PATCH,
            &format!("/docs/{num}"),
            Some(json!({"status": "active"})),
        )
        .await;
    num
}

/// The seat's note as the server has it.
async fn note(h: &Harness, seat: &str) -> Value {
    h.api.get(&format!("/seats/{seat}/handover")).await
}

/// A note with every field, written by the seat itself.
async fn write_note(seat: &Api, name: &str, traps: &str) -> Value {
    seat.call(
        reqwest::Method::PUT,
        &format!("/seats/{name}/handover"),
        Some(json!({"fields": {
            "state": "on track",
            "summary": "Fixing the login form.",
            "next": "1. cargo test -p web",
            "waiting_on": "nothing",
            "traps": traps,
            "verify": "cargo test passes",
            "pointers": "t1"
        }})),
    )
    .await
}

/// A Claude Code transcript under the node's projects directory whose
/// last model request had `input` input tokens.
fn transcript(h: &Harness, input: u64) -> PathBuf {
    let dir = h.cfg.claude_config_dir.join("projects").join("p");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("session.jsonl");
    let line = json!({"type": "assistant", "message": {"model": "claude-sonnet-4",
        "usage": {"input_tokens": 100, "cache_read_input_tokens": input - 100,
                  "output_tokens": 10}}});
    std::fs::write(&path, format!("{line}\n")).unwrap();
    path
}

/// Wait until the node's cached index of `seat` has `n` lines (the node
/// refreshes it in the background).
async fn wait_index(h: &Harness, seat: &str, n: usize) {
    h.wait(&format!("{seat}'s index with {n} lines"), || async {
        h.seat_status(seat).await["index_lines"] == n
    })
    .await;
}

async fn worklog(h: &Harness, seat: &str) -> Vec<Value> {
    h.api.get(&format!("/seats/{seat}/worklog?limit=200")).await["entries"]
        .as_array()
        .unwrap()
        .clone()
}

/// The node's worklog entries of `seat` with `kind`, oldest first.
async fn node_entries(h: &Harness, seat: &str, kind: &str) -> Vec<Value> {
    let mut v: Vec<Value> = worklog(h, seat)
        .await
        .into_iter()
        .filter(|e| e["source"] == "node" && e["kind"] == kind)
        .collect();
    v.reverse();
    v
}

async fn index_at_start_and_when_changed(b: Backend) {
    let Some(h) = setup(b, "index_at_start_and_when_changed").await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    // No documents: nothing.
    assert_eq!(
        h.hook("claude", env.clone(), claude("SessionStart")).await,
        Value::Null
    );
    let d1 = active_doc(&h, "Deploy", "how we deploy").await;
    // A draft is not in the index.
    h.api
        .post(
            "/docs",
            json!({"title": "Draft", "kind": "index", "body": "# Draft\n\nx\n"}),
        )
        .await;
    // Hooks show the index the node has cached; a hook starts a refresh
    // in the background, and a later hook shows what it brought (this one
    // when the refresh was quicker than the hook).
    wait_index(&h, "w1", 0).await;
    let want = format!("Documents:\nd{d1}  index  Deploy — how we deploy");
    let v = h.hook("claude", env.clone(), claude("SessionStart")).await;
    if v == Value::Null {
        wait_index(&h, "w1", 1).await;
        let v = h
            .hook("claude", env.clone(), claude("UserPromptSubmit"))
            .await;
        assert_eq!(hook_text(&v).unwrap(), want);
    } else {
        assert_eq!(hook_text(&v).unwrap(), want);
    }
    // A turn shows it only when it changed.
    assert_eq!(
        h.hook("claude", env.clone(), claude("UserPromptSubmit"))
            .await,
        Value::Null
    );
    let d2 = active_doc(&h, "Testing", "how we test").await;
    // A tool call refreshes the cache without showing it.
    assert_eq!(
        h.hook("claude", env.clone(), claude("PostToolUse")).await,
        Value::Null
    );
    wait_index(&h, "w1", 2).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let t = hook_text(&v).unwrap();
    assert!(t.starts_with("Documents:\n"), "{t}");
    assert!(
        t.contains(&format!("d{d1}  index")) && t.contains(&format!("d{d2}  index")),
        "{t}"
    );
    assert_eq!(
        h.hook("claude", env.clone(), claude("UserPromptSubmit"))
            .await,
        Value::Null
    );
    // A Stop with nothing to show does not block for the index.
    active_doc(&h, "Third", "another").await;
    h.hook("claude", env.clone(), claude("PostToolUse")).await;
    wait_index(&h, "w1", 3).await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    assert_eq!(h.state("w1").await, "idle");
    // The index changed since the seat saw it: the next turn shows it
    // after the mail (here the task's assignment).
    h.api
        .post("/tasks", json!({"title": "work", "assignee": "w1"}))
        .await;
    h.wait_pending("w1", 1, 0, 0).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let t = hook_text(&v).unwrap();
    assert_eq!(injected_ids(&t).len(), 1, "{t}");
    let (mail, docs) = t.split_once("\n\nDocuments:\n").unwrap();
    assert!(mail.starts_with("[amail] 1 new message"), "{t}");
    assert!(docs.contains("Third"), "{t}");
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
    let t = hook_text(&v).unwrap();
    assert_eq!(injected_ids(&t), vec![long]);
    assert!(!t.contains("Documents:"), "{t}");
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("SessionStart"), &[("source", json!("resume"))]),
        )
        .await;
    let t = hook_text(&v).unwrap();
    let (mail, task, docs) = (
        t.find("[amail] w1 has 1 unread").unwrap(),
        t.find("open task(s)").unwrap(),
        t.find("Documents:").unwrap(),
    );
    assert!(mail < task && task < docs, "{t}");
    // After a compaction the next turn shows the whole index again.
    h.hook("claude", env.clone(), claude("PostCompact")).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert!(hook_text(&v).unwrap().contains("Documents:"));
}

async fn worklog_of_a_session(b: Backend) {
    let Some(h) = setup(b, "worklog_of_a_session").await else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    // A git working tree with one commit.
    let repo = h.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "first"]);
    let cwd = json!(repo.to_string_lossy());
    let sid = claude("SessionStart")["session_id"].clone();
    // A task w1 is doing marks the entries.
    let t1 = h
        .api
        .post("/tasks", json!({"title": "login form", "assignee": "w1"}))
        .await["task"]["num"]
        .as_i64()
        .unwrap();
    h.api
        .call(
            reqwest::Method::PATCH,
            &format!("/tasks/{t1}"),
            Some(json!({"state": "doing"})),
        )
        .await;
    // The assignment's mail is shown by the first turn.
    h.wait_pending("w1", 1, 0, 0).await;
    let at = |name: &str| with(claude(name), &[("cwd", cwd.clone())]);
    h.hook("claude", env.clone(), at("SessionStart")).await;
    h.hook("claude", env.clone(), at("UserPromptSubmit")).await;
    h.hook("claude", env.clone(), at("PostToolUse")).await;
    // The baseline commit was looked at after session start; a new one is
    // logged at the turn's end.
    tokio::time::sleep(Duration::from_millis(500)).await;
    git(&["commit", "-q", "--allow-empty", "-m", "fix the login form"]);
    h.hook("claude", env.clone(), at("Stop")).await;
    h.hook("claude", env.clone(), at("PreCompact")).await;
    h.hook("claude", env.clone(), at("PostCompact")).await;
    h.hook("claude", env.clone(), at("SessionEnd")).await;
    h.wait("node worklog entries", || async {
        node_entries(&h, "w1", "session_end").await.len() == 1
            && node_entries(&h, "w1", "commit").await.len() == 1
    })
    .await;
    let one = |kind: &'static str| {
        let h = &h;
        async move {
            let v = node_entries(h, "w1", kind).await;
            assert_eq!(v.len(), 1, "{kind}: {v:?}");
            v[0].clone()
        }
    };
    assert_eq!(one("session_start").await["text"], "source: startup");
    assert_eq!(one("turn_start").await["text"], "turn started");
    let tool = one("tool").await;
    assert_eq!(tool["text"], "Bash: echo hi");
    assert_eq!(tool["task"], t1);
    let commit = one("commit").await;
    assert!(
        commit["text"]
            .as_str()
            .unwrap()
            .ends_with(" fix the login form"),
        "{commit}"
    );
    assert_eq!(one("turn_end").await["text"], "turn ended");
    assert_eq!(one("precompact").await["text"], "trigger: auto");
    assert!(
        one("compact").await["text"]
            .as_str()
            .unwrap()
            .starts_with("Summary: the user asked for a fix")
    );
    assert_eq!(
        one("session_end").await["text"],
        "reason: prompt_input_exit"
    );
    for e in worklog(&h, "w1").await {
        if e["source"] == "node" {
            assert_eq!(e["session"], sid, "{e}");
        }
    }
    // A failed turn's end names the failure.
    h.hook("claude", env.clone(), at("SessionStart")).await;
    h.hook("claude", env.clone(), at("UserPromptSubmit")).await;
    h.hook("claude", env.clone(), at("StopFailure")).await;
    h.wait("failed turn entry", || async {
        node_entries(&h, "w1", "turn_end")
            .await
            .iter()
            .any(|e| e["text"].as_str().unwrap().starts_with("turn failed: "))
    })
    .await;
}

async fn stop_asks_for_the_note_over_the_threshold(b: Backend) {
    let Some(h) = setup(b, "stop_asks_for_the_note_over_the_threshold").await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    // 150,000 of 200,000 tokens: over 70%.
    let path = transcript(&h, 150_000);
    let tp = json!(path.to_string_lossy());
    let at = |name: &str| with(claude(name), &[("transcript_path", tp.clone())]);
    h.hook("claude", env.clone(), at("SessionStart")).await;
    h.hook("claude", env.clone(), at("UserPromptSubmit")).await;
    let v = h.hook("claude", env.clone(), at("Stop")).await;
    assert_eq!(v["decision"], "block", "{v}");
    assert_eq!(hook_text(&v).unwrap(), ASK);
    assert_eq!(h.state("w1").await, "busy");
    // Once per turn: the next Stop ends it although no note was written.
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(at("Stop"), &[("stop_hook_active", json!(true))]),
        )
        .await;
    assert_eq!(v, Value::Null);
    assert_eq!(h.state("w1").await, "idle");
    // A note written in the turn: no request.
    h.hook("claude", env.clone(), at("UserPromptSubmit")).await;
    write_note(&w1, "w1", "none").await;
    assert_eq!(h.hook("claude", env.clone(), at("Stop")).await, Value::Null);
    assert_eq!(h.seat_status("w1").await["handover_note"], "current");
    // Under the threshold: no request.
    transcript(&h, 100_000);
    h.hook("claude", env.clone(), at("UserPromptSubmit")).await;
    assert_eq!(h.hook("claude", env.clone(), at("Stop")).await, Value::Null);
    assert_eq!(h.seat_status("w1").await["handover_note"], "stale");
}

async fn stop_asks_for_the_note_after_a_closed_task(b: Backend) {
    let Some(h) = setup(b, "stop_asks_for_the_note_after_a_closed_task").await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    let task = |title: &str| {
        let api = h.api.clone();
        let title = title.to_string();
        async move {
            api.post("/tasks", json!({"title": title, "assignee": "w1"}))
                .await["task"]["num"]
                .as_i64()
                .unwrap()
        }
    };
    let (t1, t2) = (task("first").await, task("second").await);
    h.wait_pending("w1", 2, 0, 0).await;
    // The seat closes a task with `amail task done` at the end of a long
    // shell command, which its PostToolUse hook reports: the node keeps
    // 200 characters of it for the worklog but finds the task command in
    // the whole input.
    let close = |n: i64| {
        let (h, w1, env) = (&h, w1.clone(), env.clone());
        async move {
            w1.call(
                reqwest::Method::PATCH,
                &format!("/tasks/{n}"),
                Some(json!({"state": "done"})),
            )
            .await;
            let tool = with(
                claude("PostToolUse"),
                &[(
                    "tool_input",
                    json!({"command": format!(
                        "cd /work/app && git commit -am '{}' && amail send lead -s merge -m ok; amail task done t{n} --result ok",
                        "fix the login form ".repeat(15)
                    )}),
                )],
            );
            h.hook("claude", env, tool).await;
        }
    };
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    // A turn that closes nothing ends at once.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    // A task closed without a tool call the node sees (here by the
    // user) is not the seat's: no request.
    let t3 = task("third").await;
    h.wait_pending("w1", 1, 0, 0).await;
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    h.api
        .call(
            reqwest::Method::PATCH,
            &format!("/tasks/{t3}"),
            Some(json!({"state": "done"})),
        )
        .await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    // The seat closes t1 and still holds t2: asked once.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    close(t1).await;
    let v = h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(hook_text(&v).unwrap(), ASK);
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    // Closing the last task: the server writes the idle note itself, so
    // the seat is not asked.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    close(t2).await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    let n = note(&h, "w1").await;
    assert_eq!(n["note"]["source"], "server", "{n}");
}

async fn stop_asks_for_a_requested_note(b: Backend) {
    let Some(h) = setup(b, "stop_asks_for_a_requested_note").await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    h.api.post("/seats/w1/handover/request", json!({})).await;
    h.wait_pending("w1", 1, 0, 0).await;
    // The request mail is shown first; then the note is asked for.
    let v = h.hook("claude", env.clone(), claude("Stop")).await;
    let t = hook_text(&v).unwrap();
    assert!(t.contains("Handover note requested by owner"), "{t}");
    let v = h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(hook_text(&v).unwrap(), ASK);
    // Written: the request is answered.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    write_note(&w1, "w1", "none").await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
}

async fn post_compact_files_a_draft(b: Backend) {
    let Some(h) = setup(b, "post_compact_files_a_draft").await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("PreCompact")).await,
        Value::Null
    );
    assert_eq!(
        h.hook("claude", env.clone(), claude("PostCompact")).await,
        Value::Null
    );
    h.wait("compaction draft", || async {
        note(&h, "w1").await["note"]["source"] == "compaction"
    })
    .await;
    let n = note(&h, "w1").await["note"].clone();
    assert_eq!(n["draft"], true);
    assert_eq!(n["fields"]["state"], "draft: from compaction summary");
    assert!(
        n["fields"]["summary"]
            .as_str()
            .unwrap()
            .contains("login form")
    );
    assert_eq!(n["session"], claude("PostCompact")["session_id"]);
    // A note written in the current turn is kept.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let rev = write_note(&w1, "w1", "none").await["current_rev"].clone();
    h.hook("claude", env.clone(), claude("PostCompact")).await;
    h.wait("compact entry", || async {
        node_entries(&h, "w1", "compact").await.len() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(note(&h, "w1").await["current_rev"], rev);
    // Without a summary there is no draft.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let mut bare = claude("PostCompact");
    bare.as_object_mut().unwrap().remove("compact_summary");
    h.hook("claude", env.clone(), bare).await;
    h.wait("compact entry", || async {
        node_entries(&h, "w1", "compact").await.len() == 3
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(note(&h, "w1").await["current_rev"], rev);
}

async fn note_shown_at_session_start(b: Backend) {
    let Some(h) = setup(b, "note_shown_at_session_start").await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    // A long Traps field: about 12,000 characters.
    let traps: Vec<String> = (1..=300)
        .map(|i| format!("- trap {i:03}: this approach fails"))
        .collect();
    write_note(&w1, "w1", &traps.join("\n")).await;
    active_doc(&h, "Deploy", "how we deploy").await;
    let sid = claude("SessionStart")["session_id"].clone();
    let v = h.hook("claude", env.clone(), claude("SessionStart")).await;
    let t = hook_text(&v).unwrap();
    assert!(
        t.starts_with("Handover note of this seat, written by the previous session at "),
        "{t}"
    );
    // Whole fields, then as much of Traps as fits; the index after it.
    for f in [
        "State: on track",
        "Summary: Fixing the login form.",
        "Next: 1. cargo test -p web",
        "Waiting on: nothing",
        "Verify: cargo test passes",
        "- trap 001:",
    ] {
        assert!(t.contains(f), "{f}: {t}");
    }
    assert!(!t.contains("trap 300"), "{t}");
    let (note_part, docs) = t.split_once("\n\nDocuments:\n").unwrap();
    assert!(
        note_part.ends_with("… the rest: amail handover show"),
        "{note_part}"
    );
    // Claude Code: the note takes at most two thirds of 9,000 characters.
    assert!(note_part.chars().count() <= 6_000);
    assert!(docs.contains("Deploy"));
    assert!(t.len() <= 9_000);
    // The read is recorded with the session.
    h.wait("read recorded", || async {
        note(&h, "w1").await["note"]["read_by_session"] == sid
    })
    .await;
    // A forked session is not shown the note.
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("SessionStart"), &[("source", json!("fork"))]),
        )
        .await;
    assert!(!hook_text(&v).unwrap().contains("Handover note"));
    // A short note is shown whole, after `compact` too.
    write_note(&w1, "w1", "do not touch the schema").await;
    let v = h
        .hook(
            "claude",
            env.clone(),
            with(claude("SessionStart"), &[("source", json!("compact"))]),
        )
        .await;
    let t = hook_text(&v).unwrap();
    assert!(t.contains("Traps: do not touch the schema\n"), "{t}");
    assert!(t.contains("Pointers: t1"), "{t}");
    assert!(!t.contains("the rest"), "{t}");
}

/// The node asks a seat that has been idle with work in progress (and an
/// old note) by a request mail, once per idle period.
async fn idle_seat_with_work_is_asked(b: Backend) {
    let Some(h) = Harness::start(
        b,
        "idle_seat_with_work_is_asked",
        Opts {
            tweak: Some(|c| {
                tweak(c);
                // Every idle seat is due at once.
                c.handover_scan = Duration::from_millis(200);
            }),
            ..Default::default()
        },
    )
    .await
    else {
        return;
    };
    h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    // The team setting: asked after 1 s idle.
    set_handover_idle(&h, 1).await;
    h.api
        .post("/tasks", json!({"title": "work", "assignee": "w1"}))
        .await;
    // The assignment's mail is shown by the turn.
    h.wait_pending("w1", 1, 0, 0).await;
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(
        h.hook("claude", env.clone(), claude("Stop")).await,
        Value::Null
    );
    h.wait("request", || async {
        note(&h, "w1").await["requested_at"].is_string()
    })
    .await;
    // In the name of the org's system seat; the request mail wakes the
    // seat.
    assert_eq!(note(&h, "w1").await["requested_by"], "amail");
    h.wait("wake-up", || async { inbox.count() == 1 }).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    let t = hook_text(&v).unwrap();
    assert!(t.contains("Handover note requested by the node"), "{t}");
    // Once per idle period: the seat ends this turn without a note and is
    // not asked again (the request stays open).
    h.hook("claude", env.clone(), claude("Stop")).await;
    h.hook("claude", env.clone(), claude("Stop")).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(inbox.count(), 1);
}

/// Give the node a team setting, as `amail up` does.
async fn set_handover_idle(h: &Harness, secs: u64) {
    // The node takes settings with `up`; with no seats to place, only the
    // settings change.
    let settings = amail_node::team::Settings {
        handover_idle_ms: Some(secs * 1000),
        ..Default::default()
    };
    let v = socket_request(
        &h.sock,
        amail_node::wire::Request::Up {
            seats: Vec::new(),
            settings,
            start: false,
            only: None,
        },
    )
    .await;
    assert!(v.get("error").is_none(), "{v}");
}

/// A Codex seat: the index within Codex's share, and the compaction hooks.
async fn codex_index_and_compaction(b: Backend) {
    let Some(h) = setup(b, "codex_index_and_compaction").await else {
        return;
    };
    h.seat("cx", "codex").await;
    let long: Vec<String> = (0..40)
        .map(|i| format!("topic {i:02} {}", "y".repeat(30)))
        .collect();
    for t in &long {
        active_doc(&h, t, "a description of some length for the index line").await;
    }
    let sid = "00000000-0000-7000-8000-00000000c0de";
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
            session: None,
        })
        .await;
    assert!(plan.get("error").is_none(), "{plan}");
    let args: Vec<String> = serde_json::from_value(plan["args"].clone()).unwrap();
    let prompt = args.last().unwrap().clone();
    let env: Vec<(String, String)> = vec![
        (
            "CODEX_HOME".into(),
            h.dir
                .path()
                .join("state/codex-home")
                .to_string_lossy()
                .into(),
        ),
        (
            "AMAIL_CODEX_PASS".into(),
            plan["env"]["AMAIL_CODEX_PASS"].as_str().unwrap().into(),
        ),
    ];
    let codex = |name: &str| with(fixture("codex", name), &[("session_id", json!(sid))]);
    // The first prompt registers the session and shows the index, cut to
    // Codex's share (2,000 of 6,000 characters).
    let v = h
        .hook(
            "codex",
            env.clone(),
            with(codex("UserPromptSubmit"), &[("prompt", json!(prompt))]),
        )
        .await;
    // Nothing cached yet: no index in this answer.
    assert!(!hook_text(&v).unwrap_or_default().contains("Documents:"));
    h.wait("cx's index", || async {
        h.seat_status("cx").await["index_lines"]
            .as_u64()
            .unwrap_or(0)
            > 0
    })
    .await;
    let v = h.hook("codex", env.clone(), codex("SessionStart")).await;
    let t = hook_text(&v).unwrap();
    assert!(t.starts_with("Documents:\n"), "{t}");
    assert!(t.chars().count() <= 2_000, "{}", t.chars().count());
    assert!(t.contains("more: amail doc"), "{t}");
    assert_eq!(
        h.hook("codex", env.clone(), codex("PreCompact")).await,
        Value::Null
    );
    assert_eq!(
        h.hook("codex", env.clone(), codex("PostCompact")).await,
        Value::Null
    );
    h.wait("compaction draft", || async {
        note(&h, "cx").await["note"]["source"] == "compaction"
    })
    .await;
    assert_eq!(
        note(&h, "cx").await["note"]["fields"]["summary"],
        "Summary of the conversation so far: working on t1."
    );
    h.wait("compaction entries", || async {
        node_entries(&h, "cx", "precompact").await.len() == 1
            && node_entries(&h, "cx", "compact").await.len() == 1
    })
    .await;
    assert_eq!(node_entries(&h, "cx", "compact").await[0]["session"], sid);
}

/// The document index and the handover note are slow to come: the mail a
/// hook claimed is still answered within the hook's time, and the index
/// and the note come with a later hook.
async fn slow_index_and_note_leave_mail_on_time(b: Backend) {
    fn slow(path: &str) -> bool {
        path.ends_with("/docs/index")
            || (path.starts_with("/v1/seats/") && path.ends_with("/handover"))
    }
    let Some(h) = Harness::start(
        b,
        "slow_index_and_note_leave_mail_on_time",
        Opts {
            tweak: Some(tweak),
            slow_routes: Some((slow, Duration::from_millis(1500))),
            ..Default::default()
        },
    )
    .await
    else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    write_note(&w1, "w1", "none").await;
    active_doc(&h, "Deploy", "how we deploy").await;
    let m1 = h.api.send(&["w1"], &[], "m1", false).await;
    h.wait_pending("w1", 1, 0, 0).await;
    // A hook client that waits 500 ms, as a short hook timeout does.
    let quick = |name: &str| {
        let (sock, env) = (h.sock.clone(), env.clone());
        let input = claude(name).to_string();
        async move {
            let (out, took, ok) = tokio::task::spawn_blocking(move || {
                let env: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                run_hook(&sock, "claude", &env, &input, Some(500))
            })
            .await
            .unwrap();
            assert!(ok, "hook failed");
            assert!(took < Duration::from_millis(1400), "{took:?}");
            if out.is_empty() {
                Value::Null
            } else {
                serde_json::from_str::<Value>(&out).unwrap()
            }
        }
    };
    // Session start: neither the note nor the index arrives in time.
    let v = quick("SessionStart").await;
    let t = hook_text(&v).unwrap_or_default();
    assert!(
        !t.contains("Handover note") && !t.contains("Documents:"),
        "{t}"
    );
    assert_eq!(h.seat_status("w1").await["note_pending"], true);
    // The turn's mail is answered in time.
    let v = quick("UserPromptSubmit").await;
    let t = hook_text(&v).unwrap_or_default();
    assert_eq!(injected_ids(&t), vec![m1], "{t}");
    assert!(!t.contains("Handover note"), "{t}");
    // Once they have come, the next hook shows both.
    h.wait("index and note fetched", || async {
        h.seat_status("w1").await["index_lines"] == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let v = quick("UserPromptSubmit").await;
    let t = hook_text(&v).unwrap();
    assert!(
        t.starts_with("Handover note of this seat, written by the previous session at "),
        "{t}"
    );
    assert!(t.contains("\n\nDocuments:\n"), "{t}");
    assert_eq!(h.seat_status("w1").await["note_pending"], false);
    // Shown once.
    let v = quick("UserPromptSubmit").await;
    assert!(!hook_text(&v).unwrap_or_default().contains("Handover note"));
}

/// `amail ps` shows whether the note is current without waiting for the
/// next check: after the seat wrote its note in a turn, and after a node
/// restart.
async fn ps_knows_the_note_from_the_start(b: Backend) {
    let Some(mut h) = setup(b, "ps_knows_the_note_from_the_start").await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let inbox = FakeInbox::start(h.dir.path().join("cc.sock"), None);
    let env = claude_env("w1", &inbox.path);
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    h.wait("no note", || async {
        h.seat_status("w1").await["handover_note"] == "none"
    })
    .await;
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    // The seat writes its note with a shell command in the turn.
    write_note(&w1, "w1", "none").await;
    let tool = with(
        claude("PostToolUse"),
        &[(
            "tool_input",
            json!({"command": "cd /work/app && amail handover write -m \"State: on track ...\""}),
        )],
    );
    h.hook("claude", env.clone(), tool).await;
    h.wait("note current after the write", || async {
        h.seat_status("w1").await["handover_note"] == "current"
    })
    .await;
    h.hook("claude", env.clone(), claude("Stop")).await;
    h.wait("turn start in the worklog", || async {
        !node_entries(&h, "w1", "turn_start").await.is_empty()
    })
    .await;
    // After a restart the node fetches the note's state and the last turn
    // from the server.
    h.restart_node().await;
    h.wait("note current after a restart", || async {
        h.seat_status("w1").await["handover_note"] == "current"
    })
    .await;
    // A turn after the note makes it stale.
    h.hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(h.seat_status("w1").await["handover_note"], "stale");
}
