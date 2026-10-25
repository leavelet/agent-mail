//! Turns that end without any hook: a person interrupting a Claude Code
//! turn (Escape) fires none. The node finds such an end in the session's
//! transcript, or (supervised seats) on its pane, and must not take a long
//! tool call or a long answer for one. Also: `amail wake` on a busy seat,
//! and `amail sleep` holding a seat with waiting mail.
//! Every test runs on SQLite and (with `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use amail_node::wire::{PsRow, Request};
use amail_store::testutil::Backend;
use common::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

macro_rules! interrupt_tests {
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

interrupt_tests!(
    claude_interrupt_found_in_the_transcript,
    claude_interrupt_found_on_the_pane,
    sleep_holds_until_new_mail,
);

fn quick(c: &mut amail_node::Config) {
    c.turn_quiet = Duration::from_millis(300);
    c.screen_check_after = Duration::from_millis(1500);
}

fn now_iso(offset_ms: i64) -> String {
    let t = jiff::Timestamp::now()
        .checked_add(jiff::SignedDuration::from_millis(offset_ms))
        .unwrap();
    t.to_string()
}

fn append(path: &Path, v: Value) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{v}").unwrap();
}

/// The delivery of message `id` to `seat`.
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

/// Claude Code records `[Request interrupted by user]` and fires no hook:
/// the seat becomes idle, the mail of that turn is not delivered again,
/// and later mail is. A prompt, model output and a running tool call at the
/// end of the transcript, and an interrupt of an earlier turn, keep the
/// seat busy.
async fn claude_interrupt_found_in_the_transcript(b: Backend) {
    let Some(h) = Harness::start(
        b,
        "claude_interrupt_found_in_the_transcript",
        Opts {
            wake_window: Duration::from_millis(50),
            wake_confirm: Duration::from_secs(5),
            tweak: Some(quick),
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
    let dir = h.dir.path().join("claude/projects/-w");
    std::fs::create_dir_all(&dir).unwrap();
    let tr: PathBuf = dir.join("s1.jsonl");
    std::fs::write(&tr, "").unwrap();
    let claude = |name: &str| {
        with(
            fixture("claude", name),
            &[("transcript_path", json!(tr.to_string_lossy()))],
        )
    };
    h.hook("claude", env.clone(), claude("SessionStart")).await;
    let m1 = h.api.send(&["w1"], &[], "write a story", false).await;
    h.wait("wake-up", || async { inbox.count() == 1 }).await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m1]);
    assert_eq!(h.state("w1").await, "busy");

    // A long answer: only the prompt so far; then model text; then a long
    // tool call. No hooks all along: still busy.
    append(
        &tr,
        json!({"type": "user", "message": {"role": "user", "content": "[amail] You have new mail (wake #1)."},
               "timestamp": now_iso(0)}),
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(h.state("w1").await, "busy");
    append(
        &tr,
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "text", "text": "Once upon a time"},
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 600"}}]},
            "timestamp": now_iso(0)}),
    );
    append(
        &tr,
        json!({"type": "system", "subtype": "x", "timestamp": now_iso(0)}),
    );
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(h.state("w1").await, "busy");

    // Escape during the tool call.
    append(
        &tr,
        json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "The user doesn't want to proceed with this tool use.", "is_error": true}]},
            "timestamp": now_iso(0)}),
    );
    append(
        &tr,
        json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "text", "text": "[Request interrupted by user for tool use]"}]},
            "timestamp": now_iso(0), "someNewField": true}),
    );
    let t0 = Instant::now();
    h.wait("idle after the interrupt", || async {
        h.state("w1").await == "idle"
    })
    .await;
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    // Not a failure; its mail counts as delivered.
    assert_eq!(h.seat_status("w1").await["turn_failed"], Value::Null);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let d = delivery(&h, m1, "w1").await;
    assert!(!d["read_at"].is_null(), "{d}");
    assert_eq!(inbox.count(), 1, "nothing to deliver again");

    // Later mail is delivered.
    let m2 = h.api.send(&["w1"], &[], "and now this", false).await;
    h.wait("second wake-up", || async { inbox.count() == 2 })
        .await;
    let v = h
        .hook("claude", env.clone(), claude("UserPromptSubmit"))
        .await;
    assert_eq!(injected_ids(&hook_text(&v).unwrap()), vec![m2]);
    // The transcript still ends with the earlier interrupt (the new prompt
    // is not written yet, and the record is older than the turn): busy.
    let old = json!({"type": "user", "message": {"role": "user", "content": [
        {"type": "text", "text": "[Request interrupted by user]"}]}, "timestamp": now_iso(-10_000)});
    append(&tr, old);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(h.state("w1").await, "busy");
    h.hook("claude", env.clone(), claude("Stop")).await;
    assert_eq!(h.state("w1").await, "idle");
}

