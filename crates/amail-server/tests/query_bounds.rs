//! Bounds on queries and responses: search and archive start from the org's
//! own messages and look at a bounded window per request; thread answers
//! carry a bounded amount of body text; listing pages are bounded in bytes.
//! Both backends (Postgres only when `AMAIL_TEST_PG_URL` is set).

use amail_proto::limits::{MAX_PAGE_BYTES, MAX_THREAD_BODY_BYTES};
use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

macro_rules! query_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(amail_store::testutil::Backend::Sqlite, stringify!($name)).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::$name(amail_store::testutil::Backend::Postgres, stringify!($name)).await;
                }
            )*
        }
    };
}

query_tests!(
    search_looks_at_a_bounded_window,
    archive_starts_from_the_org,
    archive_to_another_org_matches_named_addresses,
    concurrent_queries_are_limited,
    thread_bodies_are_bounded,
    pages_are_bounded_by_bytes,
    seat_list_cuts_long_roles,
);

struct Server {
    base: String,
    db: TestDb,
    http: reqwest::Client,
}

#[derive(Clone)]
struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

fn selfhosted() -> Config {
    Config {
        sweep_every: Duration::from_millis(200),
        ..Config::default()
    }
}

impl Server {
    /// `None` (test skipped) when Postgres is not configured.
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
            http: reqwest::Client::new(),
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

    /// `n` messages of the org's user seat stored directly.
    async fn fill(&self, org: &str, n: i64, body: &str) {
        let o = self.db.store.org_by_name(org).await.unwrap().unwrap();
        let seat = self.db.store.user_seat(o.id).await.unwrap().unwrap();
        self.db.fill_messages(o.id, seat.id, n, body).await;
    }
}

macro_rules! start {
    ($b:expr, $t:expr, $cfg:expr) => {
        match Server::start($b, $t, $cfg).await {
            Some(s) => s,
            None => return,
        }
    };
}

impl Client {
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, usize) {
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
        let v = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        (status, v, text.len())
    }
    async fn get(&self, path: &str) -> Value {
        let (s, v, _) = self.call(Method::GET, path, None).await;
        assert!(s.is_success(), "GET {path} -> {s}: {v}");
        v
    }
    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (s, v, _) = self.call(method.clone(), path, body).await;
        assert!(s.is_success(), "{method} {path} -> {s}: {v}");
        v
    }
    async fn send(&self, body: Value) -> i64 {
        self.ok(Method::POST, "/messages", Some(body)).await["id"]
            .as_i64()
            .unwrap()
    }
    async fn link(&self, other: &Client, other_name: &str, my_name: &str) {
        self.ok(Method::POST, "/links", Some(json!({ "org": other_name })))
            .await;
        other
            .ok(Method::POST, &format!("/links/{my_name}/accept"), None)
            .await;
    }
}

fn ids(v: &Value) -> Vec<i64> {
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect()
}

/// More messages than one request looks at.
const BIG: i64 = 60_000;

// --- search --------------------------------------------------------------------

