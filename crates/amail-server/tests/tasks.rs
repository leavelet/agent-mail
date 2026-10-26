//! Tasks over HTTP (M8): the calls, reminders (to the right seats, once),
//! the `[t12]` link from subjects, comments, live events, other orgs'
//! visibility (none), and the bounds. Both backends (Postgres only when
//! `AMAIL_TEST_PG_URL` is set).

mod common;

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

macro_rules! task_tests {
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

task_tests!(
    create_show_change,
    personal_queries_and_board,
    reminders_reach_the_right_seats_once,
    due_and_stale_reminders,
    subject_tags_link_threads,
    comments_are_a_thread,
    live_task_events,
    other_orgs_see_no_tasks,
    size_bounds,
    storage_quota_and_request_limit,
    reminder_cap,
    work_stealing,
    revisions_and_client_keys,
    children_results_and_reminders,
    import_and_report,
);

struct Server {
    base: String,
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
        state.attention.scan_every = Duration::from_millis(100);
        state.attention.task_scan_every = Duration::from_millis(100);
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

    /// Run SQL on the test database (to age rows).
    async fn sql(&self, q: &str) {
        use sqlx::Connection;
        match self.backend {
            Backend::Sqlite => {
                let mut c = sqlx::SqliteConnection::connect(&self.db.url).await.unwrap();
                sqlx::raw_sql(q).execute(&mut c).await.unwrap();
            }
            Backend::Postgres => {
                let mut c = sqlx::PgConnection::connect(&self.db.url).await.unwrap();
                sqlx::raw_sql(q).execute(&mut c).await.unwrap();
            }
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
    async fn patch(&self, num: i64, body: Value) -> Value {
        self.ok(Method::PATCH, &format!("/tasks/{num}"), Some(body))
            .await
    }
    /// Create a task; its number.
    async fn task(&self, body: Value) -> i64 {
        self.post("/tasks", body).await["task"]["num"]
            .as_i64()
            .unwrap()
    }
    /// The error code of a refused call.
    async fn refused(&self, method: Method, path: &str, body: Value) -> (StatusCode, String) {
        let (s, v) = self.call(method, path, Some(body)).await;
        assert!(!s.is_success(), "{path} should be refused: {v}");
        (
            s,
            v["error"]["code"].as_str().unwrap_or_default().to_string(),
        )
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
    async fn inbox(&self) -> Vec<Value> {
        self.get("/inbox?limit=500").await["entries"]
            .as_array()
            .unwrap()
            .clone()
    }
    /// Subjects of the system mail in the caller's inbox, oldest first.
    async fn reminders(&self) -> Vec<String> {
        self.inbox()
            .await
            .iter()
            .filter(|e| e["message"]["source"] == "system")
            .map(|e| e["message"]["subject"].as_str().unwrap().to_string())
            .collect()
    }
}

fn nums(v: &Value) -> Vec<i64> {
    v["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["num"].as_i64().unwrap())
        .collect()
}

async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn create_show_change(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    org.seat("w2").await;
    let d = org
        .post(
            "/tasks",
            json!({ "title": "  Write the parser ", "body": "details", "assignee": "w1",
                    "priority": "high", "labels": ["core", "core", "parser"],
                    "due": "2030-01-01T00:00:00Z" }),
        )
        .await;
    let t = &d["task"];
    assert_eq!(t["num"], 1);
    assert_eq!(t["title"], "Write the parser");
    assert_eq!(t["state"], "ready");
    assert_eq!(t["stored_state"], "open");
    assert_eq!(t["reporter"], "owner");
    assert_eq!(t["assignee"], "w1");
    assert_eq!(t["labels"], json!(["core", "parser"]));
    assert_eq!(t["due"], "2030-01-01T00:00:00Z");
    assert_eq!(d["events"][0]["kind"], "created");
    // `t1` and `1` name the same task; seat tokens see every task.
    let got = w1.get("/tasks/t1").await;
    assert_eq!(got["task"]["body"], "details");
    // A change records one event per field, with the seat as actor.
    let d = w1
        .patch(
            1,
            json!({ "state": "doing", "priority": "urgent", "title": "Write the parser now" }),
        )
        .await;
    let evs = d["events"].as_array().unwrap();
    let fields: Vec<&str> = evs
        .iter()
        .filter(|e| e["kind"] == "changed")
        .map(|e| e["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields, ["title", "state", "priority"]);
    assert!(
        evs.iter()
            .filter(|e| e["kind"] == "changed")
            .all(|e| e["actor"] == "w1")
    );
    // Blocked on a seat; leaving `blocked` clears it.
    let d = w1
        .patch(
            1,
            json!({ "state": "blocked", "waiting_on": "w2", "blocked_reason": "API?" }),
        )
        .await;
    assert_eq!(d["task"]["waiting_on"], "w2");
    let d = w1.patch(1, json!({ "state": "review" })).await;
    assert!(d["task"].get("waiting_on").is_none(), "{d}");
    // Refusals.
    let (s, c) = w1
        .refused(Method::PATCH, "/tasks/1", json!({ "state": "ready" }))
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::BAD_REQUEST, "invalid"));
    let (s, c) = w1
        .refused(Method::PATCH, "/tasks/1", json!({ "waiting_on": "w2" }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "not_blocked")
    );
    let (s, _) = w1
        .refused(Method::PATCH, "/tasks/1", json!({ "assignee": "nobody" }))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = w1
        .refused(Method::PATCH, "/tasks/9", json!({ "title": "x" }))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    w1.patch(1, json!({ "state": "done" })).await;
    let (s, c) = w1
        .refused(Method::PATCH, "/tasks/1", json!({ "state": "cancelled" }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_transition")
    );
    let d = w1.patch(1, json!({ "state": "open" })).await;
    assert_eq!(d["task"]["state"], "ready");
    // Unassigning; `take` is atomic and refuses a taken task.
    w1.patch(1, json!({ "assignee": null })).await;
    let w2 = org.seat("w3").await;
    let d = w2.post("/tasks/1/take", json!({})).await;
    assert_eq!(d["task"]["assignee"], "w3");
    let (s, c) = w1.refused(Method::POST, "/tasks/1/take", json!({})).await;
    assert_eq!((s, c.as_str()), (StatusCode::CONFLICT, "taken"));
    // Dependencies and the shown state.
    let t2 = org.task(json!({ "title": "second" })).await;
    let d = org
        .post(&format!("/tasks/{t2}/deps"), json!({ "blocked_by": 1 }))
        .await;
    assert_eq!(d["task"]["state"], "open");
    assert_eq!(d["task"]["waits_on"], json!([1]));
    assert_eq!(d["blockers"][0]["num"], 1);
    let (s, c) = org
        .refused(Method::POST, "/tasks/1/deps", json!({ "blocked_by": t2 }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "dependency_cycle")
    );
    let d = org
        .ok(Method::DELETE, &format!("/tasks/{t2}/deps/t1"), None)
        .await;
    assert_eq!(d["task"]["state"], "ready");
    // Paged history.
    let p = org.get("/tasks/1/events?limit=3").await;
    assert_eq!(p["events"].as_array().unwrap().len(), 3);
    let after = p["next_after_seq"].as_i64().unwrap();
    let p2 = org.get(&format!("/tasks/1/events?after_seq={after}")).await;
    assert_eq!(p2["events"][0]["seq"].as_i64().unwrap(), after + 1);
    // From a message: its subject is the title, its thread is linked.
    let m = w1
        .post(
            "/messages",
            json!({ "to": ["owner"], "subject": "Crash on start", "body": "trace" }),
        )
        .await;
    let d = org.post("/tasks", json!({ "from_message": m["id"] })).await;
    assert_eq!(d["task"]["title"], "Crash on start");
    assert_eq!(d["links"][0]["thread_id"], m["thread_id"]);
    // The org's setting.
    let o = org
        .ok(
            Method::PATCH,
            "/org",
            Some(json!({ "task_stale_hours": 0 })),
        )
        .await;
    assert_eq!(o["task_stale_hours"], 0);
    let (s, _) = org
        .refused(Method::PATCH, "/org", json!({ "task_stale_hours": -1 }))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// Eight tasks, three seats, two blocked chains: the personal queries
/// return the documented sets, and a task shows `ready` exactly when its
/// last blocker closes.
async fn personal_queries_and_board(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    org.ok(Method::PATCH, "/seats/a", Some(json!({ "group": "core" })))
        .await;
    org.ok(Method::PATCH, "/seats/b", Some(json!({ "group": "core" })))
        .await;
    let c = org.seat("c").await;
    // Chain 1: t1 (a) <- t2 (b) <- t3 (c). Chain 2: t4 (b) <- t5 (a).
    // t6 (a, urgent), t7 unassigned in team core, t8 (c) blocked on a.
    for (title, who) in [
        ("t1", Some("a")),
        ("t2", Some("b")),
        ("t3", Some("c")),
        ("t4", Some("b")),
        ("t5", Some("a")),
        ("t6", Some("a")),
        ("t7", None),
        ("t8", Some("c")),
    ] {
        let mut body = json!({ "title": title });
        if let Some(w) = who {
            body["assignee"] = json!(w);
        } else {
            body["team"] = json!("core");
        }
        org.task(body).await;
    }
    org.post("/tasks/2/deps", json!({ "blocked_by": 1 })).await;
    org.post("/tasks/3/deps", json!({ "blocked_by": 2 })).await;
    org.post("/tasks/5/deps", json!({ "blocked_by": 4 })).await;
    org.patch(6, json!({ "priority": "urgent" })).await;
    c.patch(8, json!({ "state": "blocked", "waiting_on": "a" }))
        .await;
    // mine (a): ready t6 (urgent), t1; open t5.
    assert_eq!(nums(&a.get("/tasks/mine").await), [6, 1, 5]);
    let mine = a.get("/tasks/mine").await;
    assert_eq!(mine["tasks"][2]["state"], "open");
    assert_eq!(mine["tasks"][2]["waits_on"], json!([4]));
    // next (a): ready ones; with the pool also unassigned t7 of core.
    assert_eq!(nums(&a.get("/tasks/next").await), [6, 1]);
    assert_eq!(nums(&a.get("/tasks/next?pool=1").await), [6, 1, 7]);
    assert_eq!(nums(&c.get("/tasks/next?pool=1").await), Vec::<i64>::new());
    // waiting (a): t5 waits on t4 of b.
    let w = a.get("/tasks/waiting").await;
    assert_eq!(w["items"].as_array().unwrap().len(), 1);
    assert_eq!(w["items"][0]["task"]["num"], 5);
    assert_eq!(w["items"][0]["blocker"]["num"], 4);
    assert_eq!(w["items"][0]["seat"], "b");
    // waiting (c): t8 on a, t3 on t2 (b).
    let w = c.get("/tasks/waiting").await;
    let got: Vec<(i64, String)> = w["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["task"]["num"].as_i64().unwrap(),
                i["seat"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(got, [(8, "a".to_string()), (3, "b".to_string())]);
    // owed (a): t8 waits on a; t1 is the last blocker of b's t2.
    let o = a.get("/tasks/owed").await;
    let got: Vec<(i64, Option<i64>)> = o["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["task"]["num"].as_i64().unwrap(),
                i["blocks"]["num"].as_i64(),
            )
        })
        .collect();
    assert_eq!(got, [(8, None), (1, Some(2))]);
    // owed (b): t4 blocks a's t5; t2 blocks c's t3.
    let o = b.get("/tasks/owed").await;
    let got: Vec<i64> = o["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["task"]["num"].as_i64().unwrap())
        .collect();
    assert_eq!(got, [2, 4]);
    // `ready` exactly when the last blocker closes.
    a.patch(1, json!({ "state": "done" })).await;
    assert_eq!(b.get("/tasks/2").await["task"]["state"], "ready");
    assert_eq!(c.get("/tasks/3").await["task"]["state"], "open");
    b.patch(2, json!({ "state": "cancelled" })).await;
    assert_eq!(c.get("/tasks/3").await["task"]["state"], "ready");
    // Filters of the list.
    let l = org.get("/tasks?state=ready&assignee=c").await;
    assert_eq!(nums(&l), [3]);
    let l = org.get("/tasks?state=all&limit=3").await;
    assert_eq!(nums(&l), [8, 7, 6]);
    let l2 = org
        .get(&format!("/tasks?state=all&before={}", l["next_before"]))
        .await;
    assert_eq!(nums(&l2), [5, 4, 3, 2, 1]);
    assert_eq!(nums(&org.get("/tasks?text=T7").await), [7]);
    assert_eq!(nums(&org.get("/tasks?assignee=none").await), [7]);
    assert_eq!(nums(&org.get("/tasks?state=done,cancelled").await), [2, 1]);
    // Board.
    let bd = org.get("/tasks/board").await;
    let count = |s: &str| {
        bd["columns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["state"] == s)
            .unwrap()["count"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(
        (
            count("ready"),
            count("open"),
            count("blocked"),
            count("done")
        ),
        (4, 1, 1, 1)
    );
    assert_eq!(bd["chains"][0]["num"], 5);
    let team = org.get("/tasks/board?team=core").await;
    assert!(team["columns"].as_array().unwrap().iter().all(|c| {
        c["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["num"] != 3 && t["num"] != 8)
    }));
}

async fn reminders_reach_the_right_seats_once(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    // Assigned: the assignee is told, the creator is not.
    org.task(json!({ "title": "base", "assignee": "w1" })).await;
    org.task(json!({ "title": "top", "assignee": "w2" })).await;
    org.post("/tasks/2/deps", json!({ "blocked_by": 1 })).await;
    eventually("assignment reminders", || async {
        w1.reminders().await == ["t1 assigned to you: base"]
            && w2.reminders().await == ["t2 assigned to you: top"]
    })
    .await;
    assert!(org.reminders().await.is_empty());
    // The actor is never told about its own change.
    w1.patch(1, json!({ "state": "doing" })).await;
    // Closing the last blocker: the dependent's assignee and the reporter.
    w1.patch(1, json!({ "state": "done" })).await;
    eventually("closing reminders", || async {
        w2.reminders().await.len() == 2
    })
    .await;
    assert_eq!(w2.reminders().await[1], "t2 is no longer blocked: t1 done");
    assert_eq!(org.reminders().await, ["t1 done: base"]);
    assert_eq!(w1.reminders().await.len(), 1);
    // Blocked on a seat.
    w2.patch(
        2,
        json!({ "state": "blocked", "waiting_on": "w1", "blocked_reason": "need the schema" }),
    )
    .await;
    // Coalesced: one change that reassigns and raises to urgent sends one
    // mail to the new assignee.
    org.patch(2, json!({ "assignee": "w1", "priority": "urgent" }))
        .await;
    eventually("waiting and coalesced reminders", || async {
        w1.reminders().await.len() == 3
    })
    .await;
    let r = w1.reminders().await;
    assert_eq!(r[1], "t2 waits for you: need the schema");
    assert_eq!(r[2], "t2 assigned to you: top");
    let inbox = w1.inbox().await;
    let id = inbox.last().unwrap()["message"]["id"].as_i64().unwrap();
    let m = w1.get(&format!("/messages/{id}?peek=1")).await;
    let body = m["message"]["body"].as_str().unwrap();
    assert!(body.contains("t2 is now urgent: top"), "{body}");
    assert!(body.contains("amail task show t2"), "{body}");
    // Reminders are addressed in To (an idle agent is woken).
    assert_eq!(inbox.last().unwrap()["delivery"]["class"], "to");
    // The history records them.
    let ev = org.get("/tasks/2/events").await;
    let sent: Vec<&Value> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "reminder_sent")
        .collect();
    assert!(
        sent.iter()
            .any(|e| e["field"] == "urgent" && e["to"] == "w1")
    );
    assert!(sent.iter().all(|e| e.get("actor").is_none()));
    // Raising to urgent alone tells the assignee; lowering does not.
    org.patch(2, json!({ "priority": "low" })).await;
    org.patch(2, json!({ "priority": "urgent" })).await;
    eventually("urgent reminder", || async {
        w1.reminders().await.len() == 4
    })
    .await;
    assert_eq!(w1.reminders().await[3], "t2 is now urgent: top");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w1.reminders().await.len(), 4);
    assert_eq!(w2.reminders().await.len(), 2);
}

async fn due_and_stale_reminders(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    org.ok(Method::PUT, "/teams/core", Some(json!({}))).await;
    org.ok(Method::PATCH, "/seats/w1", Some(json!({ "group": "core" })))
        .await;
    let lead = org.seat("lead").await;
    org.ok(Method::PUT, "/teams/core", Some(json!({ "root": "lead" })))
        .await;
    org.task(json!({ "title": "late", "assignee": "w1", "due": "2001-01-01T00:00:00Z" }))
        .await;
    eventually("due reminder", || async {
        w1.reminders()
            .await
            .iter()
            .any(|s| s.starts_with("t1 is past its due time"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let due = w1
        .reminders()
        .await
        .iter()
        .filter(|s| s.starts_with("t1 is past"))
        .count();
    assert_eq!(due, 1, "once");
    // Stale: doing, not changed for the period (rows aged here).
    w1.patch(1, json!({ "state": "doing" })).await;
    // (The pass may have run for the org meanwhile: at most once an hour.)
    srv.sql("UPDATE tasks SET updated_at = 0; UPDATE orgs SET task_stale_pass_at = NULL")
        .await;
    eventually("stale reminder", || async {
        w1.reminders()
            .await
            .iter()
            .any(|s| s.starts_with("t1 has not changed"))
    })
    .await;
    // The team's lead gets a copy (Cc).
    let lr = lead.inbox().await;
    let copy = lr
        .iter()
        .find(|e| {
            e["message"]["subject"]
                .as_str()
                .unwrap()
                .starts_with("t1 has not changed")
        })
        .expect("the lead's copy");
    assert_eq!(copy["delivery"]["class"], "cc");
    // Once per period: the pass ran for this org; no second one.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let n = w1
        .reminders()
        .await
        .iter()
        .filter(|s| s.starts_with("t1 has not changed"))
        .count();
    assert_eq!(n, 1);
}

async fn subject_tags_link_threads(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    org.task(json!({ "title": "parser" })).await;
    let m = w1
        .post(
            "/messages",
            json!({ "to": ["owner"], "subject": "[t1] question about [t99]", "body": "?" }),
        )
        .await;
    let thread = m["thread_id"].as_i64().unwrap();
    let d = org.get("/tasks/1").await;
    assert_eq!(d["links"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["links"][0]["thread_id"], thread);
    assert_eq!(d["links"][0]["linked_by"], "w1");
    // A reply with the tag again links nothing new.
    org.post(
        "/messages",
        json!({ "to": ["w1"], "subject": "Re: [t1] question", "body": "!", "in_reply_to": m["id"] }),
    )
    .await;
    let d = org.get("/tasks/1").await;
    assert_eq!(d["links"].as_array().unwrap().len(), 1);
    let linked = d["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "linked")
        .count();
    assert_eq!(linked, 1);
    // The thread answer lists the task.
    let t = org.get(&format!("/threads/{thread}")).await;
    assert_eq!(t["tasks"][0]["num"], 1);
    assert_eq!(t["tasks"][0]["title"], "parser");
    // Linking by hand: by message id; unlink.
    let m2 = w1
        .post(
            "/messages",
            json!({ "to": ["owner"], "subject": "other", "body": "x" }),
        )
        .await;
    let d = w1
        .post("/tasks/1/links", json!({ "message_id": m2["id"] }))
        .await;
    assert_eq!(d["links"].as_array().unwrap().len(), 2);
    org.ok(
        Method::DELETE,
        &format!("/tasks/1/links/{}", m2["thread_id"]),
        None,
    )
    .await;
    let (s, _) = org
        .call(
            Method::DELETE,
            &format!("/tasks/1/links/{}", m2["thread_id"]),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A seat links only threads it is in.
    let w2 = org.seat("w2").await;
    let (s, _) = w2
        .call(
            Method::POST,
            "/tasks/1/links",
            Some(json!({ "thread_id": m2["thread_id"] })),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

async fn comments_are_a_thread(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    org.task(json!({ "title": "parser", "assignee": "w1" }))
        .await;
    // First comment (by w2): to the assignee and the reporter.
    let c1 = w2
        .post("/tasks/1/comments", json!({ "body": "Which grammar?" }))
        .await;
    let thread = c1["thread_id"].as_i64().unwrap();
    let m = org.get(&format!("/messages/{}?peek=1", c1["id"])).await;
    assert_eq!(m["message"]["subject"], "[t1] parser");
    let to: Vec<&str> = m["message"]["to"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(to, ["w1", "owner"]);
    let d = org.get("/tasks/1").await;
    assert_eq!(d["task"]["thread"], thread);
    assert!(d["links"].as_array().unwrap().is_empty(), "{d}");
    // Second comment (by w1): to the reporter, Cc the earlier commenter.
    let c2 = w1
        .post("/tasks/1/comments", json!({ "body": "PEG." }))
        .await;
    assert_eq!(c2["thread_id"], thread);
    let m = org.get(&format!("/messages/{}?peek=1", c2["id"])).await;
    assert_eq!(m["message"]["to"], json!(["owner"]));
    assert_eq!(m["message"]["cc"], json!(["w2"]));
    // A reply in the thread is a comment too.
    let r = org
        .post(
            "/messages",
            json!({ "to": ["w1"], "subject": "Re: [t1] parser", "body": "ok", "in_reply_to": c2["id"] }),
        )
        .await;
    let ev = org.get("/tasks/1/events").await;
    let comments: Vec<i64> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "commented")
        .map(|e| e["message_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        comments,
        [
            c1["id"].as_i64().unwrap(),
            c2["id"].as_i64().unwrap(),
            r["id"].as_i64().unwrap()
        ]
    );
    // Reading the thread shows the task.
    let t = w1.get(&format!("/threads/{thread}")).await;
    assert_eq!(t["tasks"][0]["num"], 1);
    // A seat that never took part may comment (the thread is the task's).
    let w3 = org.seat("w3").await;
    w3.post("/tasks/1/comments", json!({ "body": "me too" }))
        .await;
    let (s, _) = w3
        .call(
            Method::POST,
            "/tasks/1/comments",
            Some(json!({ "body": " " })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

async fn live_task_events(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let resp = common::open_stream(
        common::stream_client()
            .get(format!("{}/v1/events", srv.base))
            .bearer_auth(&w1.token),
    )
    .await;
    let mut body = resp.bytes_stream();
    org.task(json!({ "title": "live" })).await;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !buf.contains("event: task") {
        let chunk = tokio::time::timeout_at(deadline, body.next())
            .await
            .expect("a task event")
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(buf.contains(r#""num":1"#), "{buf}");
    assert!(buf.contains(r#""change":"created""#), "{buf}");
}

async fn other_orgs_see_no_tasks(srv: Server) {
    let a = srv.org("alpha").await;
    let b = srv.org("bravo").await;
    let wa = a.seat("wa").await;
    a.post("/links", json!({ "org": "bravo" })).await;
    b.post("/links/alpha/accept", json!({})).await;
    a.task(json!({ "title": "secret plan" })).await;
    a.task(json!({ "title": "second secret" })).await;
    // Bravo's numbers start at 1 and name only its own tasks.
    let (s, _) = b.call(Method::GET, "/tasks/2", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(nums(&b.get("/tasks?state=all").await).is_empty());
    assert!(
        b.get("/tasks/board").await["columns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["count"] == 0)
    );
    let (s, _) = b
        .call(Method::PATCH, "/tasks/1", Some(json!({ "title": "mine" })))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Bravo cannot link alpha's thread.
    let m = wa
        .post(
            "/messages",
            json!({ "to": ["owner"], "subject": "internal", "body": "x" }),
        )
        .await;
    b.task(json!({ "title": "theirs" })).await;
    let (s, _) = b
        .call(
            Method::POST,
            "/tasks/1/links",
            Some(json!({ "thread_id": m["thread_id"] })),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A `[t1]` subject from bravo's user to alpha's user links bravo's t1,
    // not alpha's.
    let x = b
        .post(
            "/messages",
            json!({ "to": ["owner@alpha"], "subject": "[t1] hello", "body": "x" }),
        )
        .await;
    let x_thread = x["thread_id"].as_i64().unwrap();
    assert!(
        a.get("/tasks/1").await["links"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(b.get("/tasks/1").await["links"][0]["thread_id"], x_thread);
    // Alpha reading that thread sees none of bravo's tasks.
    let t = a.get(&format!("/threads/{x_thread}")).await;
    assert!(t.get("tasks").is_none(), "{t}");
    // Node tokens have no tasks, except the count of an agent seat's own
    // (`mine?seat=`) that the node shows at session start.
    a.task(json!({ "title": "for wa", "assignee": "wa" })).await;
    let node = a
        .ok(Method::POST, "/tokens", Some(json!({ "scope": "node" })))
        .await;
    let n = Client {
        token: node["token"].as_str().unwrap().to_string(),
        ..a.clone()
    };
    let (s, _) = n.call(Method::GET, "/tasks", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = n.call(Method::GET, "/tasks/next?seat=wa", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = n.call(Method::GET, "/tasks/mine?seat=wa", None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["tasks"][0]["title"], "for wa", "{v}");
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1, "{v}");
    let (s, _) = n.call(Method::GET, "/tasks/mine", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = n.call(Method::GET, "/tasks/mine?seat=owner", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // A seat token's `seat=` changes nothing: it is always its own seat.
    let (_, v) = wa.call(Method::GET, "/tasks/mine?seat=owner", None).await;
    assert_eq!(v["tasks"][0]["title"], "for wa", "{v}");
}

async fn size_bounds(srv: Server) {
    let org = srv.org("alpha").await;
    let cases = [
        (json!({ "title": "" }), "invalid_title"),
        (json!({ "title": "a\nb" }), "invalid_title"),
        (json!({ "title": "x".repeat(513) }), "title_too_long"),
        (
            json!({ "title": "x", "body": "y".repeat(64 * 1024 + 1) }),
            "body_too_large",
        ),
        (
            json!({ "title": "x", "labels": (0..11).map(|i| format!("l{i}")).collect::<Vec<_>>() }),
            "too_many_labels",
        ),
        (json!({ "title": "x", "labels": ["a,b"] }), "invalid_label"),
        (
            json!({ "title": "x", "labels": ["x".repeat(33)] }),
            "invalid_label",
        ),
    ];
    for (body, code) in cases {
        let (s, c) = org.refused(Method::POST, "/tasks", body).await;
        assert_eq!((s, c.as_str()), (StatusCode::UNPROCESSABLE_ENTITY, code));
    }
    org.task(json!({ "title": "x".repeat(512) })).await;
    org.task(json!({ "title": "t" })).await;
    org.patch(2, json!({ "state": "blocked" })).await;
    let (s, c) = org
        .refused(
            Method::PATCH,
            "/tasks/2",
            json!({ "blocked_reason": "r".repeat(513) }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "reason_too_long")
    );
    // Links per task.
    org.seat("w").await;
    for i in 0..50 {
        let m = org
            .post(
                "/messages",
                json!({ "to": ["w"], "subject": format!("m{i}"), "body": "x" }),
            )
            .await;
        org.post("/tasks/2/links", json!({ "thread_id": m["thread_id"] }))
            .await;
    }
    let m = org
        .post(
            "/messages",
            json!({ "to": ["w"], "subject": "one more", "body": "x" }),
        )
        .await;
    let (s, c) = org
        .refused(
            Method::POST,
            "/tasks/2/links",
            json!({ "thread_id": m["thread_id"] }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "too_many_links")
    );
}

async fn storage_quota_and_request_limit(srv: Server) {
    let mut cfg = Config::default();
    cfg.limits.org_storage_bytes = Some(16 * 1024);
    cfg.limits.org_requests_per_sec = Some(2.0);
    let Some(small) = Server::start(srv.backend, "storage_quota_small", cfg).await else {
        return;
    };
    let org = small.org("alpha").await;
    let (s, c) = org
        .refused(
            Method::POST,
            "/tasks",
            json!({ "title": "big", "body": "x".repeat(20 * 1024) }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::INSUFFICIENT_STORAGE, "storage_quota")
    );
    let info = org.get("/org").await;
    assert_eq!(info["task_bytes"], 0);
    // Task calls count against the org's request limit (bursts of 4).
    let mut limited = false;
    for _ in 0..8 {
        let (s, _) = org.call(Method::GET, "/tasks", None).await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited);
}

async fn reminder_cap(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    org.seat("w2").await;
    org.task(json!({ "title": "ping-pong" })).await;
    for i in 0..205 {
        let who = if i % 2 == 0 { "w1" } else { "w2" };
        org.patch(1, json!({ "assignee": who })).await;
    }
    eventually("the notice to the user", || async {
        org.reminders()
            .await
            .iter()
            .any(|s| s == "Task reminders dropped")
    })
    .await;
    let n = org
        .reminders()
        .await
        .iter()
        .filter(|s| *s == "Task reminders dropped")
        .count();
    assert_eq!(n, 1, "one notice");
    let w1_count = w1.reminders().await.len();
    assert!(w1_count <= 103, "{w1_count}");
}

async fn work_stealing(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    let w3 = org.seat("w3").await;
    org.task(json!({ "title": "parser", "assignee": "w1" }))
        .await;
    eventually("assignment", || async { w1.reminders().await.len() == 1 }).await;
    // Unassigned and started tasks are refused.
    org.task(json!({ "title": "free" })).await;
    let (s, c) = w2.refused(Method::POST, "/tasks/2/steal", json!({})).await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "not_assigned")
    );
    // The request reaches the assignee, in To, with the two commands.
    let d = w2.post("/tasks/1/steal", json!({})).await;
    assert_eq!(d["task"]["steal_requested_by"], "w2");
    assert!(d["task"]["steal_requested_at"].is_string());
    eventually("the request", || async { w1.reminders().await.len() == 2 }).await;
    let inbox = w1.inbox().await;
    let last = inbox.last().unwrap();
    assert_eq!(
        last["message"]["subject"],
        "w2 asks to take over t1: parser"
    );
    assert_eq!(last["delivery"]["class"], "to");
    let id = last["message"]["id"].as_i64().unwrap();
    let body = w1.get(&format!("/messages/{id}?peek=1")).await["message"]["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        body.contains("amail task yield t1") && body.contains("amail task keep t1"),
        "{body}"
    );
    // `mine` shows the request.
    let mine = w1.get("/tasks/mine").await;
    assert_eq!(mine["tasks"][0]["steal_requested_by"], "w2");
    // A second requester waits; others cannot answer.
    let (s, c) = w3.refused(Method::POST, "/tasks/1/steal", json!({})).await;
    assert_eq!((s, c.as_str()), (StatusCode::CONFLICT, "steal_pending"));
    let (s, c) = w3.refused(Method::POST, "/tasks/1/yield", json!({})).await;
    assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "not_assignee"));
    let (s, _) = w2.refused(Method::POST, "/tasks/1/keep", json!({})).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Keep: the requester is told, once.
    w1.post("/tasks/1/keep", json!({})).await;
    eventually("the decline", || async {
        w2.reminders().await == ["t1 stays with w1: your request to take it over was declined"]
    })
    .await;
    // Yield: the requester gets the task and the assigned reminder.
    w3.post("/tasks/1/steal", json!({})).await;
    let d = w1.post("/tasks/1/yield", json!({})).await;
    assert_eq!(d["task"]["assignee"], "w3");
    let given = d["events"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|e| e["kind"] == "assigned")
        .unwrap()
        .clone();
    assert_eq!(given["note"], "yielded to w3 on request");
    assert_eq!(given["actor"], "w1");
    eventually("the hand-over", || async {
        w3.reminders().await == ["t1 assigned to you: parser"]
    })
    .await;
    // The user may answer too, or just assign (which ends the request).
    w2.post("/tasks/1/steal", json!({})).await;
    org.patch(1, json!({ "assignee": "w1" })).await;
    eventually("the cleared request", || async {
        w2.reminders().await.len() == 2
    })
    .await;
    assert_eq!(
        w2.reminders().await[1],
        "t1: your request to take it over ended: the task was reassigned"
    );
    // Started: refused.
    w1.patch(1, json!({ "state": "doing" })).await;
    let (s, c) = w2.refused(Method::POST, "/tasks/1/steal", json!({})).await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "started")
    );
    // Expiry after 24 hours (aged here): the requester is told once.
    w1.patch(1, json!({ "state": "open" })).await;
    w2.post("/tasks/1/steal", json!({})).await;
    srv.sql("UPDATE tasks SET steal_requested_at = 0 WHERE steal_requested_at IS NOT NULL")
        .await;
    eventually("the expiry", || async { w2.reminders().await.len() == 3 }).await;
    assert!(w2.reminders().await[2].contains("expired"));
    assert!(
        org.get("/tasks/1").await["task"]
            .get("steal_requested_by")
            .is_none()
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(w2.reminders().await.len(), 3);
    // Every mail went to the right seat once.
    // (assigned, w2's request, w3's request, assigned again, w2's request)
    assert_eq!(w1.reminders().await.len(), 5, "{:?}", w1.reminders().await);
}

// --- M8.1 -------------------------------------------------------------------

async fn revisions_and_client_keys(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let d = org
        .call(
            Method::POST,
            "/tasks",
            Some(json!({ "title": "spec", "assignee": "w1", "client_key": "k-1" })),
        )
        .await;
    assert_eq!(d.0, StatusCode::CREATED);
    let rev = d.1["task"]["rev"].as_i64().unwrap();
    // The same key again: the same task, 200, nothing created or mailed.
    let again = org
        .call(
            Method::POST,
            "/tasks",
            Some(json!({ "title": "spec", "assignee": "w1", "client_key": "k-1" })),
        )
        .await;
    assert_eq!(again.0, StatusCode::OK);
    assert_eq!(again.1["existing"], true);
    assert_eq!(again.1["task"]["num"], 1);
    assert_eq!(w1.reminders().await.len(), 1);
    let (s, c) = org
        .refused(
            Method::POST,
            "/tasks",
            json!({ "title": "x", "client_key": "k".repeat(65) }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_client_key")
    );
    // A write at the right revision passes; one at an old revision is
    // refused with the task and what changed since.
    let d = w1
        .patch(1, json!({ "title": "spec v2", "if_rev": rev }))
        .await;
    let rev2 = d["task"]["rev"].as_i64().unwrap();
    assert!(rev2 > rev);
    let (s, v) = org
        .call(
            Method::PATCH,
            "/tasks/1",
            Some(json!({ "body": "mine", "if_rev": rev })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "stale");
    assert_eq!(v["rev"], rev2);
    assert_eq!(v["task"]["task"]["title"], "spec v2");
    let since = v["since"].as_array().unwrap();
    assert_eq!(since[0]["field"], "title");
    assert_eq!(since[0]["to"], "spec v2");
    assert!(org.get("/tasks/1").await["task"].get("body").is_none());
    // `take` (body) and the DELETE calls (query) check it too.
    let t2 = org.task(json!({ "title": "pool" })).await;
    let (s, v) = w1
        .call(
            Method::POST,
            &format!("/tasks/{t2}/take"),
            Some(json!({ "if_rev": 0 })),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("stale"))
    );
    org.post(&format!("/tasks/{t2}/deps"), json!({ "blocked_by": 1 }))
        .await;
    let (s, _) = org
        .call(
            Method::DELETE,
            &format!("/tasks/{t2}/deps/1?if_rev=1"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // A body change records the whole text.
    w1.patch(1, json!({ "body": "line one\nline two" })).await;
    let evs = org.get("/tasks/1/events").await;
    let e = evs["events"]
        .as_array()
        .unwrap()
        .iter()
        .rfind(|e| e["field"] == "body")
        .unwrap()
        .clone();
    assert_eq!(e["to"], "line one\nline two");
    assert!(org.get("/tasks/1").await.get("events_compacted").is_none());
}

async fn children_results_and_reminders(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    let w3 = org.seat("w3").await;
    let parent = org.task(json!({ "title": "epic", "assignee": "w1" })).await;
    let c1 = org
        .task(json!({ "title": "part one", "parent": parent, "assignee": "w2" }))
        .await;
    let c2 = org
        .task(json!({ "title": "part two", "parent": parent, "assignee": "w2" }))
        .await;
    let d = org.get(&format!("/tasks/{parent}")).await;
    assert_eq!(d["task"]["progress"], json!({ "done": 0, "total": 2 }));
    // Closing the parent with open children: 409 naming them; force closes.
    let (s, v) = w1
        .call(
            Method::PATCH,
            &format!("/tasks/{parent}"),
            Some(json!({ "state": "done" })),
        )
        .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("children_open"))
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&format!("t{c1}"))
    );
    w2.patch(c1, json!({ "state": "done" })).await;
    assert!(
        !w1.reminders()
            .await
            .iter()
            .any(|r| r.contains("child tasks"))
    );
    w2.patch(c2, json!({ "state": "cancelled" })).await;
    let line = format!("t{parent}: all 2 child tasks are closed");
    assert_eq!(
        w1.reminders().await.iter().filter(|r| **r == line).count(),
        1,
        "{:?}",
        w1.reminders().await
    );
    assert_eq!(
        org.get(&format!("/tasks/{parent}")).await["task"]["progress"],
        json!({ "done": 1, "total": 1 })
    );
    // Results: only on a done task.
    let review = org
        .task(json!({ "title": "review", "assignee": "w2" }))
        .await;
    let integ = org
        .task(json!({ "title": "integrate", "assignee": "w3" }))
        .await;
    let d = org
        .post(
            &format!("/tasks/{integ}/deps"),
            json!({ "blocked_by": review, "needs_ok": true }),
        )
        .await;
    assert_eq!(d["blockers"][0]["needs_ok"], true);
    assert_eq!(d["task"]["waits_on_ok"], json!([review]));
    let (s, c) = w2
        .refused(
            Method::PATCH,
            &format!("/tasks/{review}"),
            json!({ "result": "ok" }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "not_done")
    );
    let (s, c) = w2
        .refused(
            Method::PATCH,
            &format!("/tasks/{review}"),
            json!({ "state": "done", "result": "ok", "result_ref": "x".repeat(201) }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "ref_too_long")
    );
    // Closed as rejected: w3 is told once; the task still waits.
    let d = w2
        .patch(
            review,
            json!({ "state": "done", "result": "rejected", "result_ref": "abc123", "result_note": "fails" }),
        )
        .await;
    assert_eq!(d["task"]["result"], "rejected");
    assert_eq!(d["task"]["result_by"], "w2");
    let rejected = format!("t{review} was closed as rejected; t{integ} waits for an ok result");
    let count = |v: &[String], s: &str| v.iter().filter(|r| r.as_str() == s).count();
    assert_eq!(count(&w3.reminders().await, &rejected), 1);
    assert_eq!(
        org.get(&format!("/tasks/{integ}")).await["task"]["state"],
        "open"
    );
    // A new ok result: w3 is told the task is no longer blocked.
    w2.patch(review, json!({ "result": "ok", "result_ref": "def456" }))
        .await;
    assert_eq!(
        org.get(&format!("/tasks/{integ}")).await["task"]["state"],
        "ready"
    );
    let unblocked = format!("t{integ} is no longer blocked: t{review} done with result ok");
    assert_eq!(count(&w3.reminders().await, &unblocked), 1);
    // Withdrawn: w3 is told once more.
    w2.patch(review, json!({ "result": null, "note": "wrong commit" }))
        .await;
    let withdrawn = format!("t{review}'s ok result was withdrawn; t{integ} waits for an ok result");
    assert_eq!(count(&w3.reminders().await, &withdrawn), 1);
    let evs = org.get(&format!("/tasks/{review}/events")).await;
    let last = evs["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["kind"], "result_withdrawn");
    assert_eq!(last["from"], "ok def456");
    // The list filter.
    let l = org.get("/tasks?state=all&result=none").await;
    assert!(nums(&l).contains(&integ) && nums(&l).contains(&review));
    w2.patch(review, json!({ "result": "rejected" })).await;
    assert_eq!(
        nums(&org.get("/tasks?state=all&result=rejected").await),
        [review]
    );
    assert!(nums(&org.get("/tasks?state=all&result=ok").await).is_empty());
}

async fn import_and_report(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let mut records = Vec::new();
    for i in 0..10 {
        records.push(json!({
            "id": format!("ISSUE-{i}"),
            "title": format!("issue {i}"),
            "state": if i < 2 { "done" } else if i == 2 { "ready" } else { "doing" },
            "assignee": if i == 3 { "nobody" } else { "w1" },
            "depends_on": if i == 5 { json!([{ "id": "ISSUE-0", "needs_ok": true }]) } else { json!([]) },
            "result": if i == 0 { json!({ "value": "rejected", "ref": "v1" }) } else { Value::Null },
            "history": [{ "at": "2026-10-20T10:00:00Z", "by": "old-lead", "text": format!("note {i}") }],
            "colour": "red",
        }));
    }
    records[9]["parent"] = json!("ISSUE-8");
    for r in records.iter_mut() {
        if r["result"].is_null() {
            r.as_object_mut().unwrap().remove("result");
        }
    }
    // Only the user imports.
    let (s, _) = w1
        .call(
            Method::POST,
            "/tasks/import",
            Some(json!({ "records": records })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let dry = org
        .post(
            "/tasks/import",
            json!({ "records": records, "dry_run": true }),
        )
        .await;
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["results"].as_array().unwrap().len(), 10);
    assert!(
        org.get("/tasks?state=all").await["tasks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let r = org
        .post("/tasks/import", json!({ "records": records }))
        .await;
    let res = r["results"].as_array().unwrap();
    assert!(res.iter().all(|x| x["action"] == "created"), "{r}");
    let warnings: Vec<&str> = r["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert!(
        warnings.contains(&"ISSUE-3: no seat \"nobody\"; the task is left unassigned"),
        "{warnings:?}"
    );
    assert!(
        warnings.contains(&"ISSUE-0: unknown field \"colour\" ignored"),
        "{warnings:?}"
    );
    // Nothing is mailed.
    assert!(w1.reminders().await.is_empty());
    let again = org
        .post("/tasks/import", json!({ "records": records }))
        .await;
    assert!(
        again["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["action"] == "same"),
        "{again}"
    );
    assert_eq!(
        org.get("/tasks?state=all").await["tasks"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    let found = org.get("/tasks?import_key=ISSUE-0").await;
    assert_eq!(found["tasks"][0]["import_key"], "ISSUE-0");
    assert_eq!(found["tasks"][0]["result"], "rejected");
    // Refusals: a missing title, an id used twice.
    let (s, c) = org
        .refused(
            Method::POST,
            "/tasks/import",
            json!({ "records": [{ "id": "A" }] }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_import")
    );
    let (_, c) = org
        .refused(
            Method::POST,
            "/tasks/import",
            json!({ "records": [{ "id": "A", "title": "a" }, { "id": "A", "title": "b" }] }),
        )
        .await;
    assert_eq!(c, "invalid_import");
    // The report.
    let rep = org.get("/tasks/report").await;
    let sec = |name: &str| {
        rep["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(sec("rejected")["count"], 1);
    assert_eq!(sec("in_progress")["count"], 7);
    assert_eq!(sec("not_started")["count"], 1);
    assert!(
        sec("in_progress")["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["seat"] == "w1")
    );
    assert_eq!(sec("blocked")["count"], 0);
    assert_eq!(sec("parents")["count"], 1);
}
