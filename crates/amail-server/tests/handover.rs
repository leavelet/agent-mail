//! Seat handover over HTTP (M9): the note (fields, cap, revisions,
//! private notes, requests, the read acknowledgement, the server's idle
//! note) and the worklog (node entries, server entries from mail, tasks
//! and focus, queries, who reads it, the public-only limits). Both
//! backends (Postgres only when `AMAIL_TEST_PG_URL` is set).

mod common;

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

macro_rules! handover_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    if let Some(srv) = super::Server::start(amail_store::testutil::Backend::Sqlite, stringify!($name), Config::default()).await {
                        super::common::deadline(stringify!($name), super::$name(srv)).await;
                    }
                }
            )*
            use amail_server::config::Config;
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    if let Some(srv) = super::Server::start(amail_store::testutil::Backend::Postgres, stringify!($name), Config::default()).await {
                        super::common::deadline(stringify!($name), super::$name(srv)).await;
                    }
                }
            )*
            use amail_server::config::Config;
        }
    };
}

handover_tests!(
    note_fields_cap_and_revisions,
    private_notes,
    requests_mail_the_seat,
    read_acknowledgement,
    idle_note_when_the_last_task_closes,
    server_entries,
    node_entries_and_queries,
    public_limits,
);

struct Server {
    base: String,
    #[allow(dead_code)]
    db: TestDb,
    http: reqwest::Client,
    backend: Backend,
}

#[derive(Clone)]
struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(backend: Backend, test: &str, cfg: Config) -> Option<Server> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut state = AppState::new(db.store.clone());
        state.set_config(cfg);
        tokio::spawn(amail_server::serve(listener, state, std::future::pending()));
        Some(Server {
            base,
            db,
            backend,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
        })
    }

    async fn org(&self, name: &str) -> Client {
        let token = new_token();
        self.db
            .store
            .create_org(name, "owner", &hash_token(&token))
            .await
            .unwrap();
        Client {
            base: self.base.clone(),
            token,
            http: self.http.clone(),
        }
    }
}

