//! Requests for the person over HTTP (M10 phase A): creation and the
//! format rules, the mail to the user and its urgency by class, answers
//! with `if_rev` and `answered`, reply-as-answer and comments, withdrawal,
//! the deadline pass with and without defaults and the batched reminder,
//! the long poll, attention requests from presence and their closing, the
//! task marker `blocked-on-human`, the `[r12]` auto-link, `request`
//! events, the seat prompt and interrupt routes, and the public limits.
//! Both backends (Postgres only when `AMAIL_TEST_PG_URL` is set).

mod common;

use amail_server::config::{Config, RequestLimits};
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

macro_rules! request_tests {
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

request_tests!(
    create_mail_and_rules,
    answers_and_if_rev,
    reply_as_answer_and_comments,
    withdraw,
    deadline_pass_and_reminders,
    long_poll,
    attention_from_presence,
    tasks_blocked_on_human,
    subject_links_and_events,
    prompt_and_interrupt,
    public_limits,
);

struct Server {
    base: String,
    #[allow(dead_code)]
    db: TestDb,
    http: reqwest::Client,
    backend: Backend,
    state: AppState,
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
        // The reminder pass runs only when a test asks for it.
        state.attention.task_scan_every = Duration::from_secs(3600);
        state.set_config(cfg);
        tokio::spawn(amail_server::serve(
            listener,
            state.clone(),
            std::future::pending(),
        ));
        Some(Server {
            base,
            db,
            backend,
            state,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
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
    /// The caller's inbox entries, oldest first.
    async fn inbox(&self) -> Vec<Value> {
        self.get("/inbox").await["entries"]
            .as_array()
            .unwrap()
            .clone()
    }
    /// The inbox entry whose subject is `subject`, with its body.
    async fn mail(&self, subject: &str) -> (Value, String) {
        let e = self
            .inbox()
            .await
            .into_iter()
            .find(|e| e["message"]["subject"] == subject)
            .unwrap_or_else(|| panic!("no mail {subject:?}"));
        let id = e["message"]["id"].as_i64().unwrap();
        let m = self.get(&format!("/messages/{id}")).await;
        (e, m["message"]["body"].as_str().unwrap().to_string())
    }
    async fn subjects(&self) -> Vec<String> {
        self.inbox()
            .await
            .iter()
            .map(|e| e["message"]["subject"].as_str().unwrap().to_string())
            .collect()
    }
    async fn stream(&self, path: &str) -> Sse {
        let r = common::open_stream(
            common::stream_client()
                .get(format!("{}/v1{path}", self.base))
                .bearer_auth(&self.token),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK, "GET {path}");
        Sse {
            stream: r.bytes_stream().map(|c| c.map(|b| b.to_vec())).boxed(),
            buf: String::new(),
        }
    }
}

/// Reads server-sent events as `(event, data)`.
struct Sse {
    stream: futures::stream::BoxStream<'static, reqwest::Result<Vec<u8>>>,
    buf: String,
}

impl Sse {
    /// The next event whose name is in `names`, within `secs`.
    async fn next_of(&mut self, names: &[&str], secs: u64) -> Option<(String, Value)> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
        loop {
            while let Some(i) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..i + 2).collect();
                let (mut event, mut data) = (String::from("message"), None);
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data = serde_json::from_str(v.trim()).ok();
                    }
                }
                if let Some(d) = data
                    && names.contains(&event.as_str())
                {
                    return Some((event, d));
                }
            }
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Ok(Some(Ok(chunk))) => self.buf.push_str(&String::from_utf8_lossy(&chunk)),
                _ => return None,
            }
        }
    }
}

fn question(title: &str) -> Value {
    json!({
        "title": title,
        "body": "Release notes are in d3.",
        "options": [
            { "key": "yes", "label": "Ship it", "description": "tag and push" },
            { "key": "no", "label": "Hold it" }
        ]
    })
}

