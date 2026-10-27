//! `amail doc …`, `amail handover …` and `amail worklog` against `amail
//! serve` (a child process), on SQLite and (when `AMAIL_TEST_PG_URL` is
//! set) Postgres.
//!
//! Seats: `lead`, `w1`, `w2`, all in team `core`. `w1` records a finding,
//! `w2` writes a second one on the same subject (the candidates line), and
//! the documents are read, revised, split, extracted, merged, searched and
//! verified. Then `w1` writes its handover note and the worklog is read.

use amail_store::testutil::{Backend, fresh_url};
use serde_json::Value;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_amail");

#[tokio::test(flavor = "multi_thread")]
async fn docs_cli_sqlite() {
    run(Backend::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn docs_cli_postgres() {
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

    /// Run a command that must fail; returns stdout and stderr.
    fn fail(&self, args: &[&str]) -> (String, String) {
        let out = self.try_run(args);
        assert!(!out.status.success(), "amail {args:?} should fail");
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

    /// Create a document; returns its number.
    fn new_doc(&self, args: &[&str]) -> i64 {
        let mut a = vec!["doc", "new"];
        a.extend_from_slice(args);
        self.json(&a)["doc"]["num"].as_i64().unwrap()
    }

    fn rev(&self, doc: &str) -> i64 {
        self.json(&["doc", "show", doc])["doc"]["rev"]
            .as_i64()
            .unwrap()
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

const FINDING: &str = "# Observed\n\
Codex drops the hook output when the turn is longer than 30 seconds.\n\n\
# Evidence\n\
Ran codex exec with a hook that sleeps 40 seconds; see d99 and t1.\n\n\
# Applies to\n\
Codex 0.50 and later.\n";

async fn run(backend: Backend) {
    let Some((db, _guard)) = fresh_url(backend, "docs_cli").await else {
        return;
    };
    let token = admin_create_org(&db, "acme");
    let (_server, url) = start_server(&db).await;
    let owner = Amail { url, token };
    for s in ["lead", "w1", "w2"] {
        owner.run(&["seat", "add", s, "--team", "core"]);
    }
    let w1 = owner.with_token(&owner.run(&["seat", "token", "w1"]));
    let w2 = owner.with_token(&owner.run(&["seat", "token", "w2"]));

    documents(&owner, &w1, &w2);
    handover(&owner, &w1, &w2);
}

fn documents(owner: &Amail, w1: &Amail, w2: &Amail) {
    // Create: a finding with sections, a dangling link and a slug.
    let new_args = [
        "Codex drops slow hook output",
        "--kind",
        "finding",
        "-m",
        FINDING,
        "--description",
        "Read before writing a Codex hook",
        "--slug",
        "finding.codex.hooks",
    ];
    let mut a = vec!["doc", "new"];
    a.extend_from_slice(&new_args);
    let out = w1.run(&a);
    assert!(out.starts_with("d1 "), "{out}");
    assert!(out.contains("finding   draft"), "{out}");
    assert!(out.contains("rev 1 · ~"), "{out}");
    assert!(
        out.contains("dangling links (the write was kept): d99"),
        "{out}"
    );
    // The same command again answers the same document.
    let again = w1.json(&a);
    assert_eq!(again["doc"]["num"], 1, "{again}");
    assert_eq!(again["existing"], true, "{again}");
    let out = w1.run(&a);
    assert!(out.contains("nothing new was created"), "{out}");
    // A second document on the subject: created, with the candidates line.
    let out = w2.run(&[
        "doc",
        "new",
        "Codex drops slow hook output",
        "--kind",
        "finding",
        "-m",
        FINDING,
    ]);
    assert!(out.starts_with("d2 "), "{out}");
    assert!(
        out.contains("warning: similar documents exist: d1 Codex drops slow hook output"),
        "{out}"
    );
    // A required section missing is the server's refusal.
    let (_, err) = w1.fail(&["doc", "new", "No sections", "--kind", "finding"]);
    assert!(err.contains("sections_missing"), "{err}");

    // Show: fields, sections, links (dangling marked), events, body.
    let show = w1.run(&["doc", "show", "d1"]);
    for want in [
        "d1 Codex drops slow hook output",
        "Slug: finding.codex.hooks",
        "Kind: finding · Status: draft",
        "Rev: 1 · ~",
        "Description: Read before writing a Codex hook",
        "Owner: w1 · Team: core",
        "  #evidence  Evidence  ~",
        "  #evidence -> d99  (dangling)",
        "Events:",
        "  > # Observed",
    ] {
        assert!(show.contains(want), "{want:?} in\n{show}");
    }
    // A slug names the document.
    assert_eq!(
        w1.json(&["doc", "show", "finding.codex.hooks"])["doc"]["num"],
        1
    );
    // A long body is cut at 2,000 characters.
    let long = format!("{}\n", "word ".repeat(1000));
    let n = w1.new_doc(&["Long notes", "-m", &long]);
    let show = w1.run(&["doc", "show", &format!("d{n}")]);
    assert!(
        show.contains(&format!("body not shown: amail doc read d{n}")),
        "{show}"
    );

    // Read: one section, the section list, an old revision.
    let sec = w1.run(&["doc", "read", "d1#evidence"]);
    assert!(sec.starts_with("d1#evidence rev 1 · Evidence · ~"), "{sec}");
    assert!(sec.contains("sleeps 40 seconds"), "{sec}");
    assert!(!sec.contains("Observed"), "{sec}");
    let list = w1.run(&["doc", "read", "finding.codex.hooks", "--sections"]);
    assert!(list.contains("  #applies-to  Applies to  ~"), "{list}");
    let whole = w1.run(&["doc", "read", "d1"]);
    assert!(whole.contains("# Applies to\nCodex 0.50"), "{whole}");

    // Write one section with --if-rev, then a write from the old revision
    // is refused with the changes and the diff.
    let out = w1.run(&[
        "doc",
        "write",
        "d1",
        "--section",
        "evidence",
        "-m",
        "Ran it twice with a 40 second hook.",
        "--if-rev",
        "1",
        "--message",
        "second run",
    ]);
    assert!(out.contains("rev 2 · ~"), "{out}");
    let (stdout, err) = w1.fail(&[
        "doc",
        "write",
        "d1#evidence",
        "-m",
        "Ran it once.",
        "--if-rev",
        "1",
    ]);
    assert!(err.contains("stale"), "{err}");
    assert!(
        stdout.contains("d1 changed since your revision; it is now at rev 2."),
        "{stdout}"
    );
    assert!(
        stdout.contains("revised body rev 2 (second run)"),
        "{stdout}"
    );
    assert!(stdout.contains("Diff to rev 2:"), "{stdout}");
    assert!(stdout.contains("+Ran it twice"), "{stdout}");
    assert!(stdout.contains("--if-rev 2"), "{stdout}");
    // With --json, the whole 409 answer.
    let out = w1.try_run(&["--json", "doc", "write", "d1", "-m", "x", "--if-rev", "1"]);
    assert!(!out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "stale", "{v}");
    assert_eq!(v["rev"], 2, "{v}");
    assert!(v["diff"].as_str().unwrap().contains("@@"), "{v}");
    // The same text again writes nothing.
    let out = w1.run(&[
        "doc",
        "write",
        "d1",
        "--section",
        "evidence",
        "-m",
        "Ran it twice with a 40 second hook.",
    ]);
    assert!(out.contains("unchanged"), "{out}");
    assert_eq!(w1.rev("d1"), 2);
    // Another seat may not revise it.
    let (_, err) = w2.fail(&["doc", "write", "d1", "-m", "mine now"]);
    assert!(err.contains("not_owner"), "{err}");

    // Search finds active documents; the index shows them.
    let out = w1.run(&["doc", "search", "hook", "output"]);
    assert_eq!(out.trim(), "no hits", "{out}");
    let out = w1.run(&["doc", "search", "hook", "output", "--status", "all"]);
    assert!(out.contains("d1"), "{out}");
    let out = w1.run(&["doc", "set", "d1", "--status", "active"]);
    assert!(out.contains("finding   active"), "{out}");
    let out = w1.run(&["doc", "search", "40", "second", "hook"]);
    assert!(
        out.starts_with("d1#evidence  finding  active  Evidence — "),
        "{out}"
    );
    let out = w2.run(&["doc", "find", "codex"]);
    assert!(out.contains("d1 ") && out.contains("d2 "), "{out}");
    let out = w2.run(&["doc", "index"]);
    assert_eq!(
        out.trim(),
        "d1  finding  Codex drops slow hook output — Read before writing a Codex hook"
    );
    let out = w2.run(&["doc", "list", "--status", "draft"]);
    assert!(out.contains("d2 ") && !out.contains("d1 "), "{out}");

    // Verify with an interval.
    let out = w1.run(&["doc", "verify", "d1", "--for", "30d"]);
    assert!(out.starts_with("d1 verified by w1 at "), "{out}");
    assert!(out.contains("; expires "), "{out}");
    assert!(w1.json(&["doc", "show", "d1"])["doc"]["expires_at"].is_string());

    // Comment on a section: mail in the document's thread.
    let out = w2.run(&["doc", "comment", "d1#evidence", "-m", "Which Codex build?"]);
    assert!(out.starts_with("commented on d1: #"), "{out}");
    let show = w1.run(&["doc", "show", "d1"]);
    assert!(show.contains("Comments:"), "{show}");
    assert!(show.contains("by w2"), "{show}");
    assert!(show.contains("on #evidence (Evidence)"), "{show}");

    // History and diff.
    let out = w1.run(&["doc", "history", "d1"]);
    assert!(out.contains("Revisions:\n  rev 1 "), "{out}");
    assert!(out.contains("(second run)"), "{out}");
    assert!(out.contains("changed status: draft -> active"), "{out}");
    let out = w1.run(&["doc", "diff", "d1"]);
    assert!(out.starts_with("--- d1 rev 1"), "{out}");
    let out = w1.run(&["doc", "diff", "d1", "2", "2"]);
    assert!(out.contains("no differences"), "{out}");

    // Split prints the mapping; links to moved sections follow.
    let guide = w1.new_doc(&[
        "Deploy guide",
        "--kind",
        "reference",
        "-m",
        "Intro.\n\n# Build\nmake\n\n# Rollback\nSee d1#evidence.\n\n## Old\nold text\n",
    ]);
    let linker = w2.new_doc(&["Links", "-m", &format!("See d{guide}#rollback.\n")]);
    let out = w1.run(&["doc", "split", &format!("d{guide}")]);
    assert!(
        out.starts_with(&format!("d{guide} split into 2 documents:")),
        "{out}"
    );
    let build = guide + 2;
    let rollback = guide + 3;
    assert!(out.contains(&format!("  #build -> d{build}")), "{out}");
    assert!(
        out.contains(&format!("  #rollback -> d{rollback}")),
        "{out}"
    );
    let text = w2.run(&["doc", "read", &format!("d{linker}")]);
    assert!(text.contains(&format!("d{rollback}")), "{text}");
    let (_, err) = w1.fail(&["doc", "split", &format!("d{guide}")]);
    assert!(err.contains("nothing_to_split"), "{err}");

    // Extract one section into its own document, then merge it back.
    let out = w1.run(&[
        "doc",
        "extract",
        &format!("d{rollback}#old"),
        "--into",
        "Old rollback",
    ]);
    let old = rollback + 1;
    assert!(out.contains(&format!("  #old -> d{old}")), "{out}");
    let show = w1.run(&["doc", "show", &format!("d{old}")]);
    assert!(
        show.contains(&format!("Supplements: d{rollback}")),
        "{show}"
    );
    let out = w1.run(&[
        "doc",
        "merge",
        &format!("d{old}"),
        "--into",
        &format!("d{rollback}"),
    ]);
    assert!(out.starts_with(&format!("d{old} merged:")), "{out}");
    assert!(out.contains(&format!("d{old} -> d{rollback}#")), "{out}");

    // Backlinks and the check.
    let out = w1.run(&["doc", "backlinks", &format!("d{rollback}")]);
    assert!(out.contains(&format!("d{linker} -> d{rollback}")), "{out}");
    let out = w1.run(&["doc", "check"]);
    assert!(out.contains("-> d99  (dangling)"), "{out}");

    // Supersede: the second finding replaces nothing; d2 is superseded by
    // d1 (w2 owns d2).
    let out = w2.run(&["doc", "supersede", "d2", "--by", "d1"]);
    assert!(out.contains("superseded by d1"), "{out}");

    // Kinds: listed for seats, added by the user only.
    let out = w1.run(&["doc", "kinds"]);
    assert!(
        out.contains("decision   activate by human  in index  verify none"),
        "{out}"
    );
    assert!(
        out.contains("sections: Observed, Evidence, Applies to"),
        "{out}"
    );
    let (_, err) = w1.fail(&["doc", "kind", "add", "incident"]);
    assert!(err.contains("error"), "{err}");
    let out = owner.run(&[
        "doc",
        "kind",
        "add",
        "incident",
        "--sections",
        "Timeline,Cause",
        "--no-index",
        "--verify-days",
        "0",
        "--description",
        "what went wrong and why",
    ]);
    assert!(
        out.starts_with("incident   activate by owner  not in index  verify none"),
        "{out}"
    );
    assert!(out.contains("sections: Timeline, Cause"), "{out}");
}

const NOTE: &str = "State: on track, parser half done\n\
Summary: Porting the parser to the new AST.\n\
Next:\n1. cargo test -p parser\n2. port the error paths\n\
Waiting on: nothing\n\
Traps: do not touch src/legacy.rs\n\
Verify: cargo test -p parser prints 12 passed\n";

fn handover(owner: &Amail, w1: &Amail, w2: &Amail) {
    // The template needs no server.
    let out = Command::new(BIN)
        .args(["handover", "template"])
        .env_remove("AMAIL_TOKEN")
        .env("AMAIL_URL", "http://127.0.0.1:9")
        .env("HOME", std::env::temp_dir())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let t = String::from_utf8_lossy(&out.stdout);
    assert!(t.starts_with("State: "), "{t}");
    assert!(t.contains("\nPointers:\n"), "{t}");

    // No note yet.
    let out = w2.run(&["handover", "show", "w1"]);
    assert_eq!(out.trim(), "w1 has no handover note");

    // A note lacking fields is refused with the missing labels.
    let (stdout, err) = w1.fail(&["handover", "write", "-m", "State: on track\nSummary: x\n"]);
    assert!(err.contains("note_fields_missing"), "{err}");
    assert!(
        stdout.contains("The note lacks: Next, Waiting on, Traps, Verify."),
        "{stdout}"
    );
    assert!(stdout.contains("amail handover template"), "{stdout}");

    // Written; Pointers filled in by the server.
    let out = w1.run(&["handover", "write", "-m", NOTE, "--session", "s-1"]);
    assert!(
        out.starts_with("handover note of w1: rev 1 written ("),
        "{out}"
    );
    assert!(out.contains("Pointers filled in from the worklog"), "{out}");
    let show = w2.run(&["handover", "show", "w1"]);
    assert!(
        show.starts_with("Handover note of w1, rev 1 (agent), written "),
        "{show}"
    );
    assert!(show.contains(" in session s-1"), "{show}");
    assert!(show.contains("Read by: no session yet"), "{show}");
    assert!(show.contains("  > Summary: Porting the parser"), "{show}");
    assert!(show.contains("  > 1. cargo test -p parser"), "{show}");
    // Default seat: your own.
    let mine = w1.json(&["handover", "show"]);
    assert_eq!(mine["note"]["rev"], 1, "{mine}");

    // A private note: other seats are refused.
    let out = w1.run(&["handover", "write", "-m", NOTE, "--private"]);
    assert!(
        out.contains("rev 2 written") && out.contains("private"),
        "{out}"
    );
    let (_, err) = w2.fail(&["handover", "show", "w1"]);
    assert!(err.contains("private"), "{err}");
    let show = owner.run(&["handover", "show", "w1", "--rev", "1"]);
    assert!(show.contains("rev 1"), "{show}");
    assert!(show.contains("(the current one is rev 2)"), "{show}");
    assert!(show.contains("Revisions:\n  rev 2 "), "{show}");

    // A request by the user mails the seat.
    let out = owner.run(&["handover", "request", "w1"]);
    assert!(
        out.starts_with("asked w1 for its handover note at "),
        "{out}"
    );
    assert!(out.contains("(mail #"), "{out}");
    let show = w1.run(&["handover", "show"]);
    assert!(show.contains("Requested by owner at "), "{show}");

    // The worklog: entries newest first, filters, the summary.
    let out = w1.run(&["worklog", "w1"]);
    let first = out.lines().next().unwrap();
    assert!(first.contains("handover"), "{out}");
    // w2's comment was mail it sent.
    let out = w2.run(&["worklog"]);
    assert!(out.contains("mail_sent  m"), "{out}");
    let out = w1.run(&["worklog", "--kind", "handover", "--limit", "1"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(out.contains("(older: amail worklog w1 --before "), "{out}");
    let out = w2.run(&["worklog", "w1", "--summary"]);
    assert!(out.starts_with("worklog of w1: "), "{out}");
    assert!(out.contains("kinds: "), "{out}");
    assert!(out.contains("handover 2"), "{out}");
    assert!(out.contains("\nlast "), "{out}");
    let v = w2.json(&["worklog", "w1", "--since", "1h"]);
    assert!(!v["entries"].as_array().unwrap().is_empty(), "{v}");
}
