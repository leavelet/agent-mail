//! Team bootstrap and seat lifecycle without zellij or real harnesses: the
//! node supervises "panes" that are plain processes, each running the real
//! `amail run`, which execs a fake harness (`common/fake_harness.sh`) that
//! speaks the hook protocol through the real `amail hook`. `amail up`,
//! `down`, `ps`, `sleep` and `wake` are the real binary.
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

macro_rules! team_tests {
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

team_tests!(
    up_is_idempotent_and_incremental,
    sleep_and_cold_start,
    max_live_sleeps_least_recently_active,
    start_limit_bounds_cold_starts,
    crash_restart_backoff_and_resume_fallback,
    seats_needing_a_person_show_in_ps,
    server_commands_sleep_and_wake,
    session_without_a_turn_starts_fresh,
    list_mail_wakes_and_cold_starts,
    staged_merge_worktrees_and_validation,
    paste_fallback_types_only_when_safe,
    server_permission_mode_is_informational,
);

async fn setup(
    b: Backend,
    test: &str,
    tweak: Option<fn(&mut amail_node::Config)>,
) -> Option<Harness> {
    Harness::start(
        b,
        test,
        Opts {
            supervise: true,
            tweak,
            ..Default::default()
        },
    )
    .await
}

/// Run the `amail` binary as the org's user, against the test node.
async fn cli(h: &Harness, args: &[&str]) -> (bool, String) {
    cli_sock(h, &h.sock, args).await
}

async fn cli_sock(h: &Harness, sock: &Path, args: &[&str]) -> (bool, String) {
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
        .env("AMAIL_NODE_SOCK", sock)
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

async fn up(h: &Harness, extra: &[&str]) -> String {
    let file = h.dir.path().join("amail.toml");
    let f = file.to_string_lossy().into_owned();
    let mut args = vec!["up", "-f", &f];
    args.extend_from_slice(extra);
    let (ok, out) = cli(h, &args).await;
    assert!(ok, "amail up {extra:?} failed:\n{out}");
    out
}

fn write_team(h: &Harness, text: &str) {
    let text = text.replace("@DIR@", &h.dir.path().to_string_lossy());
    std::fs::write(h.dir.path().join("amail.toml"), text).unwrap();
}

async fn ps(h: &Harness) -> Vec<PsRow> {
    let v = h.node.as_ref().unwrap().node.handle(Request::Ps).await;
    serde_json::from_value(v["seats"].clone()).unwrap()
}

async fn row(h: &Harness, seat: &str) -> PsRow {
    ps(h)
        .await
        .into_iter()
        .find(|r| r.seat == seat)
        .unwrap_or_else(|| panic!("no ps row for {seat}"))
}

async fn wait_state(h: &Harness, seat: &str, state: &str) {
    let t0 = Instant::now();
    loop {
        let rows = ps(h).await;
        if rows.iter().any(|r| r.seat == seat && r.state == state) {
            return;
        }
        if t0.elapsed() > Duration::from_secs(20) {
            panic!(
                "timed out waiting for {seat} to be {state}; ps: {:#?}\nfake dir: {}",
                rows,
                fake_files(h)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn fake_files(h: &Harness) -> String {
    let mut out = String::new();
    if let Ok(d) = std::fs::read_dir(&h.fake) {
        for e in d.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x != "sh") {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                if name == "claude" || name == "codex" {
                    continue;
                }
                out.push_str(&format!(
                    "--- {name}\n{}\n",
                    std::fs::read_to_string(&p).unwrap_or_default()
                ));
            }
        }
    }
    out
}

fn lines(h: &Harness, file: &str) -> Vec<String> {
    std::fs::read_to_string(h.fake.join(file))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn launches(h: &Harness, seat: &str) -> Vec<String> {
    lines(h, &format!("{seat}.launches"))
}

/// Serve a Claude seat's inbox socket: a wake-up line makes the fake
/// harness run a turn.
fn inbox(h: &Harness, seat: &str) -> FakeInbox {
    let wake = h.fake.join(format!("{seat}.wake"));
    FakeInbox::start(
        h.fake.join("sock").join(format!("{seat}.sock")),
        Some(Arc::new(move |_line: String| {
            std::fs::write(&wake, "").unwrap();
        })),
    )
}

fn set_mode(h: &Harness, seat: &str, mode: &str) {
    let p = h.fake.join(format!("{seat}.mode"));
    if mode.is_empty() {
        let _ = std::fs::remove_file(p);
    } else {
        std::fs::write(p, mode).unwrap();
    }
}

fn injected(h: &Harness, seat: &str) -> String {
    std::fs::read_to_string(h.fake.join(format!("{seat}.injected"))).unwrap_or_default()
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| {
            let rest = &s[s.rfind(')').unwrap() + 2..];
            !rest.starts_with('Z')
        })
        .unwrap_or(false)
}

fn kill9(pid: u32) {
    // SAFETY: signalling a child of this test.
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

async fn wait_for<F: FnMut() -> bool>(what: &str, mut f: F) {
    let t0 = Instant::now();
    while !f() {
        if t0.elapsed() > Duration::from_secs(20) {
            panic!("timed out waiting for {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until message `id` was shown to `seat` by a hook (claimed, not just
/// listed in an unread summary).
async fn wait_injected(h: &Harness, seat: &str, id: i64) {
    let t0 = Instant::now();
    loop {
        let st = h.api.get(&format!("/messages/{id}/status")).await;
        let via = st["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["seat"] == seat)
            .and_then(|r| r["notified_via"].as_str().map(str::to_string));
        // Claimed by a hook; the fake harness records the answer just after.
        if let Some(via) = via {
            assert!(via.starts_with("hook:"), "{st}");
            let needle = format!("#{id} from");
            wait_for("the injection to be recorded", || {
                injected(h, seat).contains(&needle)
            })
            .await;
            return;
        }
        if t0.elapsed() > Duration::from_secs(20) {
            panic!(
                "timed out waiting for #{id} to reach {seat}; ps: {:#?}\n{}",
                ps(h).await,
                fake_files(h)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn list_members(h: &Harness, name: &str) -> Vec<String> {
    let mut m: Vec<String> = h.api.get(&format!("/lists/{name}")).await["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["address"].as_str().unwrap().to_string())
        .collect();
    m.sort();
    m
}

const TEAM: &str = r#"
[team]
workroot = "@DIR@/work"

[defaults]
harness = "claude"
model = "test-model"
permission_mode = "unattended"

[[seat]]
name = "root"
role = "Runs the show."

[[group]]
name = "g"
parent = "root"
lead = { name = "g-lead", harness = "codex" }
workers = { count = 2, prefix = "gw" }
"#;

/// `up` creates seats, lists and sessions; again it changes nothing; then
/// only the differences; `down` leaves nothing running; `up` resumes.
async fn up_is_idempotent_and_incremental(b: Backend) {
    let Some(h) = setup(b, "up_is_idempotent_and_incremental", None).await else {
        return;
    };
    let _inboxes: Vec<FakeInbox> = ["root", "gw-1", "gw-2", "gw-3"]
        .iter()
        .map(|s| inbox(&h, s))
        .collect();
    write_team(&h, TEAM);
    let out = up(&h, &[]).await;
    assert!(
        out.contains("created seats: root, g-lead, gw-1, gw-2"),
        "{out}"
    );
    assert!(out.contains("starting: "), "{out}");
    for s in ["root", "g-lead", "gw-1", "gw-2"] {
        wait_state(&h, s, "idle").await;
    }
    // Seats on the server: tree, group, role, harness.
    let w = h.api.get("/seats/gw-1").await;
    assert_eq!(w["parent"], "g-lead");
    assert_eq!(w["group"], "g");
    assert_eq!(w["harness"], "claude");
    assert_eq!(w["model"], "test-model");
    assert_eq!(w["permission_mode"], "unattended");
    assert_eq!(w["role"], "You report to `g-lead`.");
    let r = h.api.get("/seats/root").await;
    assert_eq!(
        r["role"],
        "Your team: `g-lead` (they report to you).\n\nRuns the show."
    );
    assert!(r.get("group").is_none());
    assert_eq!(h.api.get("/seats/g-lead").await["harness"], "codex");
    // Generated lists.
    assert_eq!(list_members(&h, "g").await, vec!["g-lead", "gw-1", "gw-2"]);
    assert_eq!(list_members(&h, "leads").await, vec!["g-lead", "root"]);
    assert_eq!(list_members(&h, "all").await.len(), 4);
    let all = h.api.get("/lists/all").await;
    assert_eq!(all["post_policy"], "user_only");
    assert_eq!(all["managed"], true);
    // Sessions: one per group, ungrouped seats in `main`.
    let rows = ps(&h).await;
    let sess = |s: &str| {
        rows.iter()
            .find(|r| r.seat == s)
            .unwrap()
            .mux_session
            .clone()
    };
    assert_eq!(sess("root"), "amail-acme-main");
    assert_eq!(sess("gw-1"), "amail-acme-g");
    assert_eq!(sess("g-lead"), "amail-acme-g");
    // Presence reaches the server.
    h.wait("presence idle", || async {
        h.api.get("/seats/gw-2").await["presence"]["state"] == "idle"
    })
    .await;
    let (ok, text) = cli(&h, &["ps"]).await;
    assert!(ok, "{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("gw-1") && l.contains("idle") && l.contains("amail-acme-g:")),
        "{text}"
    );

    // Again: nothing changes, nothing restarts.
    let out = up(&h, &[]).await;
    assert!(!out.contains("created"), "{out}");
    assert!(!out.contains("updated"), "{out}");
    assert!(out.contains("running: "), "{out}");
    assert!(!out.contains("starting"), "{out}");
    for s in ["root", "g-lead", "gw-1", "gw-2"] {
        assert_eq!(launches(&h, s).len(), 1, "{s}: {}", fake_files(&h));
    }

    // User lists are left alone; generated lists are repaired.
    h.api
        .post("/lists", json!({"name": "mine", "post_policy": "anyone"}))
        .await;
    h.api
        .call(reqwest::Method::DELETE, "/lists/g/members/gw-2", None)
        .await;
    let out = up(&h, &[]).await;
    assert!(out.contains("list g: +1 -0"), "{out}");
    assert_eq!(list_members(&h, "g").await, vec!["g-lead", "gw-1", "gw-2"]);
    assert_eq!(h.api.get("/lists/mine").await["managed"], Value::Null);

    // Changes: a new worker starts; changed seats keep running.
    write_team(
        &h,
        &TEAM.replace(
            "workers = { count = 2, prefix = \"gw\" }",
            "workers = { count = 3, prefix = \"gw\", role = \"Writes tests.\" }",
        ),
    );
    let out = up(&h, &[]).await;
    assert!(out.contains("created seats: gw-3"), "{out}");
    assert!(out.contains("updated seat gw-1: role"), "{out}");
    assert!(out.contains("takes effect after a restart"), "{out}");
    assert!(out.contains("gw-1, gw-2"), "{out}");
    wait_state(&h, "gw-3", "idle").await;
    assert_eq!(launches(&h, "gw-1").len(), 1);
    assert_eq!(
        h.api.get("/seats/gw-1").await["role"],
        "You report to `g-lead`.\n\nWrites tests."
    );
    assert_eq!(list_members(&h, "g").await.len(), 4);

    // Removed from the file: reported, then pruned.
    write_team(&h, TEAM);
    let out = up(&h, &[]).await;
    assert!(
        out.contains("not in the team file (kept; use --prune to delete): gw-3"),
        "{out}"
    );
    assert_eq!(h.api.get("/seats/gw-3").await["name"], "gw-3");
    let gw3 = row(&h, "gw-3").await.pid.unwrap();
    let out = up(&h, &["--prune"]).await;
    assert!(out.contains("pruned seats: gw-3"), "{out}");
    assert!(ps(&h).await.iter().all(|r| r.seat != "gw-3"));
    assert!(!alive(gw3));
    let seats = h.api.get("/seats").await;
    assert!(
        seats
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["name"] != "gw-3")
    );

    // Down: no process left, sessions closed, seats offline.
    let pids: Vec<u32> = ps(&h).await.iter().filter_map(|r| r.pid).collect();
    assert_eq!(pids.len(), 4);
    let (ok, out) = cli(&h, &["down"]).await;
    assert!(ok, "{out}");
    assert!(
        out.contains("closed zellij sessions: amail-acme-g, amail-acme-main"),
        "{out}"
    );
    for p in &pids {
        assert!(!alive(*p), "pid {p} survived down");
    }
    assert!(h.mux.as_ref().unwrap().pids().is_empty());
    assert!(h.mux.as_ref().unwrap().session_names().is_empty());
    for r in ps(&h).await {
        assert_eq!(r.state, "offline", "{r:?}");
    }
    h.wait("presence offline", || async {
        h.api.get("/seats/root").await["presence"]["state"] == "offline"
    })
    .await;

    // Up again: every seat resumes its session.
    up(&h, &[]).await;
    for s in ["root", "g-lead", "gw-1", "gw-2"] {
        wait_state(&h, s, "idle").await;
    }
    let first = launches(&h, "root")[0].clone();
    let sid = first
        .split(' ')
        .skip_while(|w| *w != "--session-id")
        .nth(1)
        .unwrap()
        .to_string();
    assert!(launches(&h, "root")[1].contains(&format!("--resume {sid}")));
    let lead = launches(&h, "g-lead");
    assert!(lead[1].contains(" resume "), "{lead:?}");
    let tid = std::fs::read_dir(&h.fake)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_prefix("thread-")?
                .strip_suffix(".seat")
                .map(str::to_string)
        })
        .next()
        .unwrap();
    assert!(lead[1].ends_with(&tid), "{lead:?} vs {tid}");
    let (ok, out) = cli(&h, &["down"]).await;
    assert!(ok, "{out}");
}

/// Idle seats go to sleep; mail to them (not cc) starts them again with
/// their session resumed, and the mail is injected; `sleep`/`wake` by hand.
async fn sleep_and_cold_start(b: Backend) {
    let Some(h) = setup(b, "sleep_and_cold_start", None).await else {
        return;
    };
    let _i = inbox(&h, "c");
    write_team(
        &h,
        r#"
[team]
workroot = "@DIR@/work"
idle_sleep = "2s"
[defaults]
model = "m"
[[seat]]
name = "c"
harness = "claude"
[[seat]]
name = "x"
harness = "codex"
"#,
    );
    up(&h, &[]).await;
    wait_state(&h, "c", "idle").await;
    wait_state(&h, "x", "idle").await;
    let pid_c = row(&h, "c").await.pid.unwrap();
    wait_state(&h, "c", "dormant").await;
    wait_state(&h, "x", "dormant").await;
    assert!(!alive(pid_c));
    let r = row(&h, "c").await;
    assert_eq!(r.pid, None);
    assert!(r.pane.is_some());
    h.wait("presence dormant", || async {
        h.api.get("/seats/c").await["presence"]["state"] == "dormant"
    })
    .await;

    // cc does not start a sleeping seat.
    h.api.send(&[], &["c"], "fyi", false).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(row(&h, "c").await.state, "dormant");
    assert_eq!(launches(&h, "c").len(), 1);

    // Mail `to` a sleeping Claude seat: resumed, then woken, then shown.
    let id = h.api.send(&["c"], &[], "hello c", false).await;
    wait_injected(&h, "c", id).await;
    let l = launches(&h, "c");
    assert_eq!(l.len(), 2);
    let sid = l[0]
        .split(' ')
        .skip_while(|w| *w != "--session-id")
        .nth(1)
        .unwrap();
    assert!(l[1].contains(&format!("--resume {sid}")), "{l:?}");
    let st = h.api.get(&format!("/messages/{id}/status")).await;
    assert_eq!(
        st["recipients"][0]["notified_via"], "hook:UserPromptSubmit",
        "{st}"
    );
    // The cc that waited comes along with the next turn (shown or listed).
    assert!(injected(&h, "c").contains("fyi"));

    // The same for Codex: resumed with its thread, then `codex queue`.
    let id = h.api.send(&["c"], &["x"], "cc to x", false).await;
    let _ = id;
    let id = h.api.send(&["x"], &[], "hello x", false).await;
    wait_injected(&h, "x", id).await;
    let l = launches(&h, "x");
    assert_eq!(l.len(), 2, "{l:?}");
    assert!(l[1].contains(" resume "), "{l:?}");
    let queued = lines(&h, "codex.log");
    assert!(
        queued.iter().any(|l| l.starts_with("queue --thread ")),
        "{queued:?}"
    );

    // Both sleep again; then by hand.
    wait_state(&h, "c", "dormant").await;
    let (ok, out) = cli(&h, &["wake", "c"]).await;
    assert!(ok && out.contains("c: starting"), "{out}");
    wait_state(&h, "c", "idle").await;
    assert_eq!(launches(&h, "c").len(), 3);
    let (ok, out) = cli(&h, &["sleep", "c"]).await;
    assert!(ok && out.contains("c is asleep"), "{out}");
    assert_eq!(row(&h, "c").await.state, "dormant");
    let (ok, out) = cli(&h, &["sleep", "nobody"]).await;
    assert!(!ok && out.contains("not supervised"), "{out}");
    cli(&h, &["down"]).await;
}

/// With more live seats than `max_live`, the least recently active idle
/// seat goes to sleep first.
async fn max_live_sleeps_least_recently_active(b: Backend) {
    let Some(h) = setup(b, "max_live_sleeps_least_recently_active", None).await else {
        return;
    };
    let _i: Vec<FakeInbox> = ["a", "b", "c"].iter().map(|s| inbox(&h, s)).collect();
    write_team(
        &h,
        r#"
[team]
workroot = "@DIR@/work"
max_live = 2
[defaults]
harness = "claude"
[[seat]]
name = "a"
[[seat]]
name = "b"
[[seat]]
name = "c"
"#,
    );
    up(&h, &["--only", "a,b"]).await;
    wait_state(&h, "a", "idle").await;
    wait_state(&h, "b", "idle").await;
    assert_eq!(row(&h, "c").await.state, "offline");
    // Activity on `a` after `b`'s.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let id = h.api.send(&["a"], &[], "work for a", false).await;
    wait_injected(&h, "a", id).await;
    wait_state(&h, "a", "idle").await;
    up(&h, &["--only", "c"]).await;
    wait_state(&h, "c", "idle").await;
    wait_state(&h, "b", "dormant").await;
    assert_eq!(row(&h, "a").await.state, "idle");
    // Mail for the sleeping one: it starts, and another idle seat sleeps.
    let id = h.api.send(&["b"], &[], "work for b", false).await;
    wait_injected(&h, "b", id).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let live = ps(&h).await.iter().filter(|r| r.pid.is_some()).count();
    assert_eq!(live, 2, "{:#?}", ps(&h).await);
    cli(&h, &["down"]).await;
}

fn set_start_limit(c: &mut amail_node::Config) {
    c.start_limit = 2;
}

/// Most seats between launch and their first hook at any moment.
fn max_overlap(h: &Harness, seats: &[&str], run: usize) -> usize {
    let mut events: Vec<(i64, i32)> = Vec::new();
    for s in seats {
        let start: i64 = launches(h, s)[run]
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let hooked: i64 = lines(h, &format!("{s}.started"))[run]
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        events.push((start, 1));
        events.push((hooked, -1));
    }
    events.sort();
    let (mut cur, mut max) = (0i32, 0i32);
    for (_, d) in events {
        cur += d;
        max = max.max(cur);
    }
    max as usize
}

/// Starts (and cold starts after a mail to everyone) run at most
/// `start_limit` at a time.
async fn start_limit_bounds_cold_starts(b: Backend) {
    let Some(h) = setup(b, "start_limit_bounds_cold_starts", Some(set_start_limit)).await else {
        return;
    };
    let seats = ["s1", "s2", "s3", "s4", "s5"];
    let _i: Vec<FakeInbox> = seats.iter().map(|s| inbox(&h, s)).collect();
    for s in seats {
        set_mode(&h, s, "slow");
    }
    let mut text =
        String::from("[team]\nworkroot = \"@DIR@/work\"\n[defaults]\nharness = \"claude\"\n");
    for s in seats {
        text.push_str(&format!("[[seat]]\nname = \"{s}\"\n"));
    }
    write_team(&h, &text);
    let t0 = Instant::now();
    up(&h, &[]).await;
    for s in seats {
        wait_state(&h, s, "idle").await;
    }
    assert!(
        t0.elapsed() >= Duration::from_millis(4000),
        "{:?}",
        t0.elapsed()
    );
    assert_eq!(max_overlap(&h, &seats, 0), 2);

    for s in seats {
        let (ok, out) = cli(&h, &["sleep", s]).await;
        assert!(ok, "{out}");
    }
    let id = h.api.send(&seats, &[], "everyone", false).await;
    for s in seats {
        wait_injected(&h, s, id).await;
    }
    assert_eq!(max_overlap(&h, &seats, 1), 2);
    cli(&h, &["down"]).await;
}

fn crash_config(c: &mut amail_node::Config) {
    c.stable_after = Duration::from_millis(1500);
    c.restart_backoff = Duration::from_millis(300);
}

/// A killed session is restarted and resumed; a failing resume falls back
/// to a new session that is told its context is gone; repeated crashes
/// back off and then give up, visibly in `ps`.
async fn crash_restart_backoff_and_resume_fallback(b: Backend) {
    let Some(h) = setup(
        b,
        "crash_restart_backoff_and_resume_fallback",
        Some(crash_config),
    )
    .await
    else {
        return;
    };
    let _i = inbox(&h, "s");
    write_team(
        &h,
        "[team]\nworkroot = \"@DIR@/work\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"s\"\n",
    );
    up(&h, &[]).await;
    wait_state(&h, "s", "idle").await;
    tokio::time::sleep(Duration::from_millis(1600)).await;

    // Killed: restarted, resuming the same session.
    let pid = row(&h, "s").await.pid.unwrap();
    kill9(pid);
    wait_for("a second launch", || launches(&h, "s").len() == 2).await;
    wait_state(&h, "s", "idle").await;
    let l = launches(&h, "s");
    let sid = l[0]
        .split(' ')
        .skip_while(|w| *w != "--session-id")
        .nth(1)
        .unwrap();
    assert!(l[1].contains(&format!("--resume {sid}")), "{l:?}");
    assert_eq!(row(&h, "s").await.restarts, 1);
    tokio::time::sleep(Duration::from_millis(1600)).await;

    // The session cannot be resumed: a new one, told so.
    set_mode(&h, "s", "badresume");
    kill9(row(&h, "s").await.pid.unwrap());
    wait_for("a fresh launch", || launches(&h, "s").len() == 4).await;
    wait_state(&h, "s", "idle").await;
    let l = launches(&h, "s");
    assert!(l[2].contains("--resume"), "{l:?}");
    assert!(l[3].contains("--session-id"), "{l:?}");
    assert!(l[3].contains("This is a new session"), "{l:?}");
    assert!(!l[3].contains("--resume"), "{l:?}");
    tokio::time::sleep(Duration::from_millis(1600)).await;

    // Crashing every time: backoff, then give up.
    set_mode(&h, "s", "exit3");
    kill9(row(&h, "s").await.pid.unwrap());
    wait_state(&h, "s", "failed").await;
    let l = launches(&h, "s");
    assert_eq!(l.len(), 7, "{l:?}");
    let t: Vec<i64> = l[4..]
        .iter()
        .map(|x| x.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert!(t[1] - t[0] >= 550, "{t:?}");
    assert!(t[2] - t[1] >= 1150, "{t:?}");
    let r = row(&h, "s").await;
    assert_eq!(r.attention.as_deref(), Some("failed"));
    let hint = r.hint.unwrap();
    assert!(hint.contains("exited with status 3"), "{hint}");
    assert!(hint.contains("amail attach s"), "{hint}");
    let (_, text) = cli(&h, &["ps"]).await;
    assert!(
        text.contains("failed") && text.contains("s: exited with status 3"),
        "{text}"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(launches(&h, "s").len(), 7, "no restart after giving up");

    // Fixed, then woken by hand.
    set_mode(&h, "s", "");
    let (ok, out) = cli(&h, &["wake", "s"]).await;
    assert!(ok, "{out}");
    wait_state(&h, "s", "idle").await;
    assert_eq!(row(&h, "s").await.attention, None);
    cli(&h, &["down"]).await;
}

fn one_start_at_a_time(c: &mut amail_node::Config) {
    c.start_limit = 1;
    c.start_timeout = Duration::from_secs(20);
}

/// Seats stuck at the directory-trust prompt: flagged in `ps` and not
/// holding start slots; the flag clears when the prompt goes away; after
/// one answer, `amail wake` and `amail up` restart the other stuck seats.
/// A seat waiting for a permission decision is flagged too.
async fn seats_needing_a_person_show_in_ps(b: Backend) {
    let Some(h) = setup(
        b,
        "seats_needing_a_person_show_in_ps",
        Some(one_start_at_a_time),
    )
    .await
    else {
        return;
    };
    // Like zellij, the multiplexer marks a pane exited a moment after its
    // process ended; restarting a stuck seat must wait for that, not count
    // it as a failed start.
    h.mux
        .as_ref()
        .unwrap()
        .set_exit_delay(Duration::from_millis(500));
    let _i: Vec<FakeInbox> = ["t1", "t2", "t3", "p"]
        .iter()
        .map(|s| inbox(&h, s))
        .collect();
    for s in ["t1", "t2", "t3"] {
        set_mode(&h, s, "trust");
    }
    write_team(
        &h,
        "[team]\nworkroot = \"@DIR@/work\"\n[defaults]\nharness = \"claude\"\n\
         [[seat]]\nname = \"t1\"\n[[seat]]\nname = \"t2\"\n[[seat]]\nname = \"t3\"\n\
         [[seat]]\nname = \"p\"\n",
    );
    // One start at a time, and the start timeout is long: `p` comes up
    // quickly only because blocked seats give their slot back.
    let t0 = Instant::now();
    up(&h, &[]).await;
    wait_state(&h, "p", "idle").await;
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    for s in ["t1", "t2", "t3"] {
        let r = row(&h, s).await;
        assert_eq!(r.attention.as_deref(), Some("trust-prompt"), "{r:?}");
        assert_eq!(r.state, "starting");
        let hint = r.hint.unwrap();
        assert!(
            hint.contains(
                &h.dir
                    .path()
                    .join("work")
                    .join(s)
                    .to_string_lossy()
                    .into_owned()
            ),
            "{hint}"
        );
        assert!(hint.contains(&format!("amail attach {s}")), "{hint}");
        assert!(hint.contains("amail wake"), "{hint}");
    }

    // Answered in t1's pane: the flag clears once the prompt is gone, then
    // the session comes up.
    std::fs::write(h.fake.join("t1.answer"), "").unwrap();
    let t0 = Instant::now();
    loop {
        let r = row(&h, "t1").await;
        if r.attention.is_none() {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{r:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_state(&h, "t1", "idle").await;
    // The others still sit at the prompt (Claude Code checks trust only at
    // start). Trust is now granted: `wake` and `up` restart them.
    set_mode(&h, "t2", "");
    set_mode(&h, "t3", "");
    let (ok, out) = cli(&h, &["wake", "t2"]).await;
    assert!(ok && out.contains("t2: restarting"), "{out}");
    let out = up(&h, &[]).await;
    assert!(
        out.contains("restarting (was stuck at a prompt): t3"),
        "{out}"
    );
    for s in ["t2", "t3"] {
        wait_state(&h, s, "idle").await;
        assert_eq!(launches(&h, s).len(), 2, "{s}");
        assert_eq!(row(&h, s).await.restarts, 0, "a restart is not a failure");
    }
    // A working seat is not restarted by wake.
    let (ok, out) = cli(&h, &["wake", "t1"]).await;
    assert!(ok && out.contains("t1: running"), "{out}");

    std::fs::write(h.fake.join("p.ask"), "").unwrap();
    wait_state(&h, "p", "awaiting").await;
    let r = row(&h, "p").await;
    assert_eq!(r.attention.as_deref(), Some("permission"));
    let (_, text) = cli(&h, &["ps"]).await;
    assert!(text.contains("permission"), "{text}");
    // The server and the user's queue know too.
    h.wait("attention reported", || async {
        h.api.get("/seats/p").await["presence"]["attention"] == "permission"
    })
    .await;
    let q = h.api.get("/attention").await;
    assert!(
        q["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "seat" && i["seat"] == "p"),
        "{q}"
    );
    cli(&h, &["down"]).await;
}

/// Sleep and wake from the web interface: the server sends the command to
/// the node, which supervises the seat.
async fn server_commands_sleep_and_wake(b: Backend) {
    let Some(h) = setup(b, "server_commands_sleep_and_wake", None).await else {
        return;
    };
    let _i = inbox(&h, "c");
    write_team(
        &h,
        "[team]\nworkroot = \"@DIR@/work\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"c\"\n",
    );
    up(&h, &[]).await;
    wait_state(&h, "c", "idle").await;
    h.wait("node connected for commands", || async {
        h.api.get("/seats/c").await["presence"]["node_connected"] == true
    })
    .await;
    let r = h.api.post("/seats/c/sleep", json!({})).await;
    assert_eq!(r["op"], "sleep", "{r}");
    wait_state(&h, "c", "dormant").await;
    h.wait("presence dormant", || async {
        h.api.get("/seats/c").await["presence"]["state"] == "dormant"
    })
    .await;
    h.api.post("/seats/c/wake", json!({})).await;
    wait_state(&h, "c", "idle").await;
    assert_eq!(launches(&h, "c").len(), 2);
    cli(&h, &["down"]).await;
}

/// The permission mode comes from the team file; the server's seat setting
/// cannot turn off a seat's permission prompts.
async fn server_permission_mode_is_informational(b: Backend) {
    let Some(h) = setup(b, "server_permission_mode_is_informational", None).await else {
        return;
    };
    let _inboxes: Vec<FakeInbox> = ["a", "b"].iter().map(|s| inbox(&h, s)).collect();
    write_team(
        &h,
        r#"
[team]
workroot = "@DIR@/work"
[defaults]
harness = "claude"
model = "test-model"
[[seat]]
name = "a"
[[seat]]
name = "b"
permission_mode = "unattended"
[[seat]]
name = "x"
harness = "codex"
[[seat]]
name = "y"
harness = "codex"
permission_mode = "unattended"
"#,
    );
    up(&h, &[]).await;
    for s in ["a", "b", "x", "y"] {
        wait_state(&h, s, "idle").await;
    }
    let skips = |s: &str| {
        launches(&h, s)
            .iter()
            .map(|l| l.contains("--dangerously-skip-permissions") || l.contains("-a never"))
            .collect::<Vec<bool>>()
    };
    assert_eq!(skips("a"), [false]);
    assert_eq!(skips("b"), [true]);
    assert_eq!(skips("x"), [false]);
    assert_eq!(skips("y"), [true]);
    // The server's settings change (an admin, or a compromised server):
    // "unattended" for a and x, "default" for b and y. The next starts
    // still follow the team file.
    for (s, m) in [
        ("a", "unattended"),
        ("x", "unattended"),
        ("b", "default"),
        ("y", "default"),
    ] {
        h.api
            .call(
                reqwest::Method::PATCH,
                &format!("/seats/{s}"),
                Some(json!({ "permission_mode": m })),
            )
            .await;
        assert_eq!(
            h.api.get(&format!("/seats/{s}")).await["permission_mode"],
            m
        );
    }
    h.wait("node connected for commands", || async {
        h.api.get("/seats/a").await["presence"]["node_connected"] == true
    })
    .await;
    for s in ["a", "b", "x", "y"] {
        h.api.post(&format!("/seats/{s}/sleep"), json!({})).await;
        wait_state(&h, s, "dormant").await;
        h.api.post(&format!("/seats/{s}/wake"), json!({})).await;
        wait_for("the second start", || launches(&h, s).len() == 2).await;
        wait_state(&h, s, "idle").await;
    }
    assert_eq!(skips("a"), [false, false]);
    assert_eq!(skips("b"), [true, true]);
    assert_eq!(skips("x"), [false, false]);
    assert_eq!(skips("y"), [true, true]);
    cli(&h, &["down"]).await;
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The staged-merge template: worktrees and branches (idempotent), roles
/// for each position; validation errors and a missing node change nothing.
async fn staged_merge_worktrees_and_validation(b: Backend) {
    let Some(h) = setup(b, "staged_merge_worktrees_and_validation", None).await else {
        return;
    };
    let repo: PathBuf = h.dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README"), "hello\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    // A broken file: an error naming the entry, nothing created.
    write_team(
        &h,
        "[team]\nworkroot = \"@DIR@/wt\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\nparent = \"nobody\"\n",
    );
    let f = h
        .dir
        .path()
        .join("amail.toml")
        .to_string_lossy()
        .into_owned();
    let (ok, out) = cli(&h, &["up", "-f", &f]).await;
    assert!(!ok);
    assert!(
        out.contains("[[seat]] #1 \"a\": unknown parent \"nobody\""),
        "{out}"
    );
    // No node: an error saying so, nothing created.
    write_team(
        &h,
        &format!(
            r#"
[team]
template = "staged-merge"
repo = "{}"
workroot = "@DIR@/wt"
[defaults]
harness = "claude"
[[seat]]
name = "root"
role = "Ship it."
[[group]]
name = "core"
parent = "root"
lead = {{ name = "core-lead", harness = "codex" }}
workers = {{ count = 2, prefix = "core" }}
"#,
            repo.display()
        ),
    );
    let (ok, out) = cli_sock(&h, &h.dir.path().join("no-node.sock"), &["up", "-f", &f]).await;
    assert!(!ok);
    assert!(out.contains("the amail node is not running"), "{out}");
    let seats = h.api.get("/seats").await;
    assert_eq!(seats.as_array().unwrap().len(), 1, "{seats}");
    assert!(!h.dir.path().join("wt").exists());

    let out = up(&h, &["--no-start"]).await;
    assert!(out.contains("git: added worktree"), "{out}");
    assert!(
        out.contains("not started: root, core-lead, core-1, core-2"),
        "{out}"
    );
    let wt = h.dir.path().join("wt");
    for s in ["root", "core-lead", "core-1", "core-2"] {
        assert!(wt.join(s).join("README").exists(), "{s}");
        assert_eq!(
            git(&wt.join(s), &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            format!("amail/{s}")
        );
    }
    let branches = git(&repo, &["branch", "--list", "amail/*"]);
    assert!(branches.contains("amail/core-lead-next"), "{branches}");
    assert!(!branches.contains("amail/root-next"), "{branches}");
    let role = h.api.get("/seats/core-1").await["role"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        role.contains(&format!("{}/core-1 on branch `amail/core-1`", wt.display())),
        "{role}"
    );
    assert!(role.contains("amail send --to core-lead"), "{role}");
    assert!(role.contains("onto `amail/core-lead-next`"), "{role}");
    let root = h.api.get("/seats/root").await["role"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(root.contains("merge it into `main`"), "{root}");
    assert!(root.ends_with("Ship it."), "{root}");
    for r in ps(&h).await {
        assert_eq!(r.state, "offline", "{r:?}");
        assert!(r.mux_session.starts_with("amail-acme-"), "{r:?}");
    }
    // Again: no git changes.
    let out = up(&h, &["--no-start"]).await;
    assert!(!out.contains("git:"), "{out}");
}

/// A session that never had a turn cannot be resumed: after a crash the seat
/// starts a new session, without claiming that context was lost.
async fn session_without_a_turn_starts_fresh(b: Backend) {
    let Some(h) = setup(b, "session_without_a_turn_starts_fresh", None).await else {
        return;
    };
    let _i = inbox(&h, "n");
    set_mode(&h, "n", "noturn");
    write_team(
        &h,
        "[team]\nworkroot = \"@DIR@/work\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"n\"\n",
    );
    up(&h, &[]).await;
    wait_state(&h, "n", "idle").await;
    kill9(row(&h, "n").await.pid.unwrap());
    wait_for("a second launch", || launches(&h, "n").len() == 2).await;
    wait_state(&h, "n", "idle").await;
    let l = launches(&h, "n");
    assert!(l[1].contains("--session-id"), "{l:?}");
    assert!(!l[1].contains("--resume"), "{l:?}");
    assert!(!l[1].contains("This is a new session"), "{l:?}");
    // Once it had a turn, it is resumed.
    let id = h.api.send(&["n"], &[], "work", false).await;
    wait_injected(&h, "n", id).await;
    wait_state(&h, "n", "idle").await;
    let sid = l[1]
        .split(' ')
        .skip_while(|w| *w != "--session-id")
        .nth(1)
        .unwrap()
        .to_string();
    kill9(row(&h, "n").await.pid.unwrap());
    wait_for("a third launch", || launches(&h, "n").len() == 3).await;
    assert!(launches(&h, "n")[2].contains(&format!("--resume {sid}")));
    cli(&h, &["down"]).await;
}

/// Mail `To` a generated list wakes its idle members and starts its
/// sleeping ones, like direct mail; a list in `Cc` does neither.
async fn list_mail_wakes_and_cold_starts(b: Backend) {
    let Some(h) = setup(b, "list_mail_wakes_and_cold_starts", None).await else {
        return;
    };
    let _i: Vec<FakeInbox> = ["l", "a"].iter().map(|s| inbox(&h, s)).collect();
    let team = r#"
[team]
workroot = "@DIR@/work"
[defaults]
harness = "claude"
[[group]]
name = "g"
lead = { name = "l" }
workers = [{ name = "a" }, { name = "b", harness = "codex" }]
"#;
    write_team(&h, team);
    up(&h, &[]).await;
    for s in ["l", "a", "b"] {
        wait_state(&h, s, "idle").await;
    }
    let members = list_members(&h, "g").await;
    assert_eq!(members, vec!["a", "b", "l"]);
    let (ok, out) = cli(&h, &["sleep", "b"]).await;
    assert!(ok, "{out}");

    // A list in Cc: not shown at once, nobody woken or started.
    h.api.send(&["l"], &["g"], "fyi", false).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(row(&h, "b").await.state, "dormant");
    assert_eq!(launches(&h, "b").len(), 1);

    // A list in To: everyone gets it now; the sleeping Codex seat resumes.
    let id = h.api.send(&["g"], &[], "to the group", false).await;
    for s in ["l", "a", "b"] {
        wait_injected(&h, s, id).await;
    }
    assert!(
        injected(&h, "a").contains("via list g"),
        "{}",
        injected(&h, "a")
    );
    assert!(launches(&h, "b")[1].contains(" resume "));

    // Changed settings are reported (and apply at once).
    write_team(
        &h,
        &team.replace("[defaults]", "idle_sleep = \"1m\"\n[defaults]"),
    );
    let out = up(&h, &[]).await;
    assert!(
        out.contains("settings changed (in effect now): idle_sleep off -> 60s"),
        "{out}"
    );
    cli(&h, &["down"]).await;
}

/// When native wake-ups fail, a seat with `paste_fallback` gets the nudge
/// typed into its pane, but only if its screen shows an empty input box;
/// with a dialog open, or without the option, it is marked wake_failed.
async fn paste_fallback_types_only_when_safe(b: Backend) {
    let Some(h) = setup(
        b,
        "paste_fallback_types_only_when_safe",
        Some(|c| {
            c.wake_confirm = Duration::from_millis(700);
            c.wake_window = Duration::from_millis(100);
        }),
    )
    .await
    else {
        return;
    };
    let _i: Vec<FakeInbox> = ["p", "q", "r"].iter().map(|s| inbox(&h, s)).collect();
    set_mode(&h, "p", "deaf");
    set_mode(&h, "q", "deafdialog");
    set_mode(&h, "r", "deaf");
    write_team(
        &h,
        r#"
[team]
workroot = "@DIR@/work"
[defaults]
harness = "claude"
paste_fallback = true
[[seat]]
name = "p"
[[seat]]
name = "q"
[[seat]]
name = "r"
paste_fallback = false
"#,
    );
    up(&h, &[]).await;
    for s in ["p", "q", "r"] {
        wait_state(&h, s, "idle").await;
    }
    let pane = |seat: &str| {
        let h = &h;
        let seat = seat.to_string();
        async move { row(h, &seat).await.pane.unwrap() }
    };
    let (pp, pq, pr) = (pane("p").await, pane("q").await, pane("r").await);
    let typed = |pane: u32| -> Vec<String> {
        h.mux
            .as_ref()
            .unwrap()
            .typed
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, p, _)| *p == pane)
            .map(|(_, _, t)| t.clone())
            .collect()
    };
    let ids: Vec<i64> = {
        let mut v = Vec::new();
        for s in ["p", "q", "r"] {
            v.push(h.api.send(&[s], &[], &format!("for {s}"), false).await);
        }
        v
    };
    // p: two native wake-ups go unanswered, then the nudge is typed and
    // Enter pressed (the "TUI" submits it as a prompt).
    let t0 = Instant::now();
    while typed(pp).len() < 2 {
        if t0.elapsed() > Duration::from_secs(20) {
            let m = h.mux.as_ref().unwrap();
            let r = row(&h, "p").await;
            let screen = amail_node::mux::Mux::screen(&**m, &r.mux_session, pp).await;
            panic!(
                "nothing typed into p: {r:?}\nscreen: {screen:?}\ntyped: {:?}",
                typed(pp)
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let t = typed(pp);
    assert!(
        t[0].starts_with("[amail] You have new mail (wake #"),
        "{t:?}"
    );
    assert_eq!(t[1], "\n");
    std::fs::write(h.fake.join("p.prompt"), "").unwrap();
    wait_injected(&h, "p", ids[0]).await;
    wait_state(&h, "p", "idle").await;
    assert_eq!(row(&h, "p").await.attention, None);
    // q (a dialog on screen) and r (no fallback): nothing typed, flagged.
    for s in ["q", "r"] {
        let t0 = Instant::now();
        while row(&h, s).await.attention.as_deref() != Some("wake-failed") {
            assert!(t0.elapsed() < Duration::from_secs(20), "{s} not flagged");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    assert!(typed(pq).is_empty() && typed(pr).is_empty());
    let st = h.api.get(&format!("/messages/{}/status", ids[1])).await;
    assert_eq!(st["recipients"][0]["state"], "queued");
}
