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
    "task_dep",
    "task_undep",
    "task_link",
    "task_unlink",
    "task_comment",
    "task_history",
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
    let (t1, t2) = (w1.token.clone(), w2.token.clone());
    tokio::task::spawn_blocking(move || drive(&url, &t1, &t2))
        .await
        .unwrap();
}

fn drive(url: &str, t1: &str, t2: &str) {
    // w1 as a person registers it by hand; w2 as the node registers it.
    let mut m1 = McpProc::start(&[], &[("AMAIL_URL", url), ("AMAIL_TOKEN", t1)]);
    let mut m2 = McpProc::start(&["--node"], &[("AMAIL_URL", url), ("AMAIL_TOKEN", t2)]);

    let init = m1.initialize();
    assert_eq!(init["result"]["serverInfo"]["name"], "amail", "{init}");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
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
