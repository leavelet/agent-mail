//! `amail task …` against `amail serve` (a child process), on SQLite and
//! (when `AMAIL_TEST_PG_URL` is set) Postgres.
//!
//! The fixed scenario: three seats (`lead`, `w1`, `w2`, all in team
//! `core`), eight tasks created by `lead`, two dependency chains:
//!
//! - chain A: t1 (w1) blocks t2 (w2), which blocks t3 (w1);
//! - chain B: t4 (w2) and t8 (lead, `doing`) both block t5 (w1);
//! - t6 (w2) is `blocked` waiting for `lead`; t7 is unassigned in `core`.

use amail_proto as p;
use amail_store::testutil::{Backend, fresh_url};
use serde_json::Value;
use std::collections::BTreeSet;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_amail");

#[tokio::test(flavor = "multi_thread")]
async fn tasks_cli_sqlite() {
    run(Backend::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tasks_cli_postgres() {
    run(Backend::Postgres).await;
}

struct ServerProc(Child);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Amail {
    url: String,
    token: String,
}

impl Amail {
    fn try_run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .env("AMAIL_URL", &self.url)
            .env("AMAIL_TOKEN", &self.token)
            .env_remove("AMAIL_PROFILE")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    fn run(&self, args: &[&str]) -> String {
        let out = self.try_run(args);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(
            out.status.success(),
            "amail {args:?} failed: {}{}",
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }

    /// Run a command that must fail; returns stderr.
    fn fail(&self, args: &[&str]) -> String {
        let out = self.try_run(args);
        assert!(!out.status.success(), "amail {args:?} should fail");
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        serde_json::from_str(&self.run(&a)).unwrap()
    }

    fn with_token(&self, token: &str) -> Amail {
        Amail {
            url: self.url.clone(),
            token: token.trim().to_string(),
        }
    }

    /// Create a task; returns its number.
    fn new_task(&self, args: &[&str]) -> i64 {
        let mut a = vec!["task", "new"];
        a.extend_from_slice(args);
        self.json(&a)["task"]["num"].as_i64().unwrap()
    }

    /// The numbers of a `{tasks: [...]}` answer, in order.
    fn nums(&self, args: &[&str]) -> Vec<i64> {
        self.json(args)["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["num"].as_i64().unwrap())
            .collect()
    }

    /// `waiting` / `owed` as `(task, seat, other task)`.
    fn items(&self, cmd: &str) -> BTreeSet<(i64, String, Option<i64>)> {
        let other = if cmd == "waiting" {
            "blocker"
        } else {
            "blocks"
        };
        self.json(&["task", cmd])["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["task"]["num"].as_i64().unwrap(),
                    i["seat"].as_str().unwrap_or("").to_string(),
                    i[other]["num"].as_i64(),
                )
            })
            .collect()
    }

    fn state(&self, task: &str) -> String {
        self.json(&["task", "show", task])["task"]["state"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

fn set(v: &[(i64, &str, Option<i64>)]) -> BTreeSet<(i64, String, Option<i64>)> {
    v.iter().map(|(a, b, c)| (*a, b.to_string(), *c)).collect()
}

fn admin_create_org(db: &str, org: &str) -> String {
    let out = Command::new(BIN)
        .args(["admin", "create-org", org, "--db", db])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

async fn start_server(db: &str) -> (ServerProc, String) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let listen = format!("127.0.0.1:{port}");
    let child = Command::new(BIN)
        .args(["serve", "--db", db, "--listen", &listen])
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let proc = ServerProc(child);
    let url = format!("http://{listen}");
    for _ in 0..200 {
        if let Ok(r) = reqwest::get(format!("{url}/healthz")).await
            && r.status().is_success()
        {
            return (proc, url);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("server did not start");
}

async fn run(backend: Backend) {
    let Some((db, _guard)) = fresh_url(backend, "tasks_cli").await else {
        return;
    };
    let token = admin_create_org(&db, "acme");
    let (_server, url) = start_server(&db).await;
    let owner = Amail { url, token };
    for s in ["lead", "w1", "w2"] {
        owner.run(&["seat", "add", s, "--team", "core"]);
    }
    let lead = owner.with_token(&owner.run(&["seat", "token", "lead"]));
    let w1 = owner.with_token(&owner.run(&["seat", "token", "w1"]));
    let w2 = owner.with_token(&owner.run(&["seat", "token", "w2"]));

    // The scenario.
    let t1 = lead.new_task(&[
        "Parse the input",
        "--assignee",
        "w1",
        "-m",
        "line one\nline two",
    ]);
    let t2 = lead.new_task(&["Check the AST", "--assignee", "w2"]);
    let t3 = lead.new_task(&["Emit code", "--assignee", "w1"]);
    let t4 = lead.new_task(&["Write the schema", "--assignee", "w2", "--priority", "high"]);
    let t5 = lead.new_task(&[
        "Migrate the data",
        "--assignee",
        "w1",
        "--label",
        "db",
        "--due",
        "2026-12-01",
    ]);
    let t6 = lead.new_task(&["Get the keys", "--assignee", "w2"]);
    let t7 = lead.new_task(&["Tidy the docs", "--team", "core", "--priority", "low"]);
    let t8 = lead.new_task(&["Set up CI", "--assignee", "lead"]);
    assert_eq!([t1, t2, t3, t4, t5, t6, t7, t8], [1, 2, 3, 4, 5, 6, 7, 8]);
    lead.run(&["task", "dep", "t2", "--on", "t1"]);
    lead.run(&["task", "dep", "3", "--on", "2"]);
    lead.run(&["task", "dep", "t5", "--on", "t4"]);
    let line = lead.run(&["task", "dep", "t5", "--on", "t8"]);
    assert_eq!(
        line.trim(),
        "t5   open    normal w1       Migrate the data  · waits on t4, t8"
    );
    let line = w2.run(&[
        "task",
        "block",
        "t6",
        "--on",
        "lead",
        "-m",
        "need the API keys",
    ]);
    assert_eq!(
        line.trim(),
        "t6   blocked normal w2       Get the keys  · waits for lead: need the API keys"
    );
    lead.run(&["task", "start", "t8"]);

    // The personal queries of each seat.
    assert_eq!(w1.nums(&["task", "mine"]), vec![1, 3, 5]);
    assert_eq!(w1.nums(&["task", "next"]), vec![1]);
    assert_eq!(w1.nums(&["task", "next", "--pool"]), vec![1, 7]);
    assert_eq!(
        w1.items("waiting"),
        set(&[(3, "w2", Some(2)), (5, "w2", Some(4)), (5, "lead", Some(8))])
    );
    assert_eq!(w1.items("owed"), set(&[(1, "w2", Some(2))]));

    assert_eq!(w2.nums(&["task", "mine"]), vec![4, 6, 2]);
    assert_eq!(w2.nums(&["task", "next"]), vec![4]);
    assert_eq!(
        w2.items("waiting"),
        set(&[(6, "lead", None), (2, "w1", Some(1))])
    );
    // t4 is not the last open blocker of t5 (t8 is open too).
    assert_eq!(w2.items("owed"), set(&[(2, "w1", Some(3))]));

    assert_eq!(lead.nums(&["task", "mine"]), vec![8]);
    assert!(lead.nums(&["task", "next"]).is_empty());
    assert!(lead.items("waiting").is_empty());
    assert_eq!(lead.items("owed"), set(&[(6, "w2", None)]));

    // Text forms.
    assert_eq!(
        w1.run(&["task", "mine"]),
        "t1   ready   normal w1       Parse the input\n\
         t3   open    normal w1       Emit code  · waits on t2\n\
         t5   open    normal w1       Migrate the data  · waits on t4, t8\n"
    );
    assert_eq!(lead.run(&["task", "next"]).trim(), "nothing ready");
    let waiting = w2.run(&["task", "waiting"]);
    assert!(
        waiting.contains(
            "t6   blocked normal w2       Get the keys — waits for lead: need the API keys"
        ),
        "{waiting}"
    );
    assert!(
        waiting.contains("t2   open    normal w2       Check the AST — waits for w1 (t1 ready)"),
        "{waiting}"
    );
    let owed = w1.run(&["task", "owed"]);
    assert_eq!(
        owed.trim(),
        "t1   ready   normal w1       Parse the input — w2 waits for it (t2 open)"
    );
    assert_eq!(
        lead.run(&["task", "owed"]).trim(),
        "t6   blocked normal w2       Get the keys — w2 waits for you: need the API keys"
    );
    let board = owner.run(&["task", "board"]);
    for want in [
        "ready 3 · open 3 · doing 1 · blocked 1 · review 0 · done 0 · cancelled 0",
        "ready (3):\nt4   ready   high  w2       Write the schema",
        "load:\n",
        "chains:\n",
        "t5 waits on t4, t8",
        "t3 waits on t2",
    ] {
        assert!(board.contains(want), "{want:?} not in\n{board}");
    }
    let show = w1.run(&["task", "show", "t5"]);
    for want in [
        "t5 Migrate the data\n",
        "State: open\n",
        "Priority: normal (rank ",
        "Reporter: lead\n",
        "Assignee: w1\n",
        "Labels: db\n",
        "Due: 2026-12-01 00:00Z\n",
        "Waits on: t4, t8\n",
        "Blockers:\n  t4   ready   w2       Write the schema\n  t8   doing   lead     Set up CI\n",
        "  lead  created: Migrate the data\n",
        "  lead  depends on t4\n",
    ] {
        assert!(show.contains(want), "{want:?} not in\n{show}");
    }
    let show = w1.run(&["task", "show", "t1"]);
    assert!(show.contains("\n\n  > line one\n  > line two\n"), "{show}");
    assert!(
        show.contains("Blocked:\n  t2   open    w2       Check the AST"),
        "{show}"
    );
    let history = lead.run(&["task", "history", "t8"]);
    assert!(
        history.contains("  lead  changed state: open -> doing\n"),
        "{history}"
    );
    assert!(history.contains("  lead  now blocks t5"), "{history}");
    assert_eq!(
        lead.run(&["task", "history", "t8"]),
        lead.run(&["task", "history", "t8", "--all"])
    );

    // `ready` exactly when the last blocker closes.
    w1.run(&["task", "start", "t1"]);
    assert_eq!(w2.state("t2"), "open");
    w1.run(&["task", "done", "t1"]);
    assert_eq!(w2.state("t2"), "ready");
    assert_eq!(w2.nums(&["task", "next"]), vec![4, 2]);
    assert_eq!(w1.state("t3"), "open");
    w2.run(&["task", "done", "t2"]);
    assert_eq!(w1.state("t3"), "ready");
    w2.run(&["task", "done", "t4"]);
    assert_eq!(w1.state("t5"), "open");
    // t8 is now the last open blocker of t5.
    assert_eq!(
        lead.items("owed"),
        set(&[(6, "w2", None), (8, "w1", Some(5))])
    );
    lead.run(&["task", "done", "t8"]);
    assert_eq!(w1.state("t5"), "ready");
    assert_eq!(w1.nums(&["task", "next"]), vec![3, 5]);
    let history = w1.run(&["task", "history", "t5"]);
    assert!(
        history.contains("  lead  unblocked: t8 closed"),
        "{history}"
    );
    // Reopening the blocker makes t5 wait again; removing the dependency
    // frees it.
    lead.run(&["task", "reopen", "t8"]);
    assert_eq!(w1.state("t5"), "open");
    lead.run(&["task", "undep", "t5", "--on", "t8"]);
    assert_eq!(w1.state("t5"), "ready");
    w2.run(&["task", "unblock", "t6"]);
    assert_eq!(w2.state("t6"), "ready");

    // Pool work: take, assign, set.
    w1.run(&["task", "take", "t7"]);
    let err = w2.fail(&["task", "take", "t7"]);
    assert!(err.starts_with("error: taken: "), "{err}");
    w1.run(&["task", "assign", "t7", "none"]);
    let line = w2.run(&["task", "take", "7"]);
    assert!(line.starts_with("t7   ready   low   w2 "), "{line}");
    let line = owner.run(&[
        "task",
        "set",
        "t7",
        "--priority",
        "urgent",
        "--title",
        "Tidy all docs",
        "--label",
        "docs",
        "--label",
        "later",
        "--due",
        "none",
    ]);
    assert_eq!(line.trim(), "t7   ready   urgent w2       Tidy all docs");
    let d = owner.json(&["task", "show", "t7"]);
    assert_eq!(d["task"]["labels"], serde_json::json!(["docs", "later"]));
    owner.run(&["task", "set", "t7", "--no-labels", "--state", "review"]);
    assert_eq!(owner.state("t7"), "review");
    owner.run(&["task", "cancel", "t7"]);
    assert_eq!(owner.state("t7"), "cancelled");

    // Listings and filters.
    assert_eq!(
        owner.nums(&["task", "list", "--assignee", "w1"]),
        vec![5, 3]
    );
    assert_eq!(
        owner.nums(&["task", "list", "--state", "done,cancelled"]),
        vec![7, 4, 2, 1]
    );
    assert_eq!(
        owner.nums(&["task", "list", "--text", "schema", "--state", "all"]),
        vec![4]
    );
    assert_eq!(owner.nums(&["task", "list", "--label", "db"]), vec![5]);
    assert_eq!(owner.nums(&["task", "list", "--limit", "2"]).len(), 2);
    let list = owner.run(&["task", "list", "--limit", "1"]);
    assert!(list.contains("(older: amail task list --before "), "{list}");

    // Comments and links.
    let out = w1.run(&["task", "comment", "t5", "-m", "starting now"]);
    assert!(out.starts_with("commented on t5: #"), "{out}");
    let c = w1.json(&["task", "comment", "t5", "-m", "half done"]);
    let thread = c["thread_id"].as_i64().unwrap();
    let inbox = lead.run(&["inbox"]);
    assert!(inbox.contains("[t5] Migrate the data"), "{inbox}");
    let th = lead.run(&["thread", &thread.to_string()]);
    assert!(
        th.contains("tasks of this thread:\n  t5   ready   w1       Migrate the data"),
        "{th}"
    );
    let m = w1.json(&[
        "send",
        "--to",
        "w2",
        "-s",
        "about the data",
        "-m",
        "see the migration",
    ]);
    let (mid, mthread) = (m["id"].as_i64().unwrap(), m["thread_id"].as_i64().unwrap());
    let out = w1.run(&["task", "link", "t3", &mid.to_string()]);
    assert!(out.contains(&format!("linked threads: {mthread}")), "{out}");
    let from = w2.new_task(&["--from", &mid.to_string()]);
    let d = w2.json(&["task", "show", &from.to_string()]);
    assert_eq!(d["task"]["title"], "about the data");
    assert_eq!(d["links"][0]["thread_id"], mthread);
    let out = w1.run(&["task", "unlink", "t3", &mthread.to_string()]);
    assert_eq!(out.trim(), format!("unlinked thread {mthread} from t3"));

    // `--json` prints the server's answer, which the wire types read and
    // write back unchanged.
    let raw = w1.json(&["task", "show", "t5"]);
    let d: p::TaskDetail = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&d).unwrap(), raw);
    let raw = owner.json(&["task", "board"]);
    let b: p::TaskBoard = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&b).unwrap(), raw);
    let raw = w1.json(&["task", "mine"]);
    let l: p::TaskList = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&l).unwrap(), raw);
    let raw = w1.json(&["task", "history", "t5", "--all"]);
    let e: p::TaskEventsPage = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(&e).unwrap(), raw);
    for cmd in ["waiting", "owed"] {
        let raw = w1.json(&["task", cmd]);
        let back = if cmd == "waiting" {
            serde_json::to_value(serde_json::from_value::<p::WaitingList>(raw.clone()).unwrap())
        } else {
            serde_json::to_value(serde_json::from_value::<p::OwedList>(raw.clone()).unwrap())
        };
        assert_eq!(back.unwrap(), raw);
    }

    // Errors show the server's code.
    let err = w1.fail(&["task", "show", "t99"]);
    assert!(err.starts_with("error: not_found: "), "{err}");
    let err = lead.fail(&["task", "dep", "t1", "--on", "t3"]);
    assert!(err.starts_with("error: dependency_cycle: "), "{err}");
    let err = w1.fail(&["task", "set", "t5", "--state", "ready"]);
    assert!(err.starts_with("error: invalid: "), "{err}");
    let err = w1.fail(&["task", "new", "x", "--assignee", "nobody"]);
    assert!(err.starts_with("error: not_found: "), "{err}");
    let err = w1.fail(&["task", "show", "tx"]);
    assert!(err.contains("is not a task number"), "{err}");
    let err = w1.fail(&["task", "new"]);
    assert!(err.contains("give a title"), "{err}");

    // Work stealing: w2 asks for w1's t3 (ready, not started); w1 yields.
    let line = w2.run(&["task", "steal", "t3"]);
    assert_eq!(
        line.trim(),
        "t3   ready   normal w1       Emit code  · w2 asks to take over"
    );
    let mine = w1.run(&["task", "mine"]);
    assert!(
        mine.contains("t3   ready   normal w1       Emit code  · w2 asks to take over"),
        "{mine}"
    );
    let list = owner.run(&["task", "list", "--assignee", "w1"]);
    assert!(list.contains("Emit code  · w2 asks to take over"), "{list}");
    let board = owner.run(&["task", "board"]);
    assert!(
        board.contains("Emit code  · w2 asks to take over"),
        "{board}"
    );
    let show = w1.run(&["task", "show", "t3"]);
    assert!(show.contains("Takeover: asked for by w2 at "), "{show}");
    assert!(show.contains("  w2  asked to take over from w1"), "{show}");
    let line = w1.run(&["task", "yield", "t3"]);
    assert_eq!(line.trim(), "t3   ready   normal w2       Emit code");
    assert_eq!(w2.nums(&["task", "next"]), vec![3, 6]);
    // w2 asks for t5; w1 keeps it.
    w2.run(&["task", "steal", "5"]);
    let line = w1.run(&["task", "keep", "t5"]);
    assert_eq!(line.trim(), "t5   ready   normal w1       Migrate the data");
    let d = w1.json(&["task", "show", "t5"]);
    assert!(d["task"].get("steal_requested_by").is_none(), "{d}");
    let history = w1.run(&["task", "history", "t5"]);
    assert!(
        history.contains("  w1  request of w2 to take over declined"),
        "{history}"
    );
    // A started task cannot be asked for; nor can it be answered without
    // a request.
    w2.run(&["task", "start", "t6"]);
    let err = w1.fail(&["task", "steal", "t6"]);
    assert!(err.starts_with("error: started: "), "{err}");
    let err = w1.fail(&["task", "yield", "t5"]);
    assert!(err.starts_with("error: not_found: "), "{err}");
}