/// A search looks at the newest 50,000 of the org's messages and answers
/// with a cursor to go on; another org's mail never counts.
async fn search_looks_at_a_bounded_window(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let big = srv.org("big").await;
    let small = srv.org("small").await;
    let first = big
        .send(json!({"to": ["owner"], "subject": "old", "body": "the needle, first"}))
        .await;
    srv.fill("big", BIG, "filler text without the word").await;
    let long = format!(
        "{}needle in the middle{}",
        "a b ".repeat(100),
        " c".repeat(200)
    );
    let last = big
        .send(json!({"to": ["owner"], "subject": "new", "body": long}))
        .await;
    small
        .send(json!({"to": ["owner"], "subject": "mine", "body": "数据库\n迁移方案"}))
        .await;

    // The first page finds the newest match, then stops with a cursor.
    let p1 = big.get("/archive/search?q=needle").await;
    assert_eq!(ids(&p1), vec![last], "{p1}");
    let next = p1["next_before_id"]
        .as_i64()
        .expect("cursor after the window");
    assert!(next > first && next < last);
    let snip = p1["messages"][0]["snippet"].as_str().unwrap();
    assert!(
        snip.starts_with('…') && snip.ends_with('…') && snip.contains("needle in the middle"),
        "{snip}"
    );
    assert!(snip.chars().count() <= 162, "{snip}");
    assert!(p1["messages"][0].get("body").is_none());
    // Following it reaches the oldest message, and the end.
    let p2 = big
        .get(&format!("/archive/search?q=needle&before_id={next}"))
        .await;
    assert_eq!(ids(&p2), vec![first], "{p2}");
    assert!(p2.get("next_before_id").is_none(), "{p2}");
    // Short terms look at fewer messages.
    let s = big.get("/archive/search?q=ne").await;
    assert_eq!(ids(&s), vec![last]);
    assert_eq!(s["scan_limit"], 2000);
    assert!(s["next_before_id"].as_i64().unwrap() > first);

    // The small org sees only its own mail, at once and without a cursor.
    let mine = small.get("/archive/search?q=迁移").await;
    assert_eq!(mine["messages"].as_array().unwrap().len(), 1);
    assert_eq!(mine["messages"][0]["snippet"], "数据库 迁移方案");
    assert!(mine.get("next_before_id").is_none());
    for q in ["needle", "filler", "the"] {
        let v = small.get(&format!("/archive/search?q={q}")).await;
        assert!(
            ids(&v).is_empty() && v.get("next_before_id").is_none(),
            "{q}: {v}"
        );
    }
    // Subject-only matches start the snippet at the body's start.
    let v = small.get("/archive/search?q=mine").await;
    assert_eq!(v["messages"][0]["snippet"], "数据库 迁移方案");
    // The overview counts only the org's mail.
    let o = small.get("/overview").await;
    assert_eq!(o["mail_last_hour"], 1, "{o}");
}

// --- archive -------------------------------------------------------------------

