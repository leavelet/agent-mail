//! Several `amail serve` processes on one database: two real server
//! instances on one Postgres database (skipped without
//! `AMAIL_TEST_PG_URL`), and a second instance refused on SQLite.

mod procs;

use amail_store::testutil::Backend;
use procs::*;
use reqwest::Method;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_instances_share_state() {
    let Some(db) = Db::new(Backend::Postgres, "two_instances_share_state").await else {
        return;
    };
    let user_token = db.create_org("acme").await;
    let a = ServerProc::start(&db.url, None).await;
    let b = ServerProc::start(&db.url, None).await;
    let ua = Api::new(&a.base, &user_token);
    let ub = Api::new(&b.base, &user_token);
    for s in ["w1", "w2", "lead"] {
        ua.ok(Method::POST, "/seats", Some(json!({ "name": s })))
            .await;
    }
    let node_token = ua
        .ok(Method::POST, "/tokens", Some(json!({ "scope": "node" })))
        .await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let na = Api::new(&a.base, &node_token);
    let nb = Api::new(&b.base, &node_token);

    // Mailbox events: mail sent through A reaches a stream on B.
    let mut events_b = Sse::open(&ub, "/events").await;
    let sent = ua
        .ok(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["w1"], "subject": "hi", "body": "" })),
        )
        .await;
    let ev = events_b
        .next_of("mailbox", |d| d["seat"] == "w1")
        .await
        .expect("mailbox event on the other instance");
    assert!(ev["seq"].as_i64().unwrap() >= 1);
    assert!(sent["id"].is_i64());

    // Presence reported to A is visible on B, with an event there.
    na.ok(
        Method::POST,
        "/presence",
        Some(json!({ "node": "host1", "seats": [{ "seat": "w1", "state": "idle", "harness": "claude" }] })),
    )
    .await;
    let ev = events_b
        .next_of("presence", |d| d["seat"] == "w1")
        .await
        .expect("presence event on the other instance");
    assert_eq!(ev["state"], "idle");
    let seat = ub.ok(Method::GET, "/seats/w1", None).await;
    assert_eq!(seat["presence"]["state"], "idle");
    assert_eq!(seat["presence"]["node"], "host1");

    // A node stream on A: B knows a node is connected and forwards commands.
    let mut node_a = Sse::open(&na, "/events").await;
    wait_until("B sees the node on A", || async {
        ub.ok(Method::GET, "/seats/w1", None).await["presence"]["node_connected"] == true
    })
    .await;
    let r = ub.ok(Method::POST, "/seats/w1/wake", None).await;
    assert_eq!(r["nodes"], 1);
    let cmd = node_a
        .next_of("command", |d| d["op"] == "wake")
        .await
        .expect("command forwarded to the node's instance");
    assert_eq!(cmd["seat"], "w1");

    // Conversation mirror: browser on A, node stream and posts on B.
    let mut node_b = Sse::open(&nb, "/events").await;
    let mut watch = Sse::open(&ua, "/seats/w1/transcript").await;
    node_b
        .next_of("command", |d| d["op"] == "transcript" && d["seat"] == "w1")
        .await
        .expect("lease command reaches the node on the other instance");
    let mut subscribers = 0;
    for _ in 0..50 {
        let r = nb
            .ok(
                Method::POST,
                "/seats/w1/transcript",
                Some(json!({ "node": "host1", "reset": false,
                             "events": [{ "role": "assistant", "kind": "text", "text": "hello from B" }] })),
            )
            .await;
        subscribers = r["subscribers"].as_u64().unwrap();
        if subscribers > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        subscribers, 1,
        "the watching instance answers through the relay"
    );
    let ev = watch
        .next_of("events", |d| d["events"][0]["text"] == "hello from B")
        .await
        .expect("transcript events relayed to the watching instance");
    assert_eq!(ev["events"][0]["role"], "assistant");

    // The send budget is one budget across both instances.
    ua.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "send_rate_per_min": 1, "send_burst": 4 })),
    )
    .await;
    let w2 = ua.ok(Method::POST, "/seats/w2/tokens", None).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (w2a, w2b) = (Api::new(&a.base, &w2), Api::new(&b.base, &w2));
    let mut ok = 0;
    for i in 0..10 {
        let api = if i % 2 == 0 { &w2a } else { &w2b };
        let (s, _) = api
            .call(
                Method::POST,
                "/messages",
                Some(json!({ "to": ["lead"], "subject": "x", "body": "" })),
            )
            .await;
        if s.is_success() {
            ok += 1;
        } else {
            assert_eq!(s, reqwest::StatusCode::TOO_MANY_REQUESTS);
        }
    }
    assert_eq!(ok, 4);

    // One instance dies: the other keeps serving.
    drop(node_a);
    drop(node_b);
    drop(watch);
    let mut a = a;
    a.kill();
    let v = ub
        .ok(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["w1"], "subject": "still here", "body": "" })),
        )
        .await;
    assert!(v["id"].is_i64());
    let mut node_b2 = Sse::open(&nb, "/events").await;
    // (A's streams count until its heartbeat is 30 s old.)
    let r = ub.ok(Method::POST, "/seats/w1/sleep", None).await;
    assert!(r["nodes"].as_u64().unwrap() >= 1);
    node_b2
        .next_of("command", |d| d["op"] == "sleep")
        .await
        .expect("command on the surviving instance");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_refuses_a_second_instance() {
    let Some(db) = Db::new(Backend::Sqlite, "sqlite_refuses_a_second_instance").await else {
        return;
    };
    let _a = ServerProc::start(&db.url, None).await;
    let out = std::process::Command::new(BIN)
        .args(["serve", "--db", &db.url, "--listen", "127.0.0.1:0"])
        .env("RUST_LOG", "warn")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("another amail serve is using"), "{err}");
}
