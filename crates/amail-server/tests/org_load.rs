//! One org's data and load must not slow down other orgs: requests whose
//! cost depends on the data take one of the org's query slots, the counts
//! are read from counters, and a thread the caller cannot see costs what a
//! missing one does. Both backends (Postgres only when `AMAIL_TEST_PG_URL`
//! is set).

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Instant;

macro_rules! load_tests {
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

load_tests!(
    org_request_limit_counts_all_tokens,
    heavy_endpoints_take_a_query_slot,
    counts_do_not_grow_with_deliveries,
    invisible_thread_costs_what_a_missing_one_does,
);

struct Server {
    base: String,
    db: TestDb,
    http: reqwest::Client,
    limiters: Arc<amail_server::limits::Limiters>,
}

#[derive(Clone)]
struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
    org_id: i64,
}

impl Server {
    async fn start(backend: Backend, test: &str, cfg: Config) -> Option<Server> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut state = AppState::new(db.store.clone());
        state.set_config(cfg);
        let limiters = state.limiters.clone();
        tokio::spawn(amail_server::serve(listener, state, std::future::pending()));
        Some(Server {
            base,
            db,
            http: reqwest::Client::new(),
            limiters,
        })
    }

    async fn org(&self, name: &str) -> Client {
        let token = new_token();
        let (org, _) = self
            .db
            .store
            .create_org(name, "owner", &hash_token(&token))
            .await
            .unwrap();
        Client {
            base: self.base.clone(),
            token,
            http: self.http.clone(),
            org_id: org.id,
        }
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
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method.clone(), path, body).await;
        assert!(s.is_success(), "{method} {path} -> {s}: {v}");
        v
    }
    async fn get(&self, path: &str) -> Value {
        self.ok(Method::GET, path, None).await
    }
    async fn send(&self, body: Value) -> Value {
        self.ok(Method::POST, "/messages", Some(body)).await
    }
    /// The fastest of five requests, in seconds.
    async fn time(&self, path: &str, want: StatusCode) -> f64 {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let t = Instant::now();
            let (s, v) = self.call(Method::GET, path, None).await;
            assert_eq!(s, want, "GET {path}: {v}");
            best = best.min(t.elapsed().as_secs_f64());
        }
        best
    }
}

