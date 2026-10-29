//! `amail mcp`: the real binary driven over stdio against an in-process
//! server, on SQLite and (with `AMAIL_TEST_PG_URL`) Postgres.

mod common;

use amail_store::testutil::Backend;
use common::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_sqlite() {
    deadline("mcp_sqlite", run(Backend::Sqlite)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_postgres() {
    deadline("mcp_postgres", run(Backend::Postgres)).await;
}

const TOOLS: &[&str] = &[
    "inbox",
    "read",
    "send",
    "reply",
    "ack",
    "thread",
    "search",
    "who",
    "status",
    "focus",
    "task_new",
    "task_show",
    "task_list",
    "task_mine",
    "task_next",
    "task_waiting",
    "task_owed",
    "task_board",
    "task_set",
    "task_take",
    "task_steal",
    "task_yield",
    "task_keep",
    "task_dep",
    "task_undep",
    "task_link",
    "task_unlink",
    "task_comment",
    "task_history",
    "task_result",
    "task_report",
    "doc_new",
    "doc_show",
    "doc_read",
    "doc_write",
    "doc_set",
    "doc_search",
    "doc_find",
    "doc_list",
    "doc_index",
    "doc_history",
    "doc_split",
    "doc_extract",
    "doc_verify",
    "doc_comment",
    "handover_show",
    "handover_write",
    "worklog",
    "ask",
    "requests",
    "answer",
    "prompt_seat",
    "interrupt_seat",
];

/// `amail --json <args>` as the seat with `token`.
fn cli_json(url: &str, token: &str, args: &[&str]) -> Value {
    let out = std::process::Command::new(BIN)
        .arg("--json")
        .args(args)
        .env("AMAIL_URL", url)
        .env("AMAIL_TOKEN", token)
        .env_remove("AMAIL_PROFILE")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

fn ok(v: &Value) -> &Value {
    assert_eq!(v["isError"], false, "{v}");
    assert!(v["content"][0]["text"].is_string(), "{v}");
    &v["structuredContent"]
}

fn err_code(v: &Value) -> String {
    assert_eq!(v["isError"], true, "{v}");
    v["structuredContent"]["error"]["code"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn run(b: Backend) {
    let Some(h) = Harness::start(b, "mcp_tools", Opts::default()).await else {
        return;
    };
    let w1 = h.seat("w1", "claude").await;
    let w2 = h.seat("w2", "claude").await;
    let url = h.api.base.clone();
    let (t1, t2, owner) = (w1.token.clone(), w2.token.clone(), h.api.token.clone());
    tokio::task::spawn_blocking(move || drive(&url, &t1, &t2, &owner))
        .await
        .unwrap();
}

fn drive(url: &str, t1: &str, t2: &str, owner: &str) {
    // w1 as a person registers it by hand; w2 as the node registers it.
    let mut m1 = McpProc::start(&[], &[("AMAIL_URL", url), ("AMAIL_TOKEN", t1)]);
    let mut m2 = McpProc::start(&["--node"], &[("AMAIL_URL", url), ("AMAIL_TOKEN", t2)]);

    let init = m1.initialize();
    assert_eq!(init["result"]["serverInfo"]["name"], "amail", "{init}");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    // The instructions state the rule for findings concretely.
    let instructions = init["result"]["instructions"].as_str().unwrap();
    for part in [
        "A fact you verified about the code, a tool or the environment is a `finding` document",
        "`doc_comment` is for correcting or discussing an existing document",
        "When you need the person's decision, use `ask` with the options and a default, then stop; \
         the answer comes as mail and wakes you.",
    ] {
        assert!(instructions.contains(part), "{instructions}");
    }
    m2.initialize();

    // Every tool is listed, with an object schema whose required fields
    // are among its properties.
    let list = m1.request("tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: BTreeSet<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOLS.iter().copied().collect(), "{list}");
    for t in tools {
        assert!(t["description"].as_str().unwrap().len() > 40, "{t}");
        let s = &t["inputSchema"];
        assert_eq!(s["type"], "object", "{t}");
        let props = s["properties"].as_object().unwrap();
        for r in s["required"].as_array().unwrap() {
            assert!(props.contains_key(r.as_str().unwrap()), "{t}");
        }
    }

    // Mail: send, inbox, read, ack.
    let sent = m1.call(
        "send",
        json!({"to": ["w2"], "subject": "hello", "body": "hi there", "ack_required": true}),
    );
    let id = ok(&sent)["id"].as_i64().unwrap();
    let inbox = m2.call("inbox", json!({}));
    let entries = ok(&inbox)["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(|e| e["message"]["id"] == json!(id)),
        "{inbox}"
    );
    assert!(
        inbox["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("hello"),
        "{inbox}"
    );
    let read = m2.call("read", json!({"id": id}));
    assert_eq!(ok(&read)["message"]["body"], "hi there", "{read}");
    let ack = m2.call("ack", json!({"id": id}));
    ok(&ack);
    // The same answer as the command with --json.
    let all = m2.call("inbox", json!({"all": true}));
    assert_eq!(
        ok(&all),
        &cli_json(url, t2, &["inbox", "--all"]),
        "inbox all"
    );
    let reply = m2.call("reply", json!({"id": id, "body": "got it"}));
    ok(&reply);
    let thread = m1.call("thread", json!({"id": id}));
    assert_eq!(
        ok(&thread)["messages"].as_array().unwrap().len(),
        2,
        "{thread}"
    );
    let status = m2.call("status", json!({}));
    assert_eq!(ok(&status)["whoami"]["seat"], "w2", "{status}");
    let who = m1.call("who", json!({"text": "w2"}));
    assert_eq!(ok(&who)["seats"].as_array().unwrap().len(), 1, "{who}");

    // Tasks: new, mine, set, comment.
    let new = m1.call(
        "task_new",
        json!({"title": "write docs", "assignee": "w2", "priority": "high"}),
    );
    let num = ok(&new)["task"]["num"].as_i64().unwrap();
    let mine = m2.call("task_mine", json!({}));
    assert!(
        ok(&mine)["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["num"] == json!(num)),
        "{mine}"
    );
    let set = m2.call(
        "task_set",
        json!({"num": format!("t{num}"), "state": "doing"}),
    );
    assert_eq!(ok(&set)["task"]["state"], "doing", "{set}");
    let comment = m2.call("task_comment", json!({"num": num, "body": "started"}));
    assert!(ok(&comment)["id"].is_i64(), "{comment}");
    let show = m1.call("task_show", json!({"num": num}));
    // (`amail task` is not on this branch yet: compared with the API.)
    let detail = ok(&show);
    assert_eq!(detail["task"]["num"], json!(num), "{show}");
    assert_eq!(detail["task"]["assignee"], "w2", "{show}");
    assert_eq!(detail["events"][0]["kind"], "created", "{show}");

    // Work stealing: w1 asks for a task of w2's that is not started; w2
    // keeps it, then yields after a second request.
    let other = m1.call("task_new", json!({"title": "port tests", "assignee": "w2"}));
    let num2 = ok(&other)["task"]["num"].as_i64().unwrap();
    let steal = m1.call("task_steal", json!({"num": num2}));
    assert_eq!(ok(&steal)["task"]["steal_requested_by"], "w1", "{steal}");
    assert!(
        steal["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("takeover requested by w1"),
        "{steal}"
    );
    // Only the assignee answers.
    let wrong = m1.call("task_keep", json!({"num": num2}));
    assert_eq!(
        wrong["structuredContent"]["error"]["status"], 403,
        "{wrong}"
    );
    let keep = m2.call("task_keep", json!({"num": num2}));
    assert!(
        ok(&keep)["task"].get("steal_requested_by").is_none(),
        "{keep}"
    );
    assert_eq!(ok(&keep)["task"]["assignee"], "w2", "{keep}");
    ok(&m1.call("task_steal", json!({"num": format!("t{num2}")})));
    let yielded = m2.call("task_yield", json!({"num": num2}));
    assert_eq!(ok(&yielded)["task"]["assignee"], "w1", "{yielded}");
    // A started task cannot be asked for.
    let started = m1.call("task_steal", json!({"num": num}));
    assert_eq!(err_code(&started), "started", "{started}");

    // M8.1: the same task_new twice creates one task.
    let a = m1.call(
        "task_new",
        json!({"title": "review the parser", "assignee": "w2"}),
    );
    let review = ok(&a)["task"]["num"].as_i64().unwrap();
    let b = m1.call(
        "task_new",
        json!({"title": "review the parser", "assignee": "w2"}),
    );
    assert_eq!(ok(&b)["task"]["num"], json!(review), "{b}");
    assert_eq!(ok(&b)["existing"], true, "{b}");
    let c = m1.call(
        "task_new",
        json!({"title": "review the parser", "assignee": "w2", "client_key": "another"}),
    );
    assert_ne!(ok(&c)["task"]["num"], json!(review), "{c}");
    // if_rev: an old revision is refused with the task and the changes.
    let rev = ok(&b)["task"]["rev"].as_i64().unwrap();
    ok(&m2.call(
        "task_set",
        json!({"num": review, "title": "review the parser now", "if_rev": rev}),
    ));
    let stale = m1.call(
        "task_set",
        json!({"num": review, "body": "x", "if_rev": rev}),
    );
    assert_eq!(err_code(&stale), "stale", "{stale}");
    assert_eq!(
        stale["structuredContent"]["task"]["task"]["title"],
        "review the parser now"
    );
    assert_eq!(stale["structuredContent"]["since"][0]["field"], "title");
    assert!(
        stale["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("changes since"),
        "{stale}"
    );
    // A dependency that needs an ok result, results, and the report.
    let integ = m1.call("task_new", json!({"title": "integrate", "assignee": "w1"}));
    let integ = ok(&integ)["task"]["num"].as_i64().unwrap();
    let dep = m1.call(
        "task_dep",
        json!({"num": integ, "blocked_by": review, "needs_ok": true}),
    );
    assert_eq!(ok(&dep)["blockers"][0]["needs_ok"], true, "{dep}");
    assert!(
        dep["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("(ok needed)"),
        "{dep}"
    );
    let rejected = m2.call(
        "task_set",
        json!({"num": review, "state": "done", "result": "rejected", "result_ref": "abc123"}),
    );
    assert_eq!(ok(&rejected)["task"]["result"], "rejected", "{rejected}");
    assert_eq!(
        err_code(&m2.call("task_result", json!({"num": integ, "result": "ok"}))),
        "not_done"
    );
    let okr = m2.call(
        "task_result",
        json!({"num": review, "result": "ok", "ref": "def456", "note": "fixed"}),
    );
    assert_eq!(ok(&okr)["task"]["result_ref"], "def456", "{okr}");
    assert!(
        okr["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("result: ok, ref def456"),
        "{okr}"
    );
    let show = m1.call("task_show", json!({"num": integ}));
    assert_eq!(ok(&show)["task"]["state"], "ready", "{show}");
    let report = m1.call("task_report", json!({}));
    assert_eq!(
        ok(&report),
        &cli_json(url, t1, &["task", "report"]),
        "report"
    );
    assert!(
        report["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("approved, not taken further (1):"),
        "{report}"
    );
    let wd = m2.call(
        "task_result",
        json!({"num": review, "result": "withdraw", "note": "wrong commit"}),
    );
    assert!(ok(&wd)["task"].get("result").is_none(), "{wd}");
    let list = m1.call("task_list", json!({"state": "all", "result": "none"}));
    assert!(
        ok(&list)["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["num"] == json!(review)),
        "{list}"
    );

    documents(url, t1, &mut m1, &mut m2);
    requests(url, t1, owner, &mut m1);

    // Errors: an unknown task is the server's error as a tool error; bad
    // arguments too.
    let missing = m1.call("task_show", json!({"num": 99999}));
    assert_eq!(
        missing["structuredContent"]["error"]["status"], 404,
        "{missing}"
    );
    err_code(&missing);
    assert_eq!(err_code(&m1.call("read", json!({}))), "invalid_arguments");
    assert_eq!(
        err_code(&m1.call("no_such_tool", json!({}))),
        "unknown_tool"
    );

    // Malformed requests: no crash; a JSON-RPC error where there is an id
    // to answer.
    m1.write_line("this is not JSON");
    m1.write_line(r#"{"jsonrpc":"2.0","id":900,"method":"tools/call","params":{}}"#);
    assert!(m1.answer(900)["error"]["code"].is_i64());
    m1.write_line(r#"{"jsonrpc":"2.0","id":901,"method":"no/such/method"}"#);
    assert_eq!(m1.answer(901)["error"]["code"], -32601);
    let ping = m1.request("ping", json!({}));
    assert!(ping["result"].is_object(), "{ping}");

    // No identity: every call answers so, the server keeps running.
    let mut none = McpProc::start(&["--node"], &[("AMAIL_URL", url)]);
    none.initialize();
    assert_eq!(err_code(&none.call("inbox", json!({}))), "no_identity");
    let mut none = McpProc::start(&[], &[("AMAIL_URL", url)]);
    none.initialize();
    assert_eq!(err_code(&none.call("inbox", json!({}))), "no_identity");
    assert!(none.request("ping", json!({}))["result"].is_object());
}

fn text(v: &Value) -> &str {
    v["content"][0]["text"].as_str().unwrap()
}

/// The request tools: `m1` (w1) asks; the user's MCP server answers and
/// uses the seat controls; w1 may not.
fn requests(url: &str, t1: &str, owner: &str, m1: &mut McpProc) {
    let asked = m1.call(
        "ask",
        json!({
            "title": "Ship the parser?",
            "body": "The tests pass.",
            "options": [{"key": "yes", "label": "Ship it"}, "no:Hold it:until the docs are done"],
            "default": "no",
            "deadline": "2h",
            "class": "now",
        }),
    );
    let r = ok(&asked)["request"].clone();
    let num = r["num"].as_i64().unwrap();
    assert_eq!(r["source"], "mcp", "{asked}");
    assert_eq!(r["seat"], "w1", "{asked}");
    assert_eq!(r["class"], "now", "{asked}");
    assert_eq!(
        r["options"][1],
        json!({"key": "no", "label": "Hold it", "description": "until the docs are done"}),
        "{asked}"
    );
    assert!(
        text(&asked).starts_with(&format!(
            "r{num}  question  now  Ship the parser?  (deadline "
        )),
        "{asked}"
    );
    assert!(
        text(&asked).contains("the person's answer comes as mail"),
        "{asked}"
    );
    // Also in the structured content, which Claude Code shows the model
    // instead of the text.
    let next = ok(&asked)["next"].as_str().unwrap();
    assert!(
        next.starts_with("Stop your turn here") && next.contains(&format!("[r{num}]")),
        "{asked}"
    );
    // Bad arguments are the tool's own errors.
    assert_eq!(
        err_code(&m1.call("ask", json!({"title": "x", "options": ["nolabel"]}))),
        "invalid_arguments"
    );
    assert_eq!(
        err_code(&m1.call("ask", json!({"title": "x", "deadline": "soon"}))),
        "invalid_arguments"
    );
    // The server's format rules.
    let one = m1.call("ask", json!({"title": "x", "options": ["a:A"]}));
    assert_eq!(err_code(&one), "options_count", "{one}");

    // The caller's open requests by default; the same JSON as the command.
    let list = m1.call("requests", json!({}));
    assert_eq!(ok(&list), &cli_json(url, t1, &["requests"]), "requests");
    assert_eq!(ok(&list)["requests"][0]["num"], json!(num), "{list}");
    assert!(text(&list).starts_with(&format!("r{num}  ")), "{list}");
    let one = m1.call("requests", json!({"num": format!("r{num}")}));
    assert!(text(&one).contains("  2. no — Hold it  (default)"), "{one}");
    assert_eq!(
        ok(&one),
        &cli_json(url, t1, &["request", "show", &format!("r{num}")]),
        "request show"
    );

    // Only the user's seat answers and controls seats.
    let refused = m1.call("answer", json!({"num": num, "option": 1}));
    assert_eq!(err_code(&refused), "forbidden", "{refused}");
    assert_eq!(refused["structuredContent"]["error"]["status"], 403);
    let refused = m1.call("prompt_seat", json!({"seat": "w2", "text": "hi"}));
    assert_eq!(refused["structuredContent"]["error"]["status"], 403);
    let refused = m1.call("interrupt_seat", json!({"seat": "w2"}));
    assert_eq!(refused["structuredContent"]["error"]["status"], 403);
    assert_eq!(
        err_code(&m1.call("answer", json!({"num": num}))),
        "invalid_arguments"
    );

    let mut user = McpProc::start(&[], &[("AMAIL_URL", url), ("AMAIL_TOKEN", owner)]);
    user.initialize();
    let stale = user.call("answer", json!({"num": num, "option": 1, "if_rev": 999}));
    assert_eq!(err_code(&stale), "stale", "{stale}");
    assert_eq!(
        stale["structuredContent"]["request"]["state"], "open",
        "{stale}"
    );
    let answered = user.call(
        "answer",
        json!({"num": format!("r{num}"), "option": 1, "note": "go ahead"}),
    );
    assert_eq!(ok(&answered)["request"]["state"], "answered", "{answered}");
    assert_eq!(
        text(&answered),
        format!("r{num} answered by owner: 1. yes — Ship it\nNote: go ahead")
    );
    let again = user.call("answer", json!({"num": num, "text": "no"}));
    assert_eq!(err_code(&again), "answered", "{again}");
    assert_eq!(
        again["structuredContent"]["request"]["answer"]["option"], "yes",
        "{again}"
    );
    // Answered requests leave the open list.
    let list = m1.call("requests", json!({}));
    assert_eq!(text(&list), "no open requests", "{list}");
    let all = m1.call("requests", json!({"state": "all"}));
    assert_eq!(ok(&all)["requests"].as_array().unwrap().len(), 1, "{all}");

    // The seat controls: the seats of this test have no session, so no
    // connected node reports them; the 409 is said plainly.
    let p = user.call("prompt_seat", json!({"seat": "w1", "text": "status?"}));
    assert_eq!(err_code(&p), "no_node", "{p}");
    assert!(
        text(&p).contains("w1: no node is connected for this seat"),
        "{p}"
    );
    let i = user.call("interrupt_seat", json!({"seat": "w1"}));
    assert_eq!(i["structuredContent"]["error"]["status"], 409, "{i}");
}

/// The document, handover and worklog tools. `m1` is w1 (token `t1`), `m2`
/// is w2.
fn documents(url: &str, t1: &str, m1: &mut McpProc, m2: &mut McpProc) {
    let body = "# Observed\nThe build cache is cleared on every merge.\n\n\
                # Evidence\nRan the pipeline twice; the cache step took 12 minutes both times.\n\n\
                # Applies to\nThe CI of this repository.\n";
    let args = json!({
        "title": "CI clears the build cache", "kind": "finding", "body": body,
        "description": "Why CI is slow after merges",
    });
    let new = m1.call("doc_new", args.clone());
    let num = ok(&new)["doc"]["num"].as_i64().unwrap();
    assert_eq!(ok(&new)["doc"]["status"], "draft", "{new}");
    assert!(text(&new).starts_with(&format!("d{num} ")), "{new}");
    let rev = ok(&new)["doc"]["rev"].as_i64().unwrap();
    // The same call again: the same document, nothing created.
    let again = m1.call("doc_new", args);
    assert_eq!(ok(&again)["doc"]["num"], json!(num), "{again}");
    assert_eq!(ok(&again)["existing"], true, "{again}");
    // Another seat on the same subject: created, with the candidates.
    let other = m2.call(
        "doc_new",
        json!({"title": "CI clears the build cache", "kind": "finding", "body": body}),
    );
    let other_num = ok(&other)["doc"]["num"].as_i64().unwrap();
    assert_eq!(
        ok(&other)["candidates"][0]["doc"]["num"],
        json!(num),
        "{other}"
    );
    assert!(
        text(&other).contains(&format!("warning: similar documents exist: d{num} ")),
        "{other}"
    );

    // Read one section; the same JSON as the command.
    let r = m2.call("doc_read", json!({"doc": format!("d{num}#evidence")}));
    assert_eq!(ok(&r)["read"]["anchor"], "evidence", "{r}");
    assert!(text(&r).contains("12 minutes"), "{r}");
    assert!(!text(&r).contains("Observed"), "{r}");
    assert_eq!(
        ok(&r),
        &cli_json(url, t1, &["doc", "read", &format!("d{num}#evidence")]),
        "doc_read"
    );
    let r = m2.call("doc_read", json!({"doc": num, "sections": true}));
    assert!(text(&r).contains("#applies-to  Applies to"), "{r}");
    // A whole document above 20,000 characters: the section list.
    let long = format!(
        "# Part one\n{}\n\n# Part two\n{}\n",
        "alpha ".repeat(2000),
        "beta ".repeat(2000)
    );
    let ln = m1.call("doc_new", json!({"title": "Long reference", "body": long}));
    let lnum = ok(&ln)["doc"]["num"].as_i64().unwrap();
    let r = m1.call("doc_read", json!({"doc": lnum}));
    assert!(text(&r).contains("#part-one  Part one"), "{r}");
    assert!(text(&r).contains("or pass whole: true"), "{r}");
    assert!(ok(&r)["whole_chars"].as_i64().unwrap() > 20_000, "{r}");
    assert!(ok(&r)["doc"].get("body").is_none(), "{r}");
    let r = m1.call("doc_read", json!({"doc": lnum, "whole": true}));
    assert!(text(&r).contains("beta beta"), "{r}");
    // doc_show: the record without the body (its size instead).
    let s = m1.call("doc_show", json!({"doc": num}));
    assert!(ok(&s)["doc"].get("body").is_none(), "{s}");
    assert!(ok(&s)["doc"]["body_chars"].as_i64().unwrap() > 50, "{s}");
    assert!(text(&s).contains("Kind: finding · Status: draft"), "{s}");

    // doc_write with if_rev, then a stale write with the changes and diff.
    let w = m1.call(
        "doc_write",
        json!({"doc": format!("d{num}#evidence"), "body": "Ran the pipeline three times.",
               "if_rev": rev, "message": "third run"}),
    );
    assert_eq!(ok(&w)["doc"]["rev"], json!(rev + 1), "{w}");
    let stale = m1.call("doc_write", json!({"doc": num, "body": "x", "if_rev": rev}));
    assert_eq!(err_code(&stale), "stale", "{stale}");
    let sc = &stale["structuredContent"];
    assert_eq!(sc["rev"], json!(rev + 1), "{stale}");
    assert!(
        sc["diff"].as_str().unwrap().contains("three times"),
        "{stale}"
    );
    assert_eq!(sc["since"][0]["note"], "third run", "{stale}");
    assert!(
        text(&stale).contains(&format!("call again with if_rev {}", rev + 1)),
        "{stale}"
    );

    // Search: active documents; activate, then it is found and indexed.
    let none = m2.call("doc_search", json!({"terms": "pipeline three"}));
    assert_eq!(text(&none), "no hits", "{none}");
    let set = m1.call("doc_set", json!({"doc": num, "status": "active"}));
    assert_eq!(ok(&set)["doc"]["status"], "active", "{set}");
    let hits = m2.call("doc_search", json!({"terms": "pipeline three"}));
    assert_eq!(ok(&hits)["hits"][0]["num"], json!(num), "{hits}");
    assert!(
        text(&hits).starts_with(&format!("d{num}#evidence  finding  active  Evidence — ")),
        "{hits}"
    );
    let idx = m2.call("doc_index", json!({}));
    assert!(
        text(&idx).contains(&format!("d{num}  finding  CI clears the build cache")),
        "{idx}"
    );
    let found = m2.call("doc_find", json!({"words": "build cache"}));
    assert_eq!(ok(&found)["docs"].as_array().unwrap().len(), 2, "{found}");
    let list = m2.call("doc_list", json!({"kind": "finding", "status": "all"}));
    assert_eq!(ok(&list)["docs"].as_array().unwrap().len(), 2, "{list}");
    // w2 supersedes its own duplicate by w1's document.
    let sup = m2.call(
        "doc_set",
        json!({"doc": other_num, "superseded_by": format!("d{num}")}),
    );
    assert_eq!(ok(&sup)["doc"]["status"], "superseded", "{sup}");
    assert!(
        text(&sup).contains(&format!("superseded by d{num}")),
        "{sup}"
    );
    assert_eq!(
        err_code(&m2.call(
            "doc_set",
            json!({"doc": other_num, "superseded_by": num, "title": "x"})
        )),
        "invalid_arguments"
    );
    let c = m2.call(
        "doc_comment",
        json!({"doc": format!("d{num}#evidence"), "body": "Which runner?"}),
    );
    assert!(ok(&c)["id"].is_i64(), "{c}");
    let v = m1.call("doc_verify", json!({"doc": num, "for": "2w"}));
    assert!(ok(&v)["doc"]["expires_at"].is_string(), "{v}");
    let sp = m1.call("doc_split", json!({"doc": lnum}));
    assert!(text(&sp).contains("#part-one -> d"), "{sp}");
    assert_eq!(ok(&sp)["moved"].as_array().unwrap().len(), 2, "{sp}");
    let h = m1.call("doc_history", json!({"doc": num}));
    assert_eq!(ok(&h)["revisions"].as_array().unwrap().len(), 2, "{h}");
    assert!(text(&h).contains("(third run)"), "{h}");

    // The handover note: missing fields, then written as fields.
    let missing = m1.call(
        "handover_write",
        json!({"text": "State: on track\nSummary: x\n"}),
    );
    assert_eq!(err_code(&missing), "note_fields_missing", "{missing}");
    assert_eq!(
        missing["structuredContent"]["missing"],
        json!(["Next", "Waiting on", "Traps", "Verify"]),
        "{missing}"
    );
    assert!(
        text(&missing).contains("The note lacks: Next, Waiting on, Traps, Verify."),
        "{missing}"
    );
    assert!(text(&missing).contains("\nPointers:\n"), "{missing}");
    let w = m1.call(
        "handover_write",
        json!({"fields": {"state": "on track", "summary": "CI finding recorded.",
                          "next": format!("1. amail doc show d{num}"), "waiting_on": "nothing",
                          "traps": "none", "verify": "doc_search finds it"},
               "session": "sess-1"}),
    );
    assert_eq!(ok(&w)["note"]["rev"], 1, "{w}");
    assert_eq!(ok(&w)["note"]["pointers_filled"], true, "{w}");
    let show = m2.call("handover_show", json!({"seat": "w1"}));
    assert!(
        text(&show).contains("Summary: CI finding recorded."),
        "{show}"
    );
    assert_eq!(ok(&show)["note"]["session"], "sess-1", "{show}");

    // The worklog: entries, and the summary as the command answers it.
    let wl = m1.call("worklog", json!({"kind": "handover"}));
    assert_eq!(ok(&wl)["entries"].as_array().unwrap().len(), 1, "{wl}");
    assert!(text(&wl).contains("handover note rev 1 written"), "{wl}");
    let sum = m1.call("worklog", json!({"summary": true}));
    assert!(text(&sum).starts_with("worklog of w1: "), "{sum}");
    assert_eq!(
        ok(&sum),
        &cli_json(url, t1, &["worklog", "--summary"]),
        "worklog summary"
    );
}
