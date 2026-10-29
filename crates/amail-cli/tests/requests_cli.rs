//! `amail ask`, `answer`, `requests`, `request show`, `withdraw r12`,
//! `prompt` and `interrupt` against `amail serve` (a child process), on
//! SQLite and (when `AMAIL_TEST_PG_URL` is set) Postgres.
//!
//! Seats: `w1`, `w2` (team `core`) and the user `owner`. `w1` asks the
//! person; the owner reads the mail and answers by number, key and text;
//! a stale answer, a second answer and a withdrawal are refused or shown;
//! the queue is listed and filtered; `ask --wait` ends on an answer given
//! by another process; a task with an open request is blocked-on-human and
//! cannot be taken. The seat controls run against a fake node: an event
//! stream opened with a node token and a presence report.

use amail_store::testutil::{Backend, fresh_url};
use serde_json::{Value, json};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_amail");

#[tokio::test(flavor = "multi_thread")]
async fn requests_cli_sqlite() {
    run(Backend::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_cli_postgres() {
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
    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env("AMAIL_URL", &self.url)
            .env("AMAIL_TOKEN", &self.token)
            .env_remove("AMAIL_PROFILE")
            .env_remove("CODEX_THREAD_ID")
            .stdin(Stdio::null());
        c
    }

    fn try_run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
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

    /// Run a command that must fail; returns stdout and stderr.
    fn fail(&self, args: &[&str]) -> (String, String) {
        let out = self.try_run(args);
        assert!(
            !out.status.success(),
            "amail {args:?} should fail: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
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

    /// Ask with the given arguments; returns the request number.
    fn ask(&self, args: &[&str]) -> i64 {
        let mut a = vec!["ask"];
        a.extend_from_slice(args);
        self.json(&a)["request"]["num"].as_i64().unwrap()
    }
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
    let Some((db, _guard)) = fresh_url(backend, "requests_cli").await else {
        return;
    };
    let token = admin_create_org(&db, "acme");
    let (_server, url) = start_server(&db).await;
    let owner = Amail { url, token };
    for s in ["w1", "w2"] {
        owner.run(&["seat", "add", s, "--team", "core"]);
    }
    let w1 = owner.with_token(&owner.run(&["seat", "token", "w1"]));
    let w2 = owner.with_token(&owner.run(&["seat", "token", "w2"]));

    asking_and_answering(&owner, &w1, &w2);
    queue(&owner, &w1, &w2);
    waiting(&owner, &w1);
    tasks(&owner, &w1, &w2);
    controls(&owner, &w1).await;
}

fn asking_and_answering(owner: &Amail, w1: &Amail, w2: &Amail) {
    // Ask with options, a default and a deadline.
    let out = w1.run(&[
        "ask",
        "Ship the parser?",
        "-m",
        "The tests pass; the docs are not done.",
        "--option",
        "yes:Ship it",
        "--option",
        "no:Hold it:until the docs are done",
        "--default",
        "no",
        "--deadline",
        "2h",
    ]);
    assert!(
        out.starts_with("r1  question  soon  Ship the parser?  (deadline "),
        "{out}"
    );
    assert!(out.contains(", then the default no)"), "{out}");
    assert!(
        out.contains("The answer comes as mail in the request's thread"),
        "{out}"
    );
    let r = owner.json(&["request", "show", "r1"])["request"].clone();
    assert_eq!(r["source"], "cli", "{r}");
    assert_eq!(r["seat"], "w1", "{r}");
    // The deadline is two hours away (with a minute's slack).
    let deadline: jiff::Timestamp = r["deadline"].as_str().unwrap().parse().unwrap();
    let left = deadline.as_second() - jiff::Timestamp::now().as_second();
    assert!((7_100..=7_200).contains(&left), "{left}");

    // The mail the user gets: To the owner, subject [r1], the options as
    // numbered lines and how to answer.
    let inbox = owner.json(&["inbox"]);
    let entry = inbox["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["message"]["subject"] == "[r1] Ship the parser?")
        .cloned()
        .unwrap_or_else(|| panic!("{inbox}"));
    assert_eq!(entry["delivery"]["class"], "to", "{entry}");
    let id = entry["message"]["id"].as_i64().unwrap().to_string();
    let mail = owner.run(&["read", &id]);
    for want in [
        "The tests pass; the docs are not done.",
        "1. yes — Ship it",
        "2. no — Hold it",
        "amail answer r1 <option>",
    ] {
        assert!(mail.contains(want), "{want:?} in\n{mail}");
    }

    // `request show`: the fields, the options numbered as in the mail, the
    // events, the body.
    let show = owner.run(&["request", "show", "r1"]);
    for want in [
        "r1 Ship the parser?",
        "Kind: question · Class: soon · State: open · Rev: ",
        "Seat: w1 · Source: cli",
        "Deadline: ",
        "the default is taken)",
        "Default: 2. no — Hold it",
        "Options:\n  1. yes — Ship it\n  2. no — Hold it  (default)\n     until the docs are done",
        "Thread: ",
        "Events:",
        "  w1  created",
        "  > The tests pass; the docs are not done.",
        "Answer it (user): amail answer r1 <option number, key or label>",
    ] {
        assert!(show.contains(want), "{want:?} in\n{show}");
    }
    // The asker may see it; another seat may not.
    w1.run(&["request", "show", "r1"]);
    let (_, err) = w2.fail(&["request", "show", "r1"]);
    assert!(err.contains("not_found"), "{err}");

    // Only the user answers.
    let (_, err) = w1.fail(&["answer", "r1", "yes"]);
    assert!(err.contains("forbidden"), "{err}");
    // An option the request does not have.
    let (_, err) = owner.fail(&["answer", "r1", "maybe"]);
    assert!(err.contains("invalid_option"), "{err}");
    // A stale revision: nothing changes, and the request is shown.
    let (out, err) = owner.fail(&["answer", "r1", "1", "--if-rev", "999"]);
    assert!(err.contains("stale"), "{err}");
    assert!(out.contains("r1 changed since your revision"), "{out}");
    assert!(out.contains("r1 is still open"), "{out}");
    // By number, with the current revision and a note.
    let rev = owner.json(&["request", "show", "r1"])["request"]["rev"]
        .as_i64()
        .unwrap()
        .to_string();
    let out = owner.run(&["answer", "r1", "1", "--if-rev", &rev, "-m", "go ahead"]);
    assert_eq!(
        out.trim(),
        "r1 answered by owner: 1. yes — Ship it\nNote: go ahead"
    );
    // A second answer: 409 answered, with the first answer.
    let (out, err) = owner.fail(&["answer", "r1", "no"]);
    assert!(err.contains("answered"), "{err}");
    assert!(
        out.contains(
            "r1 was answered before; nothing was changed.\nr1 answered by owner: 1. yes — Ship it"
        ),
        "{out}"
    );
    // The seat gets the answer as mail in the request's thread.
    let inbox = w1.json(&["inbox"]);
    let answer = inbox["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["message"]["subject"] == "Re: [r1] Ship the parser?")
        .cloned()
        .unwrap_or_else(|| panic!("{inbox}"));
    let mail = w1.run(&["read", &answer["message"]["id"].to_string()]);
    assert!(mail.contains("Answer: 1. yes — Ship it"), "{mail}");
    let show = w1.run(&["request", "show", "r1"]);
    assert!(
        show.contains("Answer: 1. yes — Ship it · by owner at "),
        "{show}"
    );
    assert!(show.contains("Answer note: go ahead"), "{show}");
    assert!(!show.contains("Answer it (user)"), "{show}");
    // The deadline no longer takes the default.
    assert!(!show.contains("the default is taken"), "{show}");
    assert!(show.contains("; answered before it)"), "{show}");

    // By key.
    let n = w1.ask(&[
        "Which database?",
        "--option",
        "pg:Postgres",
        "--option",
        "lite:SQLite",
    ]);
    assert_eq!(n, 2);
    let out = owner.run(&["answer", "r2", "lite"]);
    assert_eq!(out.trim(), "r2 answered by owner: 2. lite — SQLite");
    // Free text.
    let n = w1.ask(&["What should the release be called?", "--free-text"]);
    let out = owner.run(&["answer", &format!("r{n}"), "Parsley"]);
    assert_eq!(out.trim(), format!("r{n} answered by owner: Parsley"));

    // Withdraw: the asker withdraws with a note; a second time is refused.
    let n = w1.ask(&["Rename the crate?", "--option", "a:Yes", "--option", "b:No"]);
    let r = format!("r{n}");
    let out = w1.run(&["withdraw", &r, "-m", "decided in review"]);
    assert_eq!(out.trim(), format!("withdrew {r}: Rename the crate?"));
    let (out, err) = w1.fail(&["withdraw", &r]);
    assert!(err.contains("not_open"), "{err}");
    assert!(out.contains(&format!("{r} was withdrawn")), "{out}");
    let show = owner.run(&["request", "show", &r]);
    assert!(show.contains("State: withdrawn"), "{show}");
    assert!(show.contains("withdrawn (decided in review)"), "{show}");
    // `withdraw` still withdraws messages, which need --from.
    let (_, err) = w1.fail(&["withdraw", "12"]);
    assert!(err.contains("--from"), "{err}");
}

fn queue(owner: &Amail, w1: &Amail, w2: &Amail) {
    // Open requests: two `soon` from w1, one `now` from w2, one `later`.
    let a = w1.ask(&[
        "Merge the branch?",
        "--kind",
        "merge",
        "--option",
        "merge:Merge",
        "--option",
        "reject:Reject",
    ]);
    let b = w2.ask(&[
        "Rotate the keys?",
        "--class",
        "now",
        "--option",
        "y:Yes",
        "--option",
        "n:No",
        "--default",
        "n",
    ]);
    let c = w1.ask(&[
        "Plan for the cache",
        "--kind",
        "plan",
        "--class",
        "later",
        "--free-text",
    ]);
    // `now` first, then the older `soon`, then `later`.
    let list = owner.run(&["requests"]);
    let lines: Vec<&str> = list.lines().collect();
    assert_eq!(lines.len(), 3, "{list}");
    assert!(lines[0].starts_with(&format!("r{b}")), "{list}");
    assert!(lines[1].starts_with(&format!("r{a}")), "{list}");
    assert!(lines[2].starts_with(&format!("r{c}")), "{list}");
    assert!(lines[0].contains("open      now    question  w2"), "{list}");
    assert!(lines[0].contains("s ago"), "{list}");
    assert!(
        lines[0].contains("Rotate the keys?  · default n at "),
        "{list}"
    );
    assert!(lines[1].contains("· no default, reminders at "), "{list}");
    // Filters.
    let one = |args: &[&str]| -> Vec<i64> {
        let mut x = vec!["requests"];
        x.extend_from_slice(args);
        owner.json(&x)["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["num"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(one(&["--kind", "plan"]), vec![c]);
    assert_eq!(one(&["--class", "now"]), vec![b]);
    assert_eq!(one(&["--seat", "w2"]), vec![b]);
    assert_eq!(one(&["--limit", "1"]), vec![b]);
    let all = one(&["--state", "all"]);
    assert_eq!(all.len(), 7, "{all:?}");
    assert_eq!(one(&["--state", "withdrawn"]), vec![4]);
    let answered = owner.run(&["requests", "--state", "answered"]);
    assert!(answered.contains("· 2. lite — SQLite"), "{answered}");
    // A seat sees its own requests.
    let mine = w2.json(&["requests"]);
    assert_eq!(mine["requests"].as_array().unwrap().len(), 1, "{mine}");
    // Nothing matches.
    let none = owner.run(&["requests", "--kind", "attention"]);
    assert_eq!(none.trim(), "no open requests");
    for n in [a, b, c] {
        owner.run(&["withdraw", &format!("r{n}")]);
    }
    assert_eq!(owner.run(&["requests"]).trim(), "no open requests");
}

fn waiting(owner: &Amail, w1: &Amail) {
    // `ask --wait` in another process ends on the owner's answer.
    let child = w1
        .command(&[
            "ask",
            "Deploy now?",
            "--option",
            "go:Deploy",
            "--option",
            "wait:Wait",
            "--wait",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    let num = loop {
        let open = owner.json(&["requests"]);
        if let Some(r) = open["requests"].as_array().unwrap().first() {
            break r["num"].as_i64().unwrap();
        }
        assert!(Instant::now() < end, "the request did not appear");
        std::thread::sleep(Duration::from_millis(100));
    };
    std::thread::sleep(Duration::from_millis(300));
    owner.run(&["answer", &format!("r{num}"), "wait", "-m", "after lunch"]);
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.starts_with(&format!("r{num}  question  soon  Deploy now?")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "r{num} answered by owner: 2. wait — Wait\nNote: after lunch"
        )),
        "{stdout}"
    );
    // A request answered before the wait: the wait ends at once.
    let w = w1.json(&["request", "show", &format!("r{num}")]);
    assert_eq!(w["request"]["state"], "answered");
}

fn tasks(owner: &Amail, w1: &Amail, w2: &Amail) {
    let t = owner.json(&["task", "new", "Choose the cache"])["task"]["num"]
        .as_i64()
        .unwrap();
    let task = format!("t{t}");
    let n = w1.ask(&[
        "Which cache?",
        "--kind",
        "plan",
        "--task",
        &task,
        "--option",
        "lru:LRU",
        "--option",
        "arc:ARC",
    ]);
    let r = format!("r{n}");
    let line = owner.run(&["task", "list"]);
    assert!(
        line.lines()
            .any(|x| x.starts_with(&task) && x.ends_with(&format!("  · blocked-on-human ({r})"))),
        "{line}"
    );
    let show = owner.run(&["task", "show", &task]);
    assert!(
        show.contains(&format!("· blocked-on-human ({r}: amail request show {r})")),
        "{show}"
    );
    // The request lists it.
    let list = owner.run(&["requests", "--task", &task]);
    assert!(list.contains(&format!("Which cache?  · {task}")), "{list}");
    // Take is refused, plainly.
    let (_, err) = w2.fail(&["task", "take", &task]);
    assert!(err.contains("blocked_on_human"), "{err}");
    assert!(
        err.contains(&format!("waits for the person's answer to {r}")),
        "{err}"
    );
    assert!(
        err.contains("the task is blocked-on-human until the request is answered or withdrawn"),
        "{err}"
    );
    // Answered: the task is free again.
    owner.run(&["answer", &r, "lru"]);
    let show = owner.run(&["task", "show", &task]);
    assert!(!show.contains("blocked-on-human"), "{show}");
    w2.run(&["task", "take", &task]);
}

/// Read the stream until a `command` event arrives; its data.
async fn next_command(resp: &mut reqwest::Response, buf: &mut String) -> Value {
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(i) = buf.find("\n\n") {
            let event: String = buf.drain(..i + 2).collect();
            if event.lines().any(|l| l == "event: command") {
                let data: String = event
                    .lines()
                    .filter_map(|l| l.strip_prefix("data: ").or(l.strip_prefix("data:")))
                    .collect();
                return serde_json::from_str(&data).unwrap();
            }
            continue;
        }
        let chunk = tokio::time::timeout_at(end, resp.chunk())
            .await
            .expect("no command within 10 s")
            .unwrap()
            .expect("the stream ended");
        buf.push_str(&String::from_utf8_lossy(&chunk).replace("\r\n", "\n"));
    }
}

async fn controls(owner: &Amail, w1: &Amail) {
    // No node: plainly.
    let (_, err) = owner.fail(&["prompt", "w1", "-m", "hi"]);
    assert!(
        err.contains("w1: no node is connected for this seat"),
        "{err}"
    );
    assert!(err.contains("no_node"), "{err}");
    let (_, err) = owner.fail(&["interrupt", "w1"]);
    assert!(
        err.contains("w1: no node is connected for this seat"),
        "{err}"
    );
    // A fake node: an event stream with a node token, and a presence report
    // with a Codex seat and a Claude Code seat.
    let http = reqwest::Client::new();
    let node_token = owner.json(&["token", "node"]);
    let node_token = node_token["token"].as_str().unwrap().to_string();
    let mut stream = http
        .get(format!("{}/v1/events", owner.url))
        .bearer_auth(&node_token)
        .send()
        .await
        .unwrap();
    assert!(stream.status().is_success());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let r = http
        .post(format!("{}/v1/presence", owner.url))
        .bearer_auth(&node_token)
        .json(&json!({ "node": "h1", "seats": [
            { "seat": "w1", "state": "busy", "harness": "codex" },
            { "seat": "w2", "state": "busy", "harness": "claude" }
        ] }))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    let mut buf = String::new();

    // Only the user.
    let (_, err) = w1.fail(&["prompt", "w1", "-m", "hi"]);
    assert!(err.contains("forbidden"), "{err}");
    // A prompt from -m, then from stdin.
    let out = owner.run(&["prompt", "w1", "-m", "Run the tests again."]);
    assert_eq!(out.trim(), "prompt sent to w1 (through 1 node)");
    let c = next_command(&mut stream, &mut buf).await;
    assert_eq!(
        c,
        json!({ "op": "prompt", "seat": "w1", "node": "h1", "text": "Run the tests again." })
    );
    let mut child = owner
        .command(&["prompt", "w1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"Status, please.\n").unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let c = next_command(&mut stream, &mut buf).await;
    assert_eq!(c["text"], "Status, please.");
    // An empty prompt is refused before the call.
    let (_, err) = owner.fail(&["prompt", "w1", "-m", "  "]);
    assert!(err.contains("the prompt is empty"), "{err}");
    // Interrupt: Codex has one; Claude Code does not.
    let out = owner.run(&["interrupt", "w1"]);
    assert_eq!(out.trim(), "interrupt sent to w1 (through 1 node)");
    let c = next_command(&mut stream, &mut buf).await;
    assert_eq!(c, json!({ "op": "interrupt", "seat": "w1", "node": "h1" }));
    let (_, err) = owner.fail(&["interrupt", "w2"]);
    assert!(
        err.contains("w2: Claude Code sessions have no structured interrupt; use the terminal"),
        "{err}"
    );
    let v = owner.try_run(&["--json", "interrupt", "w2"]);
    assert!(!v.status.success());
    drop(stream);
    // The server's entries of the controls and the requests read as
    // sentences in `amail worklog`.
    let out = owner.run(&["worklog", "w1"]);
    for want in [
        "prompt_from_user owner sent a prompt: Run the tests again.",
        "interrupt  owner interrupted the seat's turn",
    ] {
        assert!(out.contains(want), "{want}: {out}");
    }
    assert!(
        out.lines()
            .any(|l| l.contains(" request ") && l.contains("r1 created: ")),
        "{out}"
    );
}
