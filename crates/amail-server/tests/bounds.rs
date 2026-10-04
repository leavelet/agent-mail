//! Security regressions: bounds on stored text, responses, storage and
//! memory per org; token revocation ending streams and sessions; org
//! deletion keeping the mail other orgs received. Both backends (Postgres
//! only when `AMAIL_TEST_PG_URL` is set).

#![allow(dead_code)]

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

macro_rules! bound_tests {
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

bound_tests!(text_field_limits, revoked_tokens_are_pruned,);

/// Start a server; `None` (test skipped) when Postgres is not configured.
macro_rules! start {
    ($b:expr, $t:expr, $cfg:expr) => {
        match Server::start($b, $t, $cfg, |_| {}).await {
            Some(s) => s,
            None => return,
        }
    };
    ($b:expr, $t:expr, $cfg:expr, $f:expr) => {
        match Server::start($b, $t, $cfg, $f).await {
            Some(s) => s,
            None => return,
        }
    };
}

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

fn public() -> Config {
    Config {
        sweep_every: Duration::from_millis(200),
        ..Config::public()
    }
}

fn selfhosted() -> Config {
    Config {
        sweep_every: Duration::from_millis(200),
        ..Config::default()
    }
}

impl Server {
    async fn start(
        backend: Backend,
        test: &str,
        cfg: Config,
        f: impl FnOnce(&mut AppState),
    ) -> Option<Server> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut state = AppState::new(db.store.clone());
        state.set_config(cfg);
        state.mirror_config.lease = Duration::from_millis(900);
        state.mirror_config.answer_timeout = Duration::from_millis(300);
        state.webhook_config.backoff = vec![Duration::from_millis(50), Duration::from_millis(50)];
        state.webhook_config.timeout = Duration::from_secs(2);
        f(&mut state);
        tokio::spawn(amail_server::serve(listener, state, std::future::pending()));
        Some(Server {
            base,
            db,
            http: reqwest::Client::new(),
        })
    }

    /// An org made directly in the store (like `amail admin create-org`).
    async fn org(&self, name: &str) -> Client {
        let token = new_token();
        self.db
            .store
            .create_org(name, "owner", &hash_token(&token))
            .await
            .unwrap();
        self.client(&token)
    }

    fn client(&self, token: &str) -> Client {
        Client {
            base: self.base.clone(),
            token: token.to_string(),
            http: self.http.clone(),
        }
    }

    /// An invite code (like `amail admin invite create`).
    async fn invite(&self, expires_at: Option<i64>) -> String {
        let code = amail_server::new_invite();
        self.db
            .store
            .create_invite(&hash_token(&code), Some("test"), expires_at)
            .await
            .unwrap();
        code
    }

    async fn operator(&self) -> Client {
        let token = new_token();
        self.db
            .store
            .create_operator_token(&hash_token(&token), Some("test"))
            .await
            .unwrap();
        self.client(&token)
    }

    async fn register(&self, body: Value) -> (StatusCode, Value) {
        self.raw(Method::POST, "/register", Some(body), &[]).await
    }

    /// An unauthenticated request with extra headers.
    async fn raw(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut req = self.http.request(method, format!("{}/v1{path}", self.base));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
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

    /// Sign a browser in; returns the `Set-Cookie` header.
    async fn login(&self, token: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
        let mut req = self
            .http
            .post(format!("{}/v1/session", self.base))
            .header("X-Amail-Csrf", "1")
            .json(&json!({ "token": token }));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let r = req.send().await.unwrap();
        let set = r
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (r.status(), set)
    }

    /// A request authenticated by a session cookie.
    async fn with_cookie(
        &self,
        method: Method,
        path: &str,
        set_cookie: &str,
    ) -> (StatusCode, Value) {
        let cookie = set_cookie.split(';').next().unwrap().to_string();
        self.raw(
            method,
            path,
            None,
            &[("Cookie", &cookie), ("X-Amail-Csrf", "1")],
        )
        .await
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
    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call(Method::GET, path, None).await
    }
    async fn status(&self, method: Method, path: &str, body: Option<Value>) -> StatusCode {
        self.call(method, path, body).await.0
    }
    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method.clone(), path, body).await;
        assert!(s.is_success(), "{method} {path} -> {s}: {v}");
        v
    }
    async fn send(&self, body: Value) -> i64 {
        self.ok(Method::POST, "/messages", Some(body)).await["id"]
            .as_i64()
            .unwrap()
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
    async fn link(&self, other: &Client, other_name: &str, my_name: &str) {
        self.ok(Method::POST, "/links", Some(json!({ "org": other_name })))
            .await;
        other
            .ok(Method::POST, &format!("/links/{my_name}/accept"), None)
            .await;
    }
    /// Collect the lines of a server-sent event stream in the background.
    async fn stream(&self, path: &str) -> (StatusCode, Arc<Mutex<Vec<String>>>) {
        let resp = self
            .http
            .get(format!("{}/v1{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        let status = resp.status();
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        if status.is_success() {
            let l = lines.clone();
            tokio::spawn(async move {
                let mut s = resp.bytes_stream();
                let mut buf = String::new();
                while let Some(Ok(chunk)) = s.next().await {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(i) = buf.find('\n') {
                        let line = buf[..i].to_string();
                        buf.drain(..=i);
                        l.lock().unwrap().push(line);
                    }
                }
                l.lock().unwrap().push("<closed>".into());
            });
        }
        (status, lines)
    }
}

async fn eventually(what: &str, mut f: impl AsyncFnMut() -> bool) {
    for _ in 0..100 {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out: {what}");
}

fn joined(lines: &Arc<Mutex<Vec<String>>>) -> String {
    lines.lock().unwrap().join("\n")
}

// --- stored text -------------------------------------------------------------

async fn text_field_limits(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let long = |n: usize| "x".repeat(n);
    let bad = [
        ("/seats", json!({ "name": "a", "display_name": long(201) })),
        ("/seats", json!({ "name": "a", "role": long(16_001) })),
        ("/seats", json!({ "name": "a", "model": long(201) })),
        ("/seats", json!({ "name": "a", "harness": long(65) })),
        ("/lists", json!({ "name": "l", "description": long(1001) })),
        ("/tokens", json!({ "scope": "node", "label": long(201) })),
        ("/tokens/rotate", json!({ "label": long(201) })),
    ];
    for (path, body) in bad {
        let (s, v) = acme.call(Method::POST, path, Some(body.clone())).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{path} {body}: {v}");
    }
    // At the limit is accepted.
    acme.ok(
        Method::POST,
        "/seats",
        Some(json!({ "name": "a", "display_name": long(200), "role": long(16_000) })),
    )
    .await;
    let (s, v) = acme
        .call(
            Method::PATCH,
            "/seats/a",
            Some(json!({ "role": long(16_001) })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let (s, v) = acme
        .call(
            Method::POST,
            "/seats/a/tokens",
            Some(json!({ "label": long(201) })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    let node = acme.node().await;
    let (s, v) = node
        .call(
            Method::POST,
            "/presence",
            Some(
                json!({ "seats": [{ "seat": "a", "state": "idle", "attention_hint": long(501) }] }),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
}

/// Revoked token rows are deleted by the periodic cleanup, so rotating or
/// revoking does not grow the database.
async fn revoked_tokens_are_pruned(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let mut acme = srv.org("acme").await;
    for _ in 0..5 {
        let v = acme.ok(Method::POST, "/tokens/rotate", None).await;
        acme = srv.client(v["token"].as_str().unwrap());
    }
    let node = acme.node().await;
    let id = acme
        .get("/tokens")
        .await
        .1
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["scope"] == "node")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    acme.ok(Method::DELETE, &format!("/tokens/{id}"), None)
        .await;
    assert_eq!(node.get("/pending").await.0, StatusCode::UNAUTHORIZED);
    let rows = srv.db.store.row_counts().await.unwrap();
    assert_eq!((rows.tokens, rows.revoked_tokens), (7, 6));
    srv.db.store.cleanup().await.unwrap();
    let rows = srv.db.store.row_counts().await.unwrap();
    assert_eq!((rows.tokens, rows.revoked_tokens), (1, 0));
    assert_eq!(acme.get("/whoami").await.0, StatusCode::OK);
}