async fn archive_starts_from_the_org(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let big = srv.org("big").await;
    let small = srv.org("small").await;
    big.link(&small, "small", "big").await;
    let from_small = small
        .send(json!({"to": ["owner@big"], "subject": "hello", "body": "from small"}))
        .await;
    srv.fill("big", BIG, "filler").await;
    let own = big
        .send(json!({"to": ["owner"], "subject": "own", "body": "own"}))
        .await;

    // No filter: the newest messages, with a cursor when the page is full.
    let v = big.get("/archive/messages?limit=3").await;
    assert_eq!(ids(&v).len(), 3);
    assert_eq!(ids(&v)[0], own);
    assert_eq!(v["next_before_id"].as_i64(), Some(ids(&v)[2]));
    // Another org's seat as sender: the window first (empty page, cursor),
    // then the match.
    let p1 = big.get("/archive/messages?from=owner@small").await;
    assert!(ids(&p1).is_empty(), "{p1}");
    let next = p1["next_before_id"].as_i64().expect("cursor");
    let p2 = big
        .get(&format!(
            "/archive/messages?from=owner@small&before_id={next}"
        ))
        .await;
    assert_eq!(ids(&p2), vec![from_small]);
    assert!(p2.get("next_before_id").is_none());
    // A seat of the org: its sent mail or mailbox directly.
    let v = big.get("/archive/messages?to=owner&limit=5").await;
    assert_eq!(ids(&v), vec![own, from_small]);
    let v = big.get("/sent?limit=2").await;
    assert_eq!(ids(&v)[0], own);
    assert_eq!(ids(&v).len(), 2);
    // A thread.
    let v = big
        .get(&format!("/archive/messages?thread={from_small}"))
        .await;
    assert_eq!(ids(&v), vec![from_small]);
    // The small org: its own message only, whatever the filter.
    let v = small.get("/archive/messages?limit=500").await;
    assert_eq!(ids(&v), vec![from_small]);
    assert!(v.get("next_before_id").is_none());
    for f in [
        "from=owner@big",
        "to=owner@big",
        "thread=2",
        "since=2000-01-01T00:00:00Z",
    ] {
        let v = small.get(&format!("/archive/messages?{f}")).await;
        assert!(v.get("next_before_id").is_none(), "{f}: {v}");
        assert!(ids(&v).iter().all(|id| *id == from_small), "{f}: {v}");
    }
    let (s, _, _) = small
        .call(Method::GET, &format!("/threads/{own}"), None)
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// `to=` naming a seat of another org matches the messages that name that
/// address, not those that reached the seat through that org's list.
async fn archive_to_another_org_matches_named_addresses(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let alpha = srv.org("alpha").await;
    let bravo = srv.org("bravo").await;
    alpha.link(&bravo, "bravo", "alpha").await;
    bravo
        .ok(Method::POST, "/seats", Some(json!({"name": "secret-ops"})))
        .await;
    bravo
        .ok(
            Method::POST,
            "/lists",
            Some(json!({"name": "team", "post_policy": "anyone"})),
        )
        .await;
    bravo
        .ok(Method::PUT, "/lists/team/members/secret-ops", None)
        .await;
    let via_list = alpha
        .send(json!({"to": ["team@bravo"], "subject": "to the list", "body": "x"}))
        .await;
    let named = alpha
        .send(json!({"to": ["secret-ops@bravo"], "subject": "named", "body": "y"}))
        .await;
    let v = alpha.get("/archive/messages?to=secret-ops@bravo").await;
    assert_eq!(ids(&v), vec![named], "{v}");
    let v = alpha.get("/archive/messages?to=team@bravo").await;
    assert_eq!(ids(&v), vec![via_list], "{v}");
    // Bravo sees its own seat's mailbox, list mail included.
    let v = bravo.get("/archive/messages?to=secret-ops").await;
    assert_eq!(ids(&v), vec![named, via_list], "{v}");
}

async fn concurrent_queries_are_limited(b: Backend, t: &str) {
    let mut cfg = selfhosted();
    cfg.limits.org_queries = Some(1);
    let srv = start!(b, t, cfg);
    let big = srv.org("big").await;
    srv.fill("big", BIG, "filler").await;
    // More than one slot can serve while the others wait for one.
    let mut tasks = Vec::new();
    for _ in 0..32 {
        let c = big.clone();
        tasks.push(tokio::spawn(async move {
            c.call(Method::GET, "/archive/search?q=zzzz", None).await
        }));
    }
    let mut codes = Vec::new();
    for t in tasks {
        let (s, v, _) = t.await.unwrap();
        if s == StatusCode::TOO_MANY_REQUESTS {
            assert_eq!(v["error"]["code"], "too_many_queries");
        }
        codes.push(s);
    }
    assert!(codes.contains(&StatusCode::OK), "{codes:?}");
    assert!(codes.contains(&StatusCode::TOO_MANY_REQUESTS), "{codes:?}");
    // Afterwards the slot is free again.
    big.get("/archive/messages?limit=1").await;
}

// --- threads -------------------------------------------------------------------

async fn thread_bodies_are_bounded(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let acme = srv.org("acme").await;
    let root = acme
        .send(json!({"to": ["owner"], "subject": "root", "body": "root body"}))
        .await;
    let big = "x".repeat(250_000);
    let mut replies = Vec::new();
    for _ in 0..6 {
        replies.push(
            acme.send(json!({"to": ["owner"], "subject": "r", "body": big, "in_reply_to": root}))
                .await,
        );
    }
    let (s, v, len) = acme
        .call(Method::GET, &format!("/threads/{root}"), None)
        .await;
    assert!(s.is_success());
    assert!(len < MAX_THREAD_BODY_BYTES + 64 * 1024, "{len}");
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 7);
    // Newest bodies first: the four newest replies fit, the two older ones
    // do not; the small root does.
    let omitted: Vec<i64> = msgs
        .iter()
        .filter(|m| m["body_omitted"] == true)
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(omitted, replies[..2].to_vec(), "{omitted:?}");
    assert_eq!(msgs[0]["body"], "root body");
    assert_eq!(msgs[6]["body"].as_str().unwrap().len(), 250_000);
    assert!(msgs[1]["body"].as_str().unwrap().is_empty());
    // An omitted body comes with the message itself.
    let m = acme.get(&format!("/messages/{}?peek=1", replies[0])).await;
    assert_eq!(m["message"]["body"].as_str().unwrap().len(), 250_000);
}