impl Client {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = self
            .http
            .request(method, format!("{}/v1{}", self.base, path))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap();
        let v = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::String(text))
        };
        (status, v)
    }
    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method.clone(), path, body).await;
        assert!(s.is_success(), "{method} {path} -> {s}: {v}");
        v
    }
    async fn get(&self, path: &str) -> Value {
        self.ok(Method::GET, path, None).await
    }
    async fn post(&self, path: &str, body: Value) -> Value {
        self.ok(Method::POST, path, Some(body)).await
    }
    async fn put(&self, path: &str, body: Value) -> Value {
        self.ok(Method::PUT, path, Some(body)).await
    }
    /// Status and error code of a refused call.
    async fn refused(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (s, v) = self.call(method, path, body).await;
        assert!(!s.is_success(), "{path} should be refused: {v}");
        (s, v)
    }
    async fn seat(&self, name: &str) -> Client {
        self.ok(Method::POST, "/seats", Some(json!({ "name": name })))
            .await;
        let v = self
            .ok(Method::POST, &format!("/seats/{name}/tokens"), None)
            .await;
        Client {
            token: v["token"].as_str().unwrap().to_string(),
            ..self.clone()
        }
    }
    async fn node(&self) -> Client {
        let v = self
            .ok(Method::POST, "/tokens", Some(json!({ "scope": "node" })))
            .await;
        Client {
            token: v["token"].as_str().unwrap().to_string(),
            ..self.clone()
        }
    }
    /// The kinds and texts of a seat's worklog, newest first.
    async fn worklog(&self, seat: &str, query: &str) -> Vec<(String, String)> {
        self.get(&format!("/seats/{seat}/worklog?{query}")).await["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().to_string(),
                    e["text"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }
}

const NOTE: &str = "## State\non track\n\n## Summary\nWriting the parser.\n\n## Next\n1. cargo test -p parser\n\n\
                    ## Waiting on\nnothing\n\n## Traps\nDo not touch lexer.rs.\n\n## Verify\ncargo test prints ok\n";

async fn note_fields_cap_and_revisions(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    // No note yet.
    let v = a.get("/seats/a/handover").await;
    assert_eq!(v["current_rev"], 0);
    assert!(v.get("note").is_none(), "{v}");
    // Missing fields, named in order.
    let (s, v) = a
        .refused(
            Method::PUT,
            "/seats/a/handover",
            Some(json!({ "text": "State: on track\nVerify: make\n" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "note_fields_missing");
    assert_eq!(
        v["missing"],
        json!(["Summary", "Next", "Waiting on", "Traps"])
    );
    // The cap, with the count.
    let big = format!("{NOTE}\n## Pointers\n{}\n", "p".repeat(16_400));
    let (s, v) = a
        .refused(
            Method::PUT,
            "/seats/a/handover",
            Some(json!({ "text": big })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "note_too_large");
    assert!(v["chars"].as_u64().unwrap() > 16_384, "{v}");
    assert_eq!(v["max"], 16_384);
    // Written: revision 1, Pointers filled in by the server.
    let v = a
        .put(
            "/seats/a/handover",
            json!({ "text": NOTE, "session": "s1", "future_field": 1 }),
        )
        .await;
    assert_eq!(v["current_rev"], 1);
    let n = &v["note"];
    assert_eq!(n["rev"], 1);
    assert_eq!(n["fields"]["state"], "on track");
    assert_eq!(n["fields"]["waiting_on"], "nothing");
    assert_eq!(n["pointers_filled"], true);
    assert_eq!(n["source"], "agent");
    assert_eq!(n["session"], "s1");
    assert!(n["text"].as_str().unwrap().starts_with("State: on track\n"));
    // The fields as JSON; the user writes for the seat.
    let mut fields = n["fields"].clone();
    fields["summary"] = json!("Second revision.");
    fields["pointers"] = json!("t3, m9");
    let v = org
        .put("/seats/a/handover", json!({ "fields": fields }))
        .await;
    assert_eq!(v["note"]["rev"], 2);
    assert_eq!(v["note"]["written_by"], "owner");
    assert_eq!(v["note"]["pointers_filled"], false);
    assert_eq!(v["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(v["revisions"][0]["rev"], 2);
    // An older revision; one that does not exist.
    let v = b.get("/seats/a/handover?rev=1").await;
    assert_eq!(v["note"]["fields"]["summary"], "Writing the parser.");
    assert_eq!(v["current_rev"], 2);
    let (s, _) = b
        .refused(Method::GET, "/seats/a/handover?rev=7", None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Only the seat and the user write it; a node writes drafts only.
    let (s, _) = b
        .refused(
            Method::PUT,
            "/seats/a/handover",
            Some(json!({ "text": NOTE })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let node = org.node().await;
    let (s, _) = node
        .refused(
            Method::PUT,
            "/seats/a/handover",
            Some(json!({ "text": NOTE })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let v = node
        .put(
            "/seats/a/handover",
            json!({ "text": "The agent was refactoring the parser; tests pass.", "source": "compaction", "session": "s2" }),
        )
        .await;
    let n = &v["note"];
    assert_eq!(
        (n["rev"].as_i64(), n["draft"].as_bool()),
        (Some(3), Some(true))
    );
    assert_eq!(n["fields"]["state"], "draft: from compaction summary");
    assert_eq!(
        n["fields"]["summary"],
        "The agent was refactoring the parser; tests pass."
    );
    // Unknown seats; the system seat is not one.
    let (s, _) = org
        .refused(Method::GET, "/seats/nobody/handover", None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Another org's seats are not reachable.
    let other = srv.org("beta").await;
    let (s, _) = other.refused(Method::GET, "/seats/a/handover", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

async fn private_notes(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let lead = org.seat("lead").await;
    org.ok(Method::PATCH, "/seats/a", Some(json!({ "group": "core" })))
        .await;
    org.ok(Method::PUT, "/teams/core", Some(json!({ "root": "lead" })))
        .await;
    let node = org.node().await;
    // Not private: every seat of the org reads it.
    a.put("/seats/a/handover", json!({ "text": NOTE })).await;
    assert_eq!(b.get("/seats/a/handover").await["private"], false);
    // Private: the seat, its lead, the user and the node.
    a.put(
        "/seats/a/handover",
        json!({ "text": NOTE, "private": true }),
    )
    .await;
    let (s, v) = b.refused(Method::GET, "/seats/a/handover", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "private");
    // Earlier revisions too, while the current note is private.
    let (s, _) = b
        .refused(Method::GET, "/seats/a/handover?rev=1", None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    for c in [&a, &lead, &org, &node] {
        let v = c.get("/seats/a/handover").await;
        assert_eq!(v["private"], true);
        assert_eq!(v["note"]["rev"], 2);
    }
    // `private` not given: stays private.
    a.put("/seats/a/handover", json!({ "text": NOTE })).await;
    let (s, _) = b.refused(Method::GET, "/seats/a/handover", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    a.put(
        "/seats/a/handover",
        json!({ "text": NOTE, "private": false }),
    )
    .await;
    assert_eq!(b.get("/seats/a/handover").await["note"]["rev"], 4);
    // The seat's setting (the team file's `handover = "private"`): the
    // user only; it makes the notes private whatever their flags.
    for c in [&a, &node] {
        let (s, _) = c
            .refused(
                Method::PUT,
                "/seats/a/handover/private",
                Some(json!({ "private": true })),
            )
            .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let v = org
        .put("/seats/a/handover/private", json!({ "private": true }))
        .await;
    assert_eq!(
        (v["private"].clone(), v["changed"].clone()),
        (json!(true), json!(true))
    );
    let v = org
        .put("/seats/a/handover/private", json!({ "private": true }))
        .await;
    assert_eq!(v["changed"], false);
    assert_eq!(org.get("/seats/a/handover/private").await["private"], true);
    let (s, _) = b.refused(Method::GET, "/seats/a/handover", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    org.put("/seats/a/handover/private", json!({ "private": false }))
        .await;
    assert_eq!(b.get("/seats/a/handover").await["private"], false);
}

async fn requests_mail_the_seat(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let lead = org.seat("lead").await;
    org.ok(Method::PATCH, "/seats/a", Some(json!({ "group": "core" })))
        .await;
    org.ok(Method::PUT, "/teams/core", Some(json!({ "root": "lead" })))
        .await;
    // Not the lead.
    let (s, _) = b
        .refused(Method::POST, "/seats/a/handover/request", None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let v = lead.post("/seats/a/handover/request", json!({})).await;
    assert!(v["message_id"].as_i64().is_some(), "{v}");
    let inbox = a.get("/inbox").await;
    let e = &inbox["entries"][0];
    assert_eq!(e["message"]["subject"], "Handover note requested by lead");
    assert_eq!(e["delivery"]["class"], "to", "{e}");
    let id = e["message"]["id"].as_i64().unwrap();
    let m = a.get(&format!("/messages/{id}")).await;
    assert!(
        m["message"]["body"]
            .as_str()
            .unwrap()
            .contains("amail handover write")
    );
    let v = a.get("/seats/a/handover").await;
    assert_eq!(v["requested_by"], "lead");
    assert!(v["requested_at"].is_string());
    // A newer note answers the request.
    a.put("/seats/a/handover", json!({ "text": NOTE })).await;
    let v = a.get("/seats/a/handover").await;
    assert!(v.get("requested_at").is_none(), "{v}");
    // A node asks in the name of the system seat.
    let node = org.node().await;
    let v = node.post("/seats/a/handover/request", json!({})).await;
    assert!(v["message_id"].as_i64().is_some(), "{v}");
    let v = a.get("/seats/a/handover").await;
    assert_eq!(v["requested_by"], "amail");
    let inbox = a.get("/inbox").await;
    assert!(
        inbox["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["message"]["subject"] == "Handover note requested by the node"),
        "{inbox}"
    );
    // The user may ask too; a seat without a team has no lead.
    org.post("/seats/b/handover/request", json!({})).await;
    let (s, _) = lead
        .refused(Method::POST, "/seats/b/handover/request", None)
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

async fn read_acknowledgement(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let node = org.node().await;
    let (s, _) = node
        .refused(
            Method::POST,
            "/seats/a/handover/read",
            Some(json!({ "session": "s1" })),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    a.put("/seats/a/handover", json!({ "text": NOTE })).await;
    let v = node
        .post("/seats/a/handover/read", json!({ "session": "s1" }))
        .await;
    assert_eq!(
        (v["rev"].as_i64(), v["read_by_session"].as_str()),
        (Some(1), Some("s1"))
    );
    let n = b.get("/seats/a/handover").await;
    assert_eq!(n["note"]["read_by_session"], "s1");
    assert!(n["note"]["read_at"].is_string());
    assert_eq!(n["revisions"][0]["read_by_session"], "s1");
    let (s, _) = b
        .refused(
            Method::POST,
            "/seats/a/handover/read",
            Some(json!({ "session": "s1" })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = node
        .refused(
            Method::POST,
            "/seats/a/handover/read",
            Some(json!({ "session": "" })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

async fn idle_note_when_the_last_task_closes(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    for t in ["parser", "lexer"] {
        org.post("/tasks", json!({ "title": t, "assignee": "a" }))
            .await;
    }
    a.ok(Method::PATCH, "/tasks/1", Some(json!({ "state": "doing" })))
        .await;
    a.ok(
        Method::PATCH,
        "/tasks/1",
        Some(json!({ "state": "done", "result": "ok", "result_note": "tests pass" })),
    )
    .await;
    // t2 is still open: no note.
    assert_eq!(a.get("/seats/a/handover").await["current_rev"], 0);
    // The reviewer (here the user) closes the last one.
    org.ok(
        Method::PATCH,
        "/tasks/2",
        Some(json!({ "state": "cancelled" })),
    )
    .await;
    let v = a.get("/seats/a/handover").await;
    let n = &v["note"];
    assert_eq!(n["source"], "server", "{v}");
    assert!(n["fields"]["state"].as_str().unwrap().starts_with("idle"));
    assert!(
        n["fields"]["summary"]
            .as_str()
            .unwrap()
            .contains("t2 (lexer) is cancelled")
    );
    let p = n["fields"]["pointers"].as_str().unwrap();
    assert!(p.contains("t2") && p.contains("t1"), "{p}");
}

async fn server_entries(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let sent = a
        .post(
            "/messages",
            json!({ "to": ["b"], "subject": "[t1] parser review", "body": "see", "ack_required": true }),
        )
        .await;
    let id = sent["id"].as_i64().unwrap();
    let wl = a.worklog("a", "").await;
    assert_eq!(wl[0].0, "mail_sent");
    assert_eq!(wl[0].1, format!("m{id} \"[t1] parser review\" to b"));
    // `[t1]` names the task.
    assert_eq!(a.get("/seats/a/worklog").await["entries"][0]["task"], 1);
    b.get(&format!("/messages/{id}")).await;
    b.post(&format!("/messages/{id}/ack"), json!({})).await;
    let wl = b.worklog("b", "").await;
    assert_eq!(
        wl[0],
        (
            "mail_acked".into(),
            format!("m{id} \"[t1] parser review\" from a")
        )
    );
    assert_eq!(wl[1].0, "mail_read");
    // Peeking writes nothing.
    b.get(&format!("/messages/{id}?peek=1")).await;
    assert_eq!(b.worklog("b", "").await.len(), 2);
    // Tasks: created and changed by the seat; changes by others are not
    // the seat's.
    a.post("/tasks", json!({ "title": "parser", "assignee": "a" }))
        .await;
    a.ok(Method::PATCH, "/tasks/1", Some(json!({ "state": "doing" })))
        .await;
    org.ok(
        Method::PATCH,
        "/tasks/1",
        Some(json!({ "priority": "high" })),
    )
    .await;
    let wl = a.worklog("a", "kind=task").await;
    assert_eq!(wl[0].1, "t1: open → doing");
    assert_eq!(wl[1].1, "t1: created (open): parser");
    assert_eq!(wl.len(), 2);
    // Focus.
    a.ok(Method::PUT, "/focus", Some(json!({ "for_s": 600 })))
        .await;
    a.ok(Method::DELETE, "/focus", None).await;
    let wl = a.worklog("a", "kind=focus").await;
    assert_eq!(wl[0].1, "focus off");
    assert!(wl[1].1.starts_with("focus on until "));
    // While t1 is `doing`, server entries name it.
    let v = a.get("/seats/a/worklog?kind=focus").await;
    assert_eq!(v["entries"][0]["task"], 1);
    // The user seat has no worklog.
    org.post(
        "/messages",
        json!({ "to": ["a"], "subject": "hi", "body": "x" }),
    )
    .await;
    assert!(org.worklog("owner", "").await.is_empty());
}

async fn node_entries_and_queries(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let c = org.seat("c").await;
    let lead = org.seat("lead").await;
    for s in ["a", "b"] {
        org.ok(
            Method::PATCH,
            &format!("/seats/{s}"),
            Some(json!({ "group": "core" })),
        )
        .await;
    }
    org.ok(Method::PUT, "/teams/core", Some(json!({ "root": "lead" })))
        .await;
    let node = org.node().await;
    let old = "2026-01-01T00:00:00Z";
    let v = node
        .post(
            "/worklog",
            json!({ "node": "host1", "entries": [
                { "seat": "a", "session": "s1", "kind": "session_start", "text": "startup", "at": old },
                { "seat": "a", "session": "s1", "kind": "tool", "text": "Edit src/parser.rs", "task": 4 },
                { "seat": "a", "session": "s1", "kind": "commit", "text": "abc1234 Fix the parser", "extra": true },
                { "seat": "a", "session": "s1", "kind": "tool", "text": "x".repeat(3000) },
                { "seat": "nobody", "kind": "tool", "text": "unknown seat" },
                { "seat": "owner", "kind": "tool", "text": "the user seat" },
                { "seat": "a", "kind": "Bad Kind", "text": "unreadable kind" },
                { "seat": "a", "kind": "tool", "text": "unreadable time", "at": "yesterday" },
            ]}),
        )
        .await;
    assert_eq!(
        (v["accepted"].as_u64(), v["skipped"].as_u64()),
        (Some(4), Some(4))
    );
    // Newest first; text cut to 1,000 characters.
    let p = a.get("/seats/a/worklog").await;
    let es = p["entries"].as_array().unwrap();
    assert_eq!(es.len(), 4);
    assert_eq!(es[0]["text"].as_str().unwrap().chars().count(), 1000);
    assert_eq!(es[0]["source"], "node");
    assert_eq!(es[0]["session"], "s1");
    // Filters and paging.
    assert_eq!(a.worklog("a", "kind=commit").await.len(), 1);
    assert_eq!(a.worklog("a", "task=t4").await.len(), 1);
    assert_eq!(a.worklog("a", "since=2h").await.len(), 3);
    let p = a.get("/seats/a/worklog?limit=3").await;
    let before = p["next_before"].as_i64().unwrap();
    let p2 = a.get(&format!("/seats/a/worklog?before={before}")).await;
    assert_eq!(p2["entries"][0]["kind"], "session_start");
    assert!(p2.get("next_before").is_none());
    let (s, _) = a
        .refused(Method::GET, "/seats/a/worklog?since=soon", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // The summary.
    let sm = a.get("/seats/a/worklog?summary=1").await;
    assert_eq!(sm["total"], 4);
    assert_eq!(sm["kinds"][0], json!({ "kind": "tool", "count": 2 }));
    assert_eq!(sm["tasks"][0]["task"], 4);
    assert_eq!(sm["last"].as_array().unwrap().len(), 4);
    // Who reads it: the seat, its team, its lead, the user.
    for who in [&a, &b, &lead, &org] {
        assert_eq!(who.worklog("a", "").await.len(), 4);
    }
    let (s, _) = c.refused(Method::GET, "/seats/a/worklog", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Node tokens read it (the node looks at the tasks a seat closed).
    assert_eq!(node.worklog("a", "").await.len(), 4);
    // Only node tokens post.
    let (s, _) = a
        .refused(
            Method::POST,
            "/worklog",
            Some(json!({ "entries": [{ "seat": "a", "kind": "tool", "text": "x" }] })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A note written in session s1 points at its task and the commit.
    let v = a
        .put(
            "/seats/a/handover",
            json!({ "text": NOTE, "session": "s1" }),
        )
        .await;
    let p = v["note"]["fields"]["pointers"].as_str().unwrap();
    assert!(p.contains("Tasks: t4"), "{p}");
    assert!(p.contains("Last commit: abc1234 Fix the parser"), "{p}");
    // ... and the note itself is in the worklog.
    assert_eq!(a.worklog("a", "kind=handover").await.len(), 1);
}

async fn public_limits(srv: Server) {
    // A self-hosted server: no batch limit, no retention.
    let org = srv.org("alpha").await;
    org.seat("a").await;
    let node = org.node().await;
    let many: Vec<Value> = (0..6000)
        .map(|i| json!({ "seat": "a", "kind": "tool", "text": format!("e{i}") }))
        .collect();
    let v = node.post("/worklog", json!({ "entries": many })).await;
    assert_eq!(v["accepted"], 6000);
    // Configured limits (public mode turns them on).
    let mut cfg = Config::default();
    cfg.limits.worklog_batch_entries = Some(3);
    cfg.limits.worklog_days = Some(90);
    cfg.retention_every = Duration::from_millis(100);
    assert_eq!(Config::public().limits.worklog_days, Some(90));
    assert_eq!(Config::public().limits.worklog_batch_entries, Some(5000));
    let srv2 = Server::start(srv.backend, "public_limits_2", cfg)
        .await
        .unwrap();
    let org = srv2.org("alpha").await;
    let a = org.seat("a").await;
    let node = org.node().await;
    let four: Vec<Value> = (0..4)
        .map(|i| json!({ "seat": "a", "kind": "tool", "text": format!("e{i}") }))
        .collect();
    let (s, v) = node
        .refused(Method::POST, "/worklog", Some(json!({ "entries": four })))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "too_many_entries");
    node.post(
        "/worklog",
        json!({ "entries": [
            { "seat": "a", "kind": "tool", "text": "old", "at": "2020-01-01T00:00:00Z" },
            { "seat": "a", "kind": "tool", "text": "new" },
        ]}),
    )
    .await;
    // The retention pass removes the old entry.
    for _ in 0..100 {
        if a.worklog("a", "").await.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        a.worklog("a", "").await,
        vec![("tool".into(), "new".into())]
    );
}