/// The per-org request limit counts the requests of all the org's tokens
/// together; other orgs are not affected.
async fn org_request_limit_counts_all_tokens(b: Backend, t: &str) {
    let mut cfg = Config::default();
    cfg.limits.org_requests_per_sec = Some(5.0); // bursts of ten
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let other = srv.org("other").await;
    let second = Client {
        token: acme
            .ok(Method::POST, "/tokens", Some(json!({"scope": "user"})))
            .await["token"]
            .as_str()
            .unwrap()
            .to_string(),
        ..acme.clone()
    };
    let mut codes = Vec::new();
    for i in 0..24 {
        let c = if i % 2 == 0 { &acme } else { &second };
        codes.push(c.call(Method::GET, "/whoami", None).await);
    }
    let ok = codes.iter().filter(|(s, _)| s.is_success()).count();
    assert!((9..=12).contains(&ok), "{ok} of 24 served");
    let limited = codes
        .iter()
        .filter(|(s, v)| {
            *s == StatusCode::TOO_MANY_REQUESTS && v["error"]["code"] == "rate_limited"
        })
        .count();
    assert_eq!(ok + limited, 24);
    // Both tokens are refused now; the other org is served.
    assert_eq!(
        second.call(Method::GET, "/whoami", None).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    other.get("/whoami").await;
}

/// While all of an org's query slots are taken, its requests whose cost
/// depends on the data get 429 `too_many_queries` (after a short wait),
/// and everything else, and other orgs, are served as usual.
async fn heavy_endpoints_take_a_query_slot(b: Backend, t: &str) {
    let mut cfg = Config::default();
    cfg.limits.org_queries = Some(2);
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let other = srv.org("other").await;
    acme.ok(Method::POST, "/seats", Some(json!({"name": "a"})))
        .await;
    acme.ok(Method::POST, "/lists", Some(json!({"name": "team"})))
        .await;
    acme.ok(Method::PUT, "/lists/team/members/a", None).await;
    let sent = acme
        .send(json!({"to": ["a", "team"], "subject": "s", "body": "b"}))
        .await;
    let (id, thread) = (
        sent["id"].as_i64().unwrap(),
        sent["thread_id"].as_i64().unwrap(),
    );

    let held: Vec<_> = vec![
        srv.limiters.queries.acquire(acme.org_id).await.unwrap(),
        srv.limiters.queries.acquire(acme.org_id).await.unwrap(),
    ];
    let heavy = [
        "/overview".to_string(),
        "/attention".to_string(),
        format!("/threads/{thread}"),
        format!("/messages/{id}/status"),
        "/lists".to_string(),
        "/lists/team".to_string(),
        "/archive/search?q=body".to_string(),
        "/archive/messages".to_string(),
        "/changes?seat=a".to_string(),
        "/inbox?seat=a&limit=500".to_string(),
        "/sent?limit=500".to_string(),
    ];
    // Each waits for a slot for a while before it is refused.
    let tasks: Vec<_> = heavy
        .iter()
        .map(|path| {
            let (c, path) = (acme.clone(), path.clone());
            tokio::spawn(async move { (c.call(Method::GET, &path, None).await, path) })
        })
        .collect();
    for t in tasks {
        let ((s, v), path) = t.await.unwrap();
        assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{path}: {v}");
        assert_eq!(v["error"]["code"], "too_many_queries", "{path}: {v}");
    }
    // Bounded lookups need no slot.
    for path in [
        "/pending".to_string(),
        "/counts".to_string(),
        "/inbox?seat=a".to_string(),
        "/sent".to_string(),
        format!("/messages/{id}"),
        "/seats".to_string(),
        "/tokens".to_string(),
        "/whoami".to_string(),
    ] {
        acme.get(&path).await;
    }
    acme.ok(
        Method::POST,
        "/claim?seat=a",
        Some(json!({"via": "test", "classes": ["to"]})),
    )
    .await;
    acme.send(json!({"to": ["a"], "subject": "s2", "body": "b"}))
        .await;
    // A thread the org cannot see is not found without taking a slot.
    let (s, _) = acme.call(Method::GET, "/threads/999999", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Other orgs are not affected.
    other.get("/overview").await;
    other.get("/attention").await;
    drop(held);
    for path in &heavy {
        acme.get(path).await;
    }
}

/// `pending` and `counts` read counters: their cost does not grow with the
/// org's deliveries. An org with 200,000 pending deliveries is answered
/// about as fast as an org with none.
async fn counts_do_not_grow_with_deliveries(b: Backend, t: &str) {
    let srv = start!(b, t, Config::default());
    let big = srv.org("big").await;
    let small = srv.org("small").await;
    for i in 0..20 {
        big.ok(
            Method::POST,
            "/seats",
            Some(json!({"name": format!("s{i:02}")})),
        )
        .await;
        small
            .ok(
                Method::POST,
                "/seats",
                Some(json!({"name": format!("s{i:02}")})),
            )
            .await;
    }
    let owner = srv.db.store.user_seat(big.org_id).await.unwrap().unwrap();
    srv.db
        .fill_messages(big.org_id, owner.id, 10_000, "bulk")
        .await;
    srv.db
        .exec(&[
            format!(
                "INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq) \
                 SELECT {o}, m.id, s.id, 'to', 'immediate', \
                 s.last_seq + ROW_NUMBER() OVER (PARTITION BY s.id ORDER BY m.id), \
                 s.last_seq + ROW_NUMBER() OVER (PARTITION BY s.id ORDER BY m.id) \
                 FROM messages m JOIN seats s ON s.org_id = {o} AND s.kind = 'agent' WHERE m.org_id = {o}",
                o = big.org_id
            ),
            format!(
                "UPDATE seats SET last_seq = (SELECT MAX(seq) FROM deliveries WHERE seat_id = seats.id) \
                 WHERE org_id = {} AND kind = 'agent'",
                big.org_id
            ),
        ])
        .await;
    let p = big.get("/pending?seat=s00").await;
    assert_eq!(p["seats"][0]["to"], 10_000, "{p}");
    let c = big.get("/counts?seat=s00").await;
    assert_eq!(c["seats"][0]["unread"], 10_000, "{c}");
    for path in ["/pending", "/counts", "/overview"] {
        let slow = big.time(path, StatusCode::OK).await;
        let fast = small.time(path, StatusCode::OK).await;
        assert!(
            slow < fast * 5.0 + 0.05,
            "{path}: {slow:.4}s for 200,000 deliveries, {fast:.4}s for none"
        );
    }
}

/// `GET /threads/{id}` checks first whether the caller sees the thread, so
/// a large thread of another org costs what a missing thread does.
async fn invisible_thread_costs_what_a_missing_one_does(b: Backend, t: &str) {
    let srv = start!(b, t, Config::default());
    let owner_of = srv.org("victim").await;
    let other = srv.org("other").await;
    let owner = srv
        .db
        .store
        .user_seat(owner_of.org_id)
        .await
        .unwrap()
        .unwrap();
    srv.db
        .fill_messages(owner_of.org_id, owner.id, 60_000, "thread")
        .await;
    srv.db
        .exec(&[format!(
            "UPDATE messages SET thread_id = (SELECT MIN(id) FROM messages WHERE org_id = {o}) \
             WHERE org_id = {o}",
            o = owner_of.org_id
        )])
        .await;
    let thread = srv
        .db
        .rows_i64(&format!(
            "SELECT MIN(id) FROM messages WHERE org_id = {}",
            owner_of.org_id
        ))
        .await[0][0];
    // The owner sees it (its newest 500 messages).
    let v = owner_of.get(&format!("/threads/{thread}")).await;
    assert_eq!(v["messages"].as_array().unwrap().len(), 500);
    let hidden = other
        .time(&format!("/threads/{thread}"), StatusCode::NOT_FOUND)
        .await;
    let missing = other
        .time("/threads/999999999", StatusCode::NOT_FOUND)
        .await;
    assert!(
        hidden < missing * 2.0 + 0.005,
        "{hidden:.4}s for another org's thread, {missing:.4}s for a missing one"
    );
}