// --- pages -----------------------------------------------------------------------

/// Headers made as large as the limits allow (quotes double in JSON).
async fn pages_are_bounded_by_bytes(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let acme = srv.org("acme").await;
    let meta: serde_json::Map<String, Value> = (0..16)
        .map(|i| (format!("k{i}"), Value::String("\"".repeat(250))))
        .collect();
    let subject = "\"".repeat(300);
    let n = 300;
    let mut sent = Vec::new();
    for _ in 0..n {
        sent.push(
            acme.send(json!({"to": ["owner"], "subject": subject, "body": "b", "meta": meta}))
                .await,
        );
    }
    let page_ok = |len: usize| len <= MAX_PAGE_BYTES + 1024;

    // Inbox: cut, flagged, and continued after the last entry.
    let (_, v, len) = acme.call(Method::GET, "/inbox?limit=1000", None).await;
    assert!(page_ok(len), "{len}");
    assert_eq!(v["truncated"], true);
    let got = v["entries"].as_array().unwrap();
    assert!(!got.is_empty() && got.len() < n);
    let last = got.last().unwrap()["delivery"]["seq"].as_i64().unwrap();
    let (_, rest, _) = acme
        .call(
            Method::GET,
            &format!("/inbox?limit=1000&after_seq={last}"),
            None,
        )
        .await;
    assert_eq!(got.len() + rest["entries"].as_array().unwrap().len(), n);
    assert!(rest.get("truncated").is_none());
    // Paging backwards keeps the newest entries.
    let (_, v, len) = acme
        .call(Method::GET, "/inbox?limit=1000&before_seq=999999999", None)
        .await;
    assert!(page_ok(len), "{len}");
    assert_eq!(v["truncated"], true);
    let back = v["entries"].as_array().unwrap();
    assert_eq!(
        back.last().unwrap()["message"]["id"].as_i64(),
        sent.last().copied()
    );

    // Changes: `more`, and `next` after the last entry kept.
    let (_, v, len) = acme.call(Method::GET, "/changes?limit=5000", None).await;
    assert!(page_ok(len), "{len}");
    assert_eq!(v["more"], true);
    let kept = v["entries"].as_array().unwrap();
    assert_eq!(v["next"], kept.last().unwrap()["delivery"]["mod_seq"]);

    // Archive and sent: the cursor continues after the last message kept.
    for path in ["/archive/messages?limit=500", "/sent?limit=500"] {
        let (_, v, len) = acme.call(Method::GET, path, None).await;
        assert!(page_ok(len), "{path}: {len}");
        let got = ids(&v);
        assert!(got.len() < n, "{path}");
        assert_eq!(v["next_before_id"].as_i64(), got.last().copied(), "{path}");
        let next = v["next_before_id"].as_i64().unwrap();
        let rest = acme.get(&format!("{path}&before_id={next}")).await;
        assert_eq!(got.len() + ids(&rest).len(), n, "{path}");
    }
}

// --- directory -------------------------------------------------------------------

async fn seat_list_cuts_long_roles(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let acme = srv.org("acme").await;
    let role = "r".repeat(16_000);
    acme.ok(
        Method::POST,
        "/seats",
        Some(json!({"name": "dev", "role": role})),
    )
    .await;
    acme.ok(
        Method::POST,
        "/seats",
        Some(json!({"name": "ops", "role": "short"})),
    )
    .await;
    let list = acme.get("/seats").await;
    let dev = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "dev")
        .unwrap();
    assert_eq!(dev["role"].as_str().unwrap().len(), 1_000);
    assert_eq!(dev["role_truncated"], true);
    let ops = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "ops")
        .unwrap();
    assert_eq!(ops["role"], "short");
    assert!(ops.get("role_truncated").is_none());
    let one = acme.get("/seats/dev").await;
    assert_eq!(one["role"].as_str().unwrap().len(), 16_000);
    assert!(one.get("role_truncated").is_none());
}