// --- supervised seats ---------------------------------------------------------

async fn cli(h: &Harness, args: &[&str]) -> (bool, String) {
    let mut c = Command::new(BIN);
    c.args(args);
    for k in [
        "AMAIL_SEAT",
        "AMAIL_PROFILE",
        "CODEX_THREAD_ID",
        "AMAIL_CODEX_PASS",
        "ZELLIJ",
        "ZELLIJ_PANE_ID",
        "ZELLIJ_SESSION_NAME",
    ] {
        c.env_remove(k);
    }
    c.env("AMAIL_URL", &h.api.base)
        .env("AMAIL_TOKEN", &h.api.token)
        .env("AMAIL_NODE_SOCK", &h.sock)
        .current_dir(h.dir.path());
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

async fn up(h: &Harness, team: &str) {
    let text = team.replace("@DIR@", &h.dir.path().to_string_lossy());
    let file = h.dir.path().join("amail.toml");
    std::fs::write(&file, text).unwrap();
    let f = file.to_string_lossy().into_owned();
    let (ok, out) = cli(h, &["up", "-f", &f]).await;
    assert!(ok, "amail up failed:\n{out}");
}

async fn row(h: &Harness, seat: &str) -> PsRow {
    let v = h.node.as_ref().unwrap().node.handle(Request::Ps).await;
    let rows: Vec<PsRow> = serde_json::from_value(v["seats"].clone()).unwrap();
    rows.into_iter().find(|r| r.seat == seat).unwrap()
}

/// Wait for `state`; for `idle` of a Claude seat, also for the fake
/// session's first turn to be over (the session is idle between its
/// SessionStart and that turn, as in `tests/team.rs`).
async fn wait_state(h: &Harness, seat: &str, state: &str) {
    let t0 = Instant::now();
    loop {
        let r = row(h, seat).await;
        let first_turn_done = state != "idle"
            || r.harness != "claude"
            || std::fs::read_to_string(h.fake.join(format!("{seat}.ready")))
                .ok()
                .and_then(|p| p.trim().parse::<u32>().ok())
                == r.pid;
        if r.state == state && first_turn_done {
            return;
        }
        if t0.elapsed() > Duration::from_secs(20) {
            panic!(
                "timed out waiting for {seat} to be {state}: {:?}",
                row(h, seat).await
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Serve a Claude seat's inbox socket: a wake-up makes the fake harness run
/// a turn.
fn inbox(h: &Harness, seat: &str) -> FakeInbox {
    let wake = h.fake.join(format!("{seat}.wake"));
    FakeInbox::start(
        h.fake.join("sock").join(format!("{seat}.sock")),
        Some(Arc::new(move |_line: String| {
            std::fs::write(&wake, "").unwrap();
        })),
    )
}

fn injected(h: &Harness, seat: &str) -> String {
    std::fs::read_to_string(h.fake.join(format!("{seat}.injected"))).unwrap_or_default()
}

fn launches(h: &Harness, seat: &str) -> usize {
    std::fs::read_to_string(h.fake.join(format!("{seat}.launches")))
        .unwrap_or_default()
        .lines()
        .count()
}

const ONE_CLAUDE: &str = r#"
[team]
workroot = "@DIR@/work"
[defaults]
model = "m"
[[seat]]
name = "c"
harness = "claude"
"#;

/// Without a transcript, a supervised seat's pane decides: a turn that
/// sends no hooks while its screen shows the working line stays busy
/// (`amail wake` says so); once the screen shows no running turn, twice,
/// the seat is idle and later mail is delivered.
async fn claude_interrupt_found_on_the_pane(b: Backend) {
    let Some(h) = Harness::start(
        b,
        "claude_interrupt_found_on_the_pane",
        Opts {
            supervise: true,
            tweak: Some(quick),
            ..Default::default()
        },
    )
    .await
    else {
        return;
    };
    let _i = inbox(&h, "c");
    up(&h, ONE_CLAUDE).await;
    wait_state(&h, "c", "idle").await;
    std::fs::write(h.fake.join("c.hang"), "").unwrap();
    let m1 = h.api.send(&["c"], &[], "a long job", false).await;
    let needle = format!("#{m1} from");
    h.wait("shown", || async { injected(&h, "c").contains(&needle) })
        .await;
    wait_state(&h, "c", "busy").await;
    // Well past the time the pane is looked at: still busy.
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(row(&h, "c").await.state, "busy");
    let (ok, out) = cli(&h, &["wake", "c"]).await;
    assert!(
        ok && out.contains("c: busy: its turn is running")
            && out.contains("its pane shows the turn running"),
        "{out}"
    );
    assert_eq!(row(&h, "c").await.state, "busy");

    // Escape: no hook; the screen shows the interrupt.
    std::fs::write(h.fake.join("c.esc"), "").unwrap();
    wait_state(&h, "c", "idle").await;
    let st = h.status().await;
    let s = st["seats"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["seat"] == "c")
        .unwrap()
        .clone();
    assert_eq!(s["turn_failed"], Value::Null, "{s}");
    let m2 = h.api.send(&["c"], &[], "next", false).await;
    let needle = format!("#{m2} from");
    h.wait("later mail shown", || async {
        injected(&h, "c").contains(&needle)
    })
    .await;
    // Shown once each.
    assert_eq!(injected(&h, "c").matches(&format!("#{m1} from")).count(), 1);
    cli(&h, &["down"]).await;
}

/// `amail sleep` on a seat with waiting mail keeps it asleep; new mail, or
/// `amail wake`, starts it again.
async fn sleep_holds_until_new_mail(b: Backend) {
    let Some(h) = Harness::start(
        b,
        "sleep_holds_until_new_mail",
        Opts {
            supervise: true,
            ..Default::default()
        },
    )
    .await
    else {
        return;
    };
    let _i = inbox(&h, "c");
    up(&h, ONE_CLAUDE).await;
    wait_state(&h, "c", "idle").await;
    // Mail arrives while a turn runs, so it waits.
    std::fs::write(h.fake.join("c.busy"), "").unwrap();
    std::fs::write(h.fake.join("c.prompt"), "").unwrap();
    wait_state(&h, "c", "busy").await;
    let (ok, out) = cli(&h, &["sleep", "c"]).await;
    assert!(ok, "{out}");
    std::fs::remove_file(h.fake.join("c.busy")).unwrap();
    wait_state(&h, "c", "dormant").await;
    let m1 = h.api.send(&["c"], &[], "waiting mail", false).await;
    // That counts as new mail: the seat starts.
    let needle = format!("#{m1} from");
    h.wait("new mail starts it", || async {
        injected(&h, "c").contains(&needle)
    })
    .await;
    assert_eq!(launches(&h, "c"), 2);

    // Now sleep with mail waiting (sent while it is busy).
    wait_state(&h, "c", "idle").await;
    std::fs::write(h.fake.join("c.hang"), "").unwrap();
    std::fs::write(h.fake.join("c.prompt"), "").unwrap();
    wait_state(&h, "c", "busy").await;
    let m2 = h.api.send(&["c"], &[], "held mail", false).await;
    h.wait_pending("c", 1, 0, 0).await;
    let (ok, out) = cli(&h, &["sleep", "c"]).await;
    assert!(
        ok && out.contains("c is asleep") && out.contains("stays asleep with its 1 waiting"),
        "{out}"
    );
    std::fs::remove_file(h.fake.join("c.hang")).ok();
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(row(&h, "c").await.state, "dormant");
    assert_eq!(launches(&h, "c"), 2);
    // A node restart keeps the hold.
    let mut h = h;
    h.restart_node().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(row(&h, "c").await.state, "dormant");
    assert_eq!(launches(&h, "c"), 2);
    // `amail wake` ends it; the waiting mail is shown.
    let (ok, out) = cli(&h, &["wake", "c"]).await;
    assert!(ok && out.contains("c: starting"), "{out}");
    let needle = format!("#{m2} from");
    h.wait("held mail shown", || async {
        injected(&h, "c").contains(&needle)
    })
    .await;
    assert_eq!(launches(&h, "c"), 3);
    cli(&h, &["down"]).await;
}