async fn create_mail_and_rules(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let v = a.post("/requests", question("Ship v2?")).await;
    let r = &v["request"];
    assert_eq!(r["num"], 1);
    assert_eq!(r["kind"], "question");
    assert_eq!(r["seat"], "a");
    assert_eq!(r["asker"], "a");
    assert_eq!(r["class"], "soon");
    assert_eq!(r["state"], "open");
    assert_eq!(r["source"], "cli");
    assert_eq!(r["rev"], 1);
    assert!(r["deadline"].is_string());
    assert_eq!(v["events"][0]["kind"], "created");
    // The mail to the user: from the seat, `To` the user, normal for
    // `soon`, the options as numbered lines.
    let (e, body) = org.mail("[r1] Ship v2?").await;
    assert_eq!(e["delivery"]["class"], "to");
    assert_eq!(e["message"]["priority"], "normal");
    assert!(
        e["message"]["from"].as_str().unwrap().starts_with('a'),
        "{e}"
    );
    assert!(body.starts_with("Release notes are in d3."), "{body}");
    assert!(
        body.contains("1. yes — Ship it\n   tag and push\n2. no — Hold it\n"),
        "{body}"
    );
    assert!(body.contains("amail answer r1 <option>"), "{body}");
    let thread = e["message"]["thread_id"].as_i64().unwrap();
    let got = org.get("/requests/r1").await;
    assert_eq!(got["request"]["thread"], thread);
    // Class `now`: urgent mail; a task of the org; a default.
    let mut q = question("Deploy now?");
    q["class"] = json!("now");
    q["default"] = json!("no");
    q["source"] = json!("mcp");
    let v = a.post("/requests", q).await;
    assert_eq!(v["request"]["default"], "no");
    assert_eq!(v["request"]["source"], "mcp");
    let (e, body) = org.mail("[r2] Deploy now?").await;
    assert_eq!(e["message"]["priority"], "urgent");
    assert!(body.contains("Default: 2. no — Hold it"), "{body}");
    // Free text.
    let v = a
        .post(
            "/requests",
            json!({ "title": "Name the release", "free_text": true, "class": "later" }),
        )
        .await;
    assert_eq!(v["request"]["free_text"], true);
    let (_, body) = org.mail("[r3] Name the release").await;
    assert!(body.contains("Answer with free text."), "{body}");

    // The format rules.
    let mut bad = question("x");
    bad["options"] = json!([{ "key": "only", "label": "One" }]);
    let (s, v) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "options_count");
    let mut bad = question("x");
    bad["options"][0]["key"] = json!("has space");
    let (_, v) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(v["error"]["code"], "invalid_option_key");
    let (_, v) = a
        .refused(Method::POST, "/requests", Some(question(&"t".repeat(513))))
        .await;
    assert_eq!(v["error"]["code"], "title_too_long");
    let mut bad = question("x");
    bad["body"] = json!("b".repeat(64 * 1024 + 1));
    let (_, v) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(v["error"]["code"], "body_too_large");
    let mut bad = question("x");
    bad["default"] = json!("maybe");
    let (_, v) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(v["error"]["code"], "invalid_default");
    let mut bad = question("x");
    bad["task"] = json!("t9");
    let (s, _) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Kinds: permission is not built yet; attention comes from nodes.
    let mut bad = question("x");
    bad["kind"] = json!("permission");
    let (_, v) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(v["error"]["code"], "kind_not_supported");
    let mut bad = question("x");
    bad["kind"] = json!("attention");
    let (_, v) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(v["error"]["code"], "kind_not_allowed");
    let node = org.node().await;
    let mut bad = question("x");
    bad["seat"] = json!("a");
    let (_, v) = node.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(v["error"]["code"], "kind_not_allowed");
    // A seat asks for itself; the user names an agent seat.
    let mut bad = question("x");
    bad["seat"] = json!("b");
    let (s, _) = a.refused(Method::POST, "/requests", Some(bad)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = org
        .refused(Method::POST, "/requests", Some(question("x")))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let mut q = question("x");
    q["seat"] = json!("owner");
    let (_, v) = org.refused(Method::POST, "/requests", Some(q)).await;
    assert_eq!(v["error"]["code"], "not_an_agent");
    // The user asks about a seat: mailed from the mail system.
    let mut q = question("Restart b?");
    q["seat"] = json!("b");
    q["kind"] = json!("plan");
    let v = org.post("/requests", q).await;
    assert_eq!(v["request"]["seat"], "b");
    assert_eq!(v["request"]["asker"], "owner");
    let (e, _) = org.mail("[r4] Restart b?").await;
    assert!(
        e["message"]["from"].as_str().unwrap().starts_with("amail"),
        "{e}"
    );

    // Who sees what: the user all, a seat its own.
    assert_eq!(
        org.get("/requests").await["requests"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let mine = b.get("/requests").await;
    assert_eq!(mine["requests"].as_array().unwrap().len(), 1);
    assert_eq!(mine["requests"][0]["num"], 4);
    let (s, _) = b.refused(Method::GET, "/requests/1", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    a.get("/requests/1").await;
    // Listing order: `now` first; filters.
    let v = org.get("/requests").await;
    let nums: Vec<i64> = v["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["num"].as_i64().unwrap())
        .collect();
    assert_eq!(nums, vec![2, 1, 4, 3]);
    let v = org.get("/requests?kind=plan").await;
    assert_eq!(v["requests"].as_array().unwrap().len(), 1);
    let v = org.get("/requests?seat=a&class=later").await;
    assert_eq!(v["requests"][0]["num"], 3);
    let v = org.get("/requests?state=answered").await;
    assert!(v["requests"].as_array().unwrap().is_empty());
    let (s, _) = org
        .refused(Method::GET, "/requests?state=bogus", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

async fn answers_and_if_rev(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    a.post("/requests", question("Ship v2?")).await;
    // Only the user answers.
    let (s, _) = a
        .refused(
            Method::POST,
            "/requests/1/answer",
            Some(json!({ "option": "yes" })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Wrong revision: 409 stale with the request.
    let (s, v) = org
        .refused(
            Method::POST,
            "/requests/r1/answer",
            Some(json!({ "option": "yes", "if_rev": 7 })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "stale");
    assert_eq!(v["request"]["rev"], 1);
    // Not an option; free text where options are wanted.
    let (s, v) = org
        .refused(
            Method::POST,
            "/requests/1/answer",
            Some(json!({ "option": "maybe" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "invalid_option");
    let (_, v) = org
        .refused(
            Method::POST,
            "/requests/1/answer",
            Some(json!({ "text": "whatever you think" })),
        )
        .await;
    assert_eq!(v["error"]["code"], "invalid_option");
    // By number, with a note, at the right revision.
    let v = org
        .post(
            "/requests/1/answer",
            json!({ "option": "2", "note": "after the review", "if_rev": 1 }),
        )
        .await;
    let r = &v["request"];
    assert_eq!(r["state"], "answered");
    assert_eq!(r["answer"]["option"], "no");
    assert_eq!(r["answer"]["by"], "owner");
    assert_eq!(r["answer"]["note"], "after the review");
    assert_eq!(r["rev"], 2);
    // The seat gets the answer in the request's thread, `To` it.
    let (e, body) = a.mail("Re: [r1] Ship v2?").await;
    assert_eq!(e["delivery"]["class"], "to");
    assert_eq!(e["message"]["thread_id"], r["thread"]);
    assert!(body.contains("Answer: 2. no — Hold it"), "{body}");
    assert!(body.contains("Note: after the review"), "{body}");
    // A second answer: 409 answered with the first answer.
    let (s, v) = org
        .refused(
            Method::POST,
            "/requests/1/answer",
            Some(json!({ "option": "yes" })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "answered");
    assert_eq!(v["request"]["answer"]["option"], "no");
    // Text that names an option is the option; free text.
    a.post("/requests", question("Again?")).await;
    let v = org
        .post("/requests/2/answer", json!({ "text": "Ship it" }))
        .await;
    assert_eq!(v["request"]["answer"]["option"], "yes");
    a.post(
        "/requests",
        json!({ "title": "Name it", "free_text": true }),
    )
    .await;
    let v = org
        .post("/requests/3/answer", json!({ "text": "lark" }))
        .await;
    assert_eq!(v["request"]["answer"]["text"], "lark");
    let (_, body) = a.mail("Re: [r3] Name it").await;
    assert!(body.contains("Answer: lark"), "{body}");
    let (s, _) = org
        .refused(
            Method::POST,
            "/requests/9/answer",
            Some(json!({ "option": "yes" })),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

async fn reply_as_answer_and_comments(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    a.post("/requests", question("Ship v2?")).await;
    let (e, _) = org.mail("[r1] Ship v2?").await;
    let first = e["message"]["id"].as_i64().unwrap();
    // A comment: the first line names no option. It reaches the seat (the
    // reply is `To` it) and the request stays open.
    org.post(
        "/messages",
        json!({ "to": ["a"], "subject": "Re: [r1] Ship v2?", "body": "What does d3 say about it?", "in_reply_to": first }),
    )
    .await;
    let v = org.get("/requests/1").await;
    assert_eq!(v["request"]["state"], "open");
    let evs: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(evs, vec!["created", "commented"]);
    // A comment not addressed to the seat is mailed to it.
    let before = a.inbox().await.len();
    org.post(
        "/messages",
        json!({ "to": ["b"], "subject": "Re: [r1] Ship v2?", "body": "b, what do you think?", "in_reply_to": first }),
    )
    .await;
    let after = a.inbox().await;
    assert_eq!(after.len(), before + 1);
    let last = after.last().unwrap();
    let id = last["message"]["id"].as_i64().unwrap();
    let body = a.get(&format!("/messages/{id}")).await["message"]["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(body.contains("owner commented on r1"), "{body}");
    assert!(body.contains("b, what do you think?"), "{body}");
    // A seat's reply is a comment too, never an answer.
    b.post(
        "/messages",
        json!({ "to": ["owner"], "subject": "Re: [r1] Ship v2?", "body": "1", "in_reply_to": first }),
    )
    .await;
    assert_eq!(org.get("/requests/1").await["request"]["state"], "open");
    // The answer: the option's label on the first line (after quoted
    // lines), `To` the seat: no second mail to it.
    let before = a.inbox().await.len();
    org.post(
        "/messages",
        json!({ "to": ["a"], "subject": "Re: [r1] Ship v2?", "body": "> Which way?\n\nship it\nand tag it v2.0", "in_reply_to": first }),
    )
    .await;
    let v = org.get("/requests/1").await;
    assert_eq!(v["request"]["state"], "answered");
    assert_eq!(v["request"]["answer"]["option"], "yes");
    assert_eq!(v["request"]["answer"]["by"], "owner");
    let ev = v["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(ev["kind"], "answered");
    assert!(ev["message_id"].is_i64());
    assert_eq!(a.inbox().await.len(), before + 1);
    // Later replies change nothing.
    org.post(
        "/messages",
        json!({ "to": ["a"], "subject": "Re: [r1] Ship v2?", "body": "2", "in_reply_to": first }),
    )
    .await;
    assert_eq!(
        org.get("/requests/1").await["request"]["answer"]["option"],
        "yes"
    );
    // An answer by reply that did not reach the seat in `To` is mailed.
    a.post("/requests", question("Second?")).await;
    let (e, _) = org.mail("[r2] Second?").await;
    let first = e["message"]["id"].as_i64().unwrap();
    org.post(
        "/messages",
        json!({ "to": ["b"], "subject": "Re: [r2] Second?", "body": "2.", "in_reply_to": first }),
    )
    .await;
    assert_eq!(
        org.get("/requests/2").await["request"]["answer"]["option"],
        "no"
    );
    let (_, body) = a.mail("Re: [r2] Second?").await;
    assert!(body.contains("Answer: 2. no — Hold it"), "{body}");
    // A free-text question: the reply's text is the answer.
    a.post(
        "/requests",
        json!({ "title": "Name it", "free_text": true }),
    )
    .await;
    let (e, _) = org.mail("[r3] Name it").await;
    let first = e["message"]["id"].as_i64().unwrap();
    org.post(
        "/messages",
        json!({ "to": ["a"], "subject": "Re: [r3] Name it", "body": "Call it lark.", "in_reply_to": first }),
    )
    .await;
    assert_eq!(
        org.get("/requests/3").await["request"]["answer"]["text"],
        "Call it lark."
    );
}

async fn withdraw(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    a.post("/requests", question("Ship v2?")).await;
    let (s, _) = b.refused(Method::POST, "/requests/1/withdraw", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let v = a
        .post("/requests/1/withdraw", json!({ "note": "found it in d3" }))
        .await;
    assert_eq!(v["request"]["state"], "withdrawn");
    // The user is told in the thread.
    let subjects = org.subjects().await;
    assert!(
        subjects.contains(&"Re: [r1] Ship v2?".to_string()),
        "{subjects:?}"
    );
    let (_, body) = org.mail("Re: [r1] Ship v2?").await;
    assert!(body.contains("a withdrew r1"), "{body}");
    assert!(body.contains("Note: found it in d3"), "{body}");
    let (s, v) = a.refused(Method::POST, "/requests/1/withdraw", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "not_open");
    assert_eq!(v["request"]["state"], "withdrawn");
    let (s, v) = org
        .refused(
            Method::POST,
            "/requests/1/answer",
            Some(json!({ "option": "yes" })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "not_open");
    // The user withdraws: the seat is told.
    a.post("/requests", question("Second?")).await;
    org.post("/requests/2/withdraw", json!({})).await;
    let (_, body) = a.mail("Re: [r2] Second?").await;
    assert!(body.contains("owner withdrew r2"), "{body}");
}

async fn deadline_pass_and_reminders(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    // A default and a deadline now.
    let mut q = question("Ship v2?");
    q["default"] = json!("no");
    q["deadline_s"] = json!(0);
    a.post("/requests", q).await;
    // No default: reminded to the user, with a task that is past due in
    // the same mail.
    let mut q = question("Rename the crate?");
    q["deadline_s"] = json!(0);
    q["class"] = json!("now");
    b.post("/requests", q).await;
    // Not due.
    let mut q = question("Later");
    q["default"] = json!("yes");
    a.post("/requests", q).await;
    a.post(
        "/tasks",
        json!({ "title": "review the plan", "assignee": "owner", "due": "2020-01-01T00:00:00Z" }),
    )
    .await;
    let mut events = org.stream("/events").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let before = org.inbox().await.len();
    amail_server::tasks::scan(&srv.state).await.unwrap();
    // The default was taken and mailed to the seat.
    let v = org.get("/requests/1").await;
    assert_eq!(v["request"]["state"], "expired");
    assert_eq!(v["request"]["answer"]["option"], "no");
    assert_eq!(v["request"]["answer"]["by"], "deadline");
    let (e, body) = a.mail("Re: [r1] Ship v2?").await;
    assert_eq!(e["delivery"]["class"], "to");
    assert!(body.contains("took its default"), "{body}");
    assert!(body.contains("Answer: 2. no — Hold it"), "{body}");
    // One reminder mail to the user with the request and the task.
    let inbox = org.inbox().await;
    assert_eq!(inbox.len(), before + 1, "{inbox:?}");
    let id = inbox.last().unwrap()["message"]["id"].as_i64().unwrap();
    let body = org.get(&format!("/messages/{id}")).await["message"]["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(body.contains("r2 waits for your answer (now"), "{body}");
    assert!(body.contains("t1 is past its due time"), "{body}");
    assert!(body.contains("amail answer r2 <option>"), "{body}");
    assert_eq!(org.get("/requests/2").await["request"]["state"], "open");
    assert_eq!(org.get("/requests/3").await["request"]["state"], "open");
    // `request` events on the user's stream.
    let mut seen = Vec::new();
    while let Some((_, d)) = events.next_of(&["request"], 2).await {
        seen.push((
            d["num"].as_i64().unwrap(),
            d["change"].as_str().unwrap().to_string(),
        ));
    }
    assert!(seen.contains(&(1, "expired".into())), "{seen:?}");
    assert!(seen.contains(&(2, "reminded".into())), "{seen:?}");
    // The next pass sends nothing (the next reminder is an hour away).
    let n = org.inbox().await.len();
    amail_server::tasks::scan(&srv.state).await.unwrap();
    assert_eq!(org.inbox().await.len(), n);
}

async fn long_poll(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    a.post("/requests", question("Ship v2?")).await;
    // Times out while open.
    let t = std::time::Instant::now();
    let v = a.get("/requests/1/wait?timeout_s=1").await;
    assert_eq!(v["done"], false);
    assert_eq!(v["request"]["state"], "open");
    assert!(t.elapsed() >= Duration::from_millis(900));
    // Another seat cannot wait for it.
    let (s, _) = b
        .refused(Method::GET, "/requests/1/wait?timeout_s=1", None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Answered meanwhile: the wait ends at once.
    let waiter = {
        let a = a.clone();
        tokio::spawn(async move { a.get("/requests/1/wait?timeout_s=30").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t = std::time::Instant::now();
    org.post("/requests/1/answer", json!({ "option": "yes" }))
        .await;
    let v = waiter.await.unwrap();
    assert!(t.elapsed() < Duration::from_secs(5));
    assert_eq!(v["done"], true);
    assert_eq!(v["request"]["answer"]["option"], "yes");
    // Not open: answered at once; a node may wait too.
    let node = org.node().await;
    let v = node.get("/requests/1/wait?timeout_s=600").await;
    assert_eq!(v["done"], true);
}

async fn attention_from_presence(srv: Server) {
    let org = srv.org("alpha").await;
    let _a = org.seat("a").await;
    let _b = org.seat("b").await;
    let node = org.node().await;
    let mut cmds = node.stream("/events").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let report = |att: Option<&str>| {
        let mut a = json!({ "seat": "a", "state": "offline", "terminal_url": "http://127.0.0.1:9/amail-alpha-a" });
        if let Some(c) = att {
            a["attention"] = json!(c);
            a["attention_hint"] = json!("Do you trust the files in this folder?");
        }
        json!({ "node": "h1", "seats": [a] })
    };
    node.post("/presence", report(Some("trust-prompt"))).await;
    let v = org.get("/requests?kind=attention").await;
    let rs = v["requests"].as_array().unwrap();
    assert_eq!(rs.len(), 1);
    let r = &rs[0];
    assert_eq!(r["seat"], "a");
    assert_eq!(r["attention"], "trust-prompt");
    assert_eq!(r["class"], "now");
    assert_eq!(r["source"], "node");
    assert!(r.get("asker").is_none());
    assert!(r.get("deadline").is_none());
    assert_eq!(r["body"], "Do you trust the files in this folder?");
    let keys: Vec<&str> = r["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["restart", "sleep", "open_terminal", "dismiss"]);
    // No mail for attention requests.
    assert!(org.inbox().await.is_empty());
    // `GET /attention`: the request first, the seat entry after it.
    let v = org.get("/attention").await;
    let items = v["items"].as_array().unwrap();
    assert_eq!(items[0]["kind"], "request");
    assert_eq!(items[0]["request"]["num"], 1);
    assert!(items.iter().any(|i| i["kind"] == "seat"));
    // The same report again: no second request.
    node.post("/presence", report(Some("trust-prompt"))).await;
    assert_eq!(
        org.get("/requests?kind=attention&state=all").await["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // The condition is gone: withdrawn.
    node.post("/presence", report(None)).await;
    let v = org.get("/requests/1").await;
    assert_eq!(v["request"]["state"], "withdrawn");
    assert_eq!(
        v["events"].as_array().unwrap().last().unwrap()["actor"],
        "node"
    );
    // It comes back: a new request; a failed turn is a condition too.
    node.post("/presence", report(Some("trust-prompt"))).await;
    node.post(
        "/presence",
        json!({ "node": "h1", "seats": [{ "seat": "b", "state": "idle", "turn_failed": { "reason": "rate limited", "at": "2026-10-29T10:00:00Z", "count": 3, "retrying": false } }] }),
    )
    .await;
    let v = org.get("/requests?kind=attention").await;
    let open: Vec<(i64, String)> = v["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["num"].as_i64().unwrap(),
                r["attention"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        open,
        vec![(2, "trust-prompt".into()), (3, "turn-failed".into())]
    );
    // A node may also make one itself; the live one is answered.
    let v = node
        .post(
            "/requests",
            json!({ "kind": "attention", "seat": "a", "attention": "trust-prompt", "title": "ignored" }),
        )
        .await;
    assert_eq!(v["existing"], true);
    assert_eq!(v["request"]["num"], 2);
    // Answering runs the action through the node.
    let v = org
        .post("/requests/2/answer", json!({ "option": "sleep" }))
        .await;
    assert_eq!(v["request"]["state"], "answered");
    let (_, c) = cmds.next_of(&["command"], 5).await.unwrap();
    assert_eq!(
        c,
        json!({ "op": "attention_action", "seat": "a", "node": "h1", "request": 2, "action": "sleep" })
    );
    // While the condition lasts, no new request; `open_terminal` gives the
    // seat's terminal link.
    node.post("/presence", report(Some("trust-prompt"))).await;
    assert_eq!(
        org.get("/requests?kind=attention").await["requests"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let v = org
        .post("/requests/3/answer", json!({ "option": "open_terminal" }))
        .await;
    assert!(v.get("terminal_url").is_none(), "b has no terminal: {v}");
    node.post("/presence", report(None)).await;
    node.post("/presence", report(Some("login"))).await;
    let v = org.get("/requests?kind=attention").await;
    let r = &v["requests"][0];
    assert_eq!(r["attention"], "login");
    let num = r["num"].as_i64().unwrap();
    let v = org
        .post(
            &format!("/requests/{num}/answer"),
            json!({ "option": "open_terminal" }),
        )
        .await;
    assert_eq!(v["terminal_url"], "http://127.0.0.1:9/amail-alpha-a");
    // `dismiss` sends nothing.
    assert!(cmds.next_of(&["command"], 1).await.is_none());
}

async fn tasks_blocked_on_human(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    a.post("/tasks", json!({ "title": "pool work" })).await;
    a.post("/tasks", json!({ "title": "a's work", "assignee": "a" }))
        .await;
    let mut q = question("Which design?");
    q["task"] = json!("t1");
    q["kind"] = json!("plan");
    a.post("/requests", q).await;
    let mut q = question("Which API?");
    q["task"] = json!(2);
    a.post("/requests", q).await;
    let t = a.get("/tasks/1").await;
    assert_eq!(t["task"]["blocked_on_human"], 1);
    let list = org.get("/tasks").await;
    assert!(
        list["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["blocked_on_human"].is_i64()),
        "{list}"
    );
    assert_eq!(org.get("/requests/1").await["request"]["task"], 1);
    // Take and steal skip it; so does `next`.
    let (s, v) = b.refused(Method::POST, "/tasks/1/take", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "blocked_on_human");
    let (s, v) = b.refused(Method::POST, "/tasks/2/steal", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "blocked_on_human");
    let next = a.get("/tasks/next?pool=1").await;
    assert!(next["tasks"].as_array().unwrap().is_empty(), "{next}");
    // Answered: the task is free again.
    org.post("/requests/1/answer", json!({ "option": "yes" }))
        .await;
    let t = a.get("/tasks/1").await;
    assert!(t["task"].get("blocked_on_human").is_none(), "{t}");
    b.post("/tasks/1/take", json!({})).await;
    org.post("/requests/2/withdraw", json!({})).await;
    b.post("/tasks/2/steal", json!({})).await;
}

async fn subject_links_and_events(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    let mut events = org.stream("/events").await;
    let mut seat_events = a.stream("/events").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    a.post("/requests", question("Ship v2?")).await;
    let (_, d) = events.next_of(&["request"], 5).await.unwrap();
    assert_eq!(
        d,
        json!({ "num": 1, "change": "created", "state": "open", "seat": "a" })
    );
    // Seats get no `request` events.
    assert!(seat_events.next_of(&["request"], 1).await.is_none());
    // `[r1]` in a subject links the thread.
    let sent = b
        .post(
            "/messages",
            json!({ "to": ["a"], "subject": "[r1] the numbers for it", "body": "see attached" }),
        )
        .await;
    let thread = sent["thread_id"].as_i64().unwrap();
    let v = org.get("/requests/1").await;
    assert_eq!(v["threads"], json!([thread]));
    let ev = v["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(ev["kind"], "linked");
    assert_eq!(ev["actor"], "b");
    let (_, d) = events.next_of(&["request"], 5).await.unwrap();
    assert_eq!(d["change"], "linked");
    // Its own thread is not linked again; an unknown number is ignored.
    let (e, _) = org.mail("[r1] Ship v2?").await;
    let first = e["message"]["id"].as_i64().unwrap();
    a.post(
        "/messages",
        json!({ "to": ["owner"], "subject": "Re: [r1] Ship v2? [r9]", "body": "more detail", "in_reply_to": first }),
    )
    .await;
    assert_eq!(org.get("/requests/1").await["threads"], json!([thread]));
}

async fn prompt_and_interrupt(srv: Server) {
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    let _b = org.seat("b").await;
    // No node: 409 no_node.
    let (s, v) = org
        .refused(
            Method::POST,
            "/seats/a/prompt",
            Some(json!({ "text": "hi" })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "no_node");
    let node = org.node().await;
    let mut cmds = node.stream("/events").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Connected, but the seat is not reported: still no node for it.
    let (s, v) = org.refused(Method::POST, "/seats/a/interrupt", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "no_node");
    node.post(
        "/presence",
        json!({ "node": "h1", "seats": [
            { "seat": "a", "state": "busy", "harness": "codex" },
            { "seat": "b", "state": "busy", "harness": "claude" }
        ] }),
    )
    .await;
    // Only the user; the text is checked.
    let (s, _) = a
        .refused(
            Method::POST,
            "/seats/a/prompt",
            Some(json!({ "text": "hi" })),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = org
        .refused(
            Method::POST,
            "/seats/a/prompt",
            Some(json!({ "text": "  " })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "prompt_empty");
    let (_, v) = org
        .refused(
            Method::POST,
            "/seats/a/prompt",
            Some(json!({ "text": "x".repeat(64 * 1024 + 1) })),
        )
        .await;
    assert_eq!(v["error"]["code"], "prompt_too_large");
    let (s, v) = org
        .refused(
            Method::POST,
            "/seats/owner/prompt",
            Some(json!({ "text": "hi" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "not_an_agent");
    // The prompt: queued, recorded, sent to the node.
    let v = org
        .post("/seats/a/prompt", json!({ "text": "Run the tests again." }))
        .await;
    assert_eq!(v, json!({ "queued": true, "seat": "a", "nodes": 1 }));
    let (_, c) = cmds.next_of(&["command"], 5).await.unwrap();
    assert_eq!(
        c,
        json!({ "op": "prompt", "seat": "a", "node": "h1", "text": "Run the tests again." })
    );
    let log = org.get("/seats/a/worklog?kind=prompt_from_user").await;
    let e = &log["entries"][0];
    assert_eq!(e["kind"], "prompt_from_user");
    assert!(
        e["text"].as_str().unwrap().contains("Run the tests again."),
        "{e}"
    );
    // The mail archive keeps it; the seat is not notified.
    let arch = org.get("/archive/messages").await;
    let m = arch["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["subject"] == "Prompt from owner to a")
        .cloned()
        .unwrap_or_else(|| panic!("{arch}"));
    assert!(m["to"].as_array().unwrap().is_empty());
    let pending = node.get("/pending?seat=a").await;
    assert_eq!(pending["seats"][0]["to"], 0, "{pending}");
    assert_eq!(pending["seats"][0]["cc"], 0, "{pending}");
    // Interrupt: Codex has one; Claude Code does not.
    let v = org.post("/seats/a/interrupt", json!({})).await;
    assert_eq!(v["queued"], true);
    let (_, c) = cmds.next_of(&["command"], 5).await.unwrap();
    assert_eq!(c, json!({ "op": "interrupt", "seat": "a", "node": "h1" }));
    let log = org.get("/seats/a/worklog?kind=interrupt").await;
    assert_eq!(log["entries"].as_array().unwrap().len(), 1);
    let (s, v) = org.refused(Method::POST, "/seats/b/interrupt", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "no_structured_interrupt");
    // A prompt to a Claude Code seat goes through.
    org.post("/seats/b/prompt", json!({ "text": "status?" }))
        .await;
    let (_, c) = cmds.next_of(&["command"], 5).await.unwrap();
    assert_eq!(c["seat"], "b");
    // The node went away.
    drop(cmds);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (s, v) = org
        .refused(
            Method::POST,
            "/seats/a/prompt",
            Some(json!({ "text": "hi" })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "no_node");
}

async fn public_limits(srv: Server) {
    // Self-hosted: no limit beyond the storage quota.
    let org = srv.org("alpha").await;
    let a = org.seat("a").await;
    for i in 0..5 {
        a.post("/requests", question(&format!("q{i}"))).await;
    }
    assert_eq!(Config::default().limits.requests, RequestLimits::default());
    assert_eq!(
        Config::public().limits.requests,
        RequestLimits {
            open_per_org: Some(10_000),
            per_seat_per_hour: Some(600)
        }
    );
    let mut cfg = Config::default();
    cfg.limits.requests = RequestLimits {
        open_per_org: Some(3),
        per_seat_per_hour: Some(2),
    };
    let srv2 = Server::start(srv.backend, "request_public_limits_2", cfg)
        .await
        .unwrap();
    let org = srv2.org("alpha").await;
    let a = org.seat("a").await;
    let b = org.seat("b").await;
    a.post("/requests", question("1")).await;
    a.post("/requests", question("2")).await;
    let (s, v) = a
        .refused(Method::POST, "/requests", Some(question("3")))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "request_rate");
    b.post("/requests", question("3")).await;
    let (s, v) = b
        .refused(Method::POST, "/requests", Some(question("4")))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "too_many_open_requests");
    // The user is not counted per hour, but open requests are.
    let mut q = question("5");
    q["seat"] = json!("a");
    let (_, v) = org
        .refused(Method::POST, "/requests", Some(q.clone()))
        .await;
    assert_eq!(v["error"]["code"], "too_many_open_requests");
    org.post("/requests/1/answer", json!({ "option": "yes" }))
        .await;
    org.post("/requests", q).await;
}
