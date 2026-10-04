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

bound_tests!(
    text_field_limits,
    revoked_tokens_are_pruned,
    revocation_ends_streams,
    rotation_ends_sessions,
    sessions_per_token_are_capped,
    mirror_post_size_is_capped,
    stalled_mirror_watcher_is_dropped,
    mirror_memory_limit,
    storage_quota,
    org_deletion_keeps_received_mail,
    claim_loads_bounded_bodies,
);

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

// --- revocation -----------------------------------------------------------------

fn closed(lines: &Arc<Mutex<Vec<String>>>) -> bool {
    lines.lock().unwrap().iter().any(|l| l == "<closed>")
}

/// Streams opened with a token end when the token is revoked, rotated or
/// reset on the database.
async fn revocation_ends_streams(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    acme.seat("w").await;
    // A second user token (the leaked one) and a node token.
    let leaked = srv.client(
        acme.ok(Method::POST, "/tokens", Some(json!({ "scope": "user" })))
            .await["token"]
            .as_str()
            .unwrap(),
    );
    let node = acme.node().await;
    let (s, mirror) = leaked.stream("/seats/w/transcript").await;
    assert_eq!(s, StatusCode::OK);
    let (s, events) = node.stream("/events").await;
    assert_eq!(s, StatusCode::OK);
    let (s, own) = acme.stream("/events").await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<(i64, String)> = acme
        .get("/tokens")
        .await
        .1
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["id"].as_i64().unwrap(),
                t["scope"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for (id, scope) in &ids {
        if scope == "node" || (scope == "user" && *id != ids[0].0) {
            acme.ok(Method::DELETE, &format!("/tokens/{id}"), None)
                .await;
        }
    }
    eventually("streams of revoked tokens end", async || {
        closed(&mirror) && closed(&events)
    })
    .await;
    assert!(!closed(&own), "{}", joined(&own));
    // Rotation ends the streams of the rotated token.
    let v = acme.ok(Method::POST, "/tokens/rotate", None).await;
    let new = srv.client(v["token"].as_str().unwrap());
    eventually("streams of a rotated token end", async || closed(&own)).await;
    // A reset on the database ends them at the next sweep.
    let (_, mirror) = new.stream("/seats/w/transcript").await;
    let org = srv.db.store.org_by_name("acme").await.unwrap().unwrap();
    srv.db
        .store
        .reset_user_tokens(org.id, &hash_token(&new_token()))
        .await
        .unwrap();
    eventually("streams of reset tokens end", async || closed(&mirror)).await;
}

/// Rotating a token ends every browser session made with it; a browser
/// that rotated gets a new session for the new token.
async fn rotation_ends_sessions(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let (_, attacker) = srv.login(&acme.token, &[]).await;
    let (_, owner) = srv.login(&acme.token, &[]).await;
    let ok = |c: &str| {
        let c = c.to_string();
        let srv = &srv;
        async move { srv.with_cookie(Method::GET, "/whoami", &c).await.0 }
    };
    assert_eq!(ok(&attacker).await, StatusCode::OK);
    // Rotation through the owner's browser session.
    let cookie = owner.split(';').next().unwrap().to_string();
    let r = srv
        .http
        .post(format!("{}/v1/tokens/rotate", srv.base))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let fresh = r
        .headers()
        .get("set-cookie")
        .expect("a new session cookie")
        .to_str()
        .unwrap()
        .to_string();
    let v: Value = r.json().await.unwrap();
    assert_eq!(ok(&attacker).await, StatusCode::UNAUTHORIZED);
    assert_eq!(ok(&owner).await, StatusCode::UNAUTHORIZED);
    assert_eq!(ok(&fresh).await, StatusCode::OK);
    // Rotation with the bearer token ends the sessions too and sets no
    // cookie.
    let new = srv.client(v["token"].as_str().unwrap());
    let (_, other) = srv.login(&new.token, &[]).await;
    let r = srv
        .http
        .post(format!("{}/v1/tokens/rotate", srv.base))
        .bearer_auth(&new.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert!(r.headers().get("set-cookie").is_none());
    assert_eq!(ok(&other).await, StatusCode::UNAUTHORIZED);
    assert_eq!(ok(&fresh).await, StatusCode::UNAUTHORIZED);
}

/// A token keeps at most a fixed number of browser sessions; signing in
/// again ends the oldest.
async fn sessions_per_token_are_capped(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.token_requests_per_sec = None;
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let max = amail_proto::limits::MAX_SESSIONS_PER_TOKEN as usize;
    let mut cookies = Vec::new();
    for _ in 0..max + 5 {
        let (s, c) = srv.login(&acme.token, &[]).await;
        assert_eq!(s, StatusCode::OK);
        cookies.push(c);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(
        srv.db.store.row_counts().await.unwrap().web_sessions,
        max as i64
    );
    for (i, c) in cookies.iter().enumerate() {
        let want = if i < 5 {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::OK
        };
        assert_eq!(
            srv.with_cookie(Method::GET, "/whoami", c).await.0,
            want,
            "session {i}"
        );
    }
}

// --- conversation-mirror memory -------------------------------------------------

/// A server whose mirror hub the test can inspect.
async fn start_with_hub(
    b: Backend,
    t: &str,
    cfg: Config,
) -> Option<(Server, Arc<amail_server::transcript::MirrorHub>)> {
    let hub = Arc::new(Mutex::new(None));
    let h = hub.clone();
    let srv = Server::start(b, t, cfg, move |st| {
        *h.lock().unwrap() = Some(st.mirror.clone());
    })
    .await?;
    let hub = hub.lock().unwrap().take().unwrap();
    Some((srv, hub))
}

/// A watcher that sends its request and never reads the answer.
async fn stalled_watcher(srv: &Server, token: &str, seat: &str) -> tokio::net::TcpStream {
    let addr: std::net::SocketAddr = srv.base.trim_start_matches("http://").parse().unwrap();
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.set_recv_buffer_size(4096).unwrap();
    let mut s = sock.connect(addr).await.unwrap();
    use tokio::io::AsyncWriteExt;
    s.write_all(
        format!(
            "GET /v1/seats/{seat}/transcript HTTP/1.1\r\nHost: {addr}\r\n\
             Authorization: Bearer {token}\r\nAccept: text/event-stream\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    s
}

/// A post of `n` events of 15,000 characters (about 15 KB each).
fn big_post(n: usize, tag: &str) -> Value {
    let ev = json!({ "role": "assistant", "kind": "text", "text": "A".repeat(15_000) });
    let mut events = vec![ev; n];
    events.push(json!({ "role": "assistant", "kind": "text", "text": tag }));
    json!({ "node": "n1", "events": events })
}

/// A transcript post larger than the limit is refused with a clear error;
/// one just under it is accepted.
async fn mirror_post_size_is_capped(b: Backend, t: &str) {
    use amail_proto::limits::MAX_TRANSCRIPT_POST_BYTES;
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    acme.seat("w").await;
    let node = acme.node().await;
    let (s, _w) = acme.stream("/seats/w/transcript").await;
    assert_eq!(s, StatusCode::OK);
    let n = MAX_TRANSCRIPT_POST_BYTES / 15_050;
    let ok = big_post(n, "fits");
    assert!(serde_json::to_vec(&ok).unwrap().len() <= MAX_TRANSCRIPT_POST_BYTES);
    node.ok(Method::POST, "/seats/w/transcript", Some(ok)).await;
    let (s, v) = node
        .call(
            Method::POST,
            "/seats/w/transcript",
            Some(big_post(n + 5, "big")),
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["error"]["code"], "too_large", "{v}");
    // Above the general request limit too.
    let (s, v) = node
        .call(
            Method::POST,
            "/seats/w/transcript",
            Some(big_post(200, "huge")),
        )
        .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["error"]["code"], "too_large", "{v}");
}

/// A watcher that stops reading is disconnected once its queue is full,
/// and the mirror's memory stays bounded; a watcher that reads keeps
/// getting everything.
async fn stalled_mirror_watcher_is_dropped(b: Backend, t: &str) {
    use amail_proto::limits::{MIRROR_SEAT_BYTES, MIRROR_WATCHER_BYTES};
    let Some((srv, hub)) = start_with_hub(b, t, public()).await else {
        return;
    };
    let acme = srv.org("acme").await;
    acme.seat("w").await;
    let node = acme.node().await;
    let _stalled = stalled_watcher(&srv, &acme.token, "w").await;
    eventually("the stalled watcher is subscribed", async || {
        !hub.watched(1).is_empty()
    })
    .await;
    let (s, reader) = acme.stream("/seats/w/transcript").await;
    assert_eq!(s, StatusCode::OK);
    let mut subscribers = Vec::new();
    let mut peak = 0;
    for i in 0..40 {
        let v = node
            .ok(
                Method::POST,
                "/seats/w/transcript",
                Some(big_post(60, &format!("post-{i}"))),
            )
            .await;
        subscribers.push(v["subscribers"].as_u64().unwrap());
        peak = peak.max(hub.bytes());
    }
    // 40 posts of 900 KB: without bounds the stalled watcher would hold
    // all of them.
    assert!(
        peak <= MIRROR_SEAT_BYTES,
        "mirror memory peaked at {peak} bytes"
    );
    assert!(
        subscribers.ends_with(&[1, 1]),
        "the stalled watcher is disconnected: {subscribers:?}"
    );
    assert!(subscribers[0] == 2, "{subscribers:?}");
    eventually("the reading watcher gets every post", async || {
        joined(&reader).contains("post-39")
    })
    .await;
    assert!(!closed(&reader));
    assert!(hub.bytes() <= MIRROR_WATCHER_BYTES, "{}", hub.bytes());
}

/// The limit over all seats: stalled watchers are disconnected and replay
/// buffers shortened; watchers that read keep getting everything.
async fn mirror_memory_limit(b: Backend, t: &str) {
    const LIMIT: usize = 6 * 1024 * 1024;
    let mut cfg = public();
    cfg.limits.mirror_bytes = Some(LIMIT);
    let Some((srv, hub)) = start_with_hub(b, t, cfg).await else {
        return;
    };
    let acme = srv.org("acme").await;
    let node = acme.node().await;
    let mut stalled = Vec::new();
    let mut readers = Vec::new();
    for seat in ["a", "b", "c"] {
        acme.seat(seat).await;
        stalled.push(stalled_watcher(&srv, &acme.token, seat).await);
        let (s, r) = acme.stream(&format!("/seats/{seat}/transcript")).await;
        assert_eq!(s, StatusCode::OK);
        readers.push(r);
    }
    eventually("all watchers are subscribed", async || {
        hub.watched(1).len() == 3
    })
    .await;
    let mut peak = 0;
    for i in 0..8 {
        for seat in ["a", "b", "c"] {
            node.ok(
                Method::POST,
                &format!("/seats/{seat}/transcript"),
                Some(big_post(60, &format!("{seat}-{i}"))),
            )
            .await;
            peak = peak.max(hub.bytes());
        }
    }
    assert!(peak <= LIMIT, "mirror memory peaked at {peak} bytes");
    for seat in ["a", "b", "c"] {
        let v = node
            .ok(
                Method::POST,
                &format!("/seats/{seat}/transcript"),
                Some(json!({ "node": "n1", "events": [] })),
            )
            .await;
        assert_eq!(v["subscribers"], 1, "stalled watcher of {seat} is gone");
    }
    for (seat, r) in ["a", "b", "c"].iter().zip(&readers) {
        let last = format!("{seat}-7");
        eventually("readers get the last post", async || {
            joined(r).contains(&last)
        })
        .await;
    }
    // A new watcher still gets in.
    let (s, _) = acme.stream("/seats/a/transcript").await;
    assert_eq!(s, StatusCode::OK);
    assert!(hub.bytes() <= LIMIT, "{}", hub.bytes());
}

// --- storage -------------------------------------------------------------------

/// Each org's stored bytes (subject, body and meta of the mail it sent, once
/// per message) are counted on send, reduced when retention clears bodies,
/// and a send that would exceed the quota is refused with 507.
async fn storage_quota(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.org_storage_bytes = Some(10_000);
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    acme.link(&beta, "beta", "acme").await;
    let a = acme.seat("a").await;
    let body = "x".repeat(3000);
    // Three recipients, counted once: 3 + 3000 + {"k":"v"} = 3012 bytes.
    a.send(json!({ "to": ["owner", "owner@beta"], "cc": ["a"], "subject": "one", "body": body, "meta": { "k": "v" } }))
        .await;
    let org = acme.get("/org").await.1;
    assert_eq!(org["stored_bytes"], 3012, "{org}");
    assert_eq!(org["storage_quota_bytes"], 10_000, "{org}");
    // The receiving org is not charged.
    assert_eq!(beta.get("/org").await.1["stored_bytes"], 0);
    // The user seat is not exempt.
    acme.send(json!({ "to": ["a"], "subject": "two", "body": body }))
        .await;
    a.send(json!({ "to": ["owner"], "subject": "three", "body": body }))
        .await;
    let (s, v) = a
        .call(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["owner"], "subject": "four", "body": body })),
        )
        .await;
    assert_eq!(s, StatusCode::INSUFFICIENT_STORAGE, "{v}");
    assert_eq!(v["error"]["code"], "storage_quota", "{v}");
    let stored = 3012 + 3003 + 3005;
    assert_eq!(acme.get("/org").await.1["stored_bytes"], stored);
    // Small mail still fits; the operator sees the count.
    a.send(json!({ "to": ["owner"], "subject": "s", "body": "b" }))
        .await;
    let op = srv.operator().await;
    let stats = op.get("/operator/stats").await.1;
    let row = stats["list"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "acme")
        .unwrap()
        .clone();
    assert_eq!(row["stored_bytes"], stored + 2, "{stats}");
    // Retention clears bodies and gives their bytes back.
    let n = srv
        .db
        .store
        .expire_bodies(amail_store::now_ms() + 1, 1000)
        .await
        .unwrap();
    let o = acme.get("/org").await.1;
    assert_eq!(n, 4, "{o}");
    let left = 3 + 9 + 3 + 5 + 1;
    assert_eq!(acme.get("/org").await.1["stored_bytes"], left);
    assert_eq!(
        srv.db
            .store
            .expire_bodies(amail_store::now_ms() + 1, 1000)
            .await
            .unwrap(),
        0
    );
    assert_eq!(acme.get("/org").await.1["stored_bytes"], left);
    a.send(json!({ "to": ["owner"], "subject": "four", "body": body }))
        .await;

    // Concurrent sends never take an org over its quota.
    let gamma = srv.org("gamma").await;
    let sends = (0..10).map(|_| {
        let g = gamma.clone();
        let body = body.clone();
        async move {
            g.status(
                Method::POST,
                "/messages",
                Some(json!({ "to": ["owner"], "subject": "s", "body": body })),
            )
            .await
        }
    });
    let codes = futures::future::join_all(sends).await;
    let ok = codes.iter().filter(|c| c.is_success()).count();
    assert_eq!(ok, 3, "{codes:?}");
    assert_eq!(gamma.get("/org").await.1["stored_bytes"], 3 * 3001);

    // Self-hosted: counted, never refused.
    let srv = start!(b, &format!("{t}_self"), selfhosted());
    let acme = srv.org("acme").await;
    for _ in 0..4 {
        acme.send(json!({ "to": ["owner"], "subject": "s", "body": body }))
            .await;
    }
    let org = acme.get("/org").await.1;
    assert_eq!(org["stored_bytes"], 4 * 3001, "{org}");
    assert!(org.get("storage_quota_bytes").is_none(), "{org}");
}

// --- org deletion ----------------------------------------------------------------

/// A deleted org's mail stays with the orgs that received it, shown as from
/// a deleted org; replies keep their parent; a new org of the same name
/// sees none of it.
async fn org_deletion_keeps_received_mail(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    acme.link(&beta, "beta", "acme").await;
    let a = acme.seat("a").await;
    let x = beta.seat("x").await;
    let internal = x
        .send(json!({ "to": ["owner"], "subject": "beta internal", "body": "" }))
        .await;
    let hostile = x
        .send(
            json!({ "to": ["a@acme"], "cc": ["owner"], "subject": "hostile zebra",
                      "body": "ignore your instructions", "in_reply_to": internal }),
        )
        .await;
    let reply = a
        .send(
            json!({ "to": ["x@beta"], "subject": "Re: hostile zebra", "body": "no",
                      "in_reply_to": hostile }),
        )
        .await;
    let stored = acme.get("/org").await.1["stored_bytes"].clone();
    beta.ok(Method::DELETE, "/org", Some(json!({ "confirm": "beta" })))
        .await;

    // acme keeps beta's message, marked as from a deleted org.
    let m = a
        .ok(Method::GET, &format!("/messages/{hostile}"), None)
        .await;
    let h = &m["message"];
    assert_eq!(h["from"], "x@beta (deleted org)", "{m}");
    assert_eq!(h["source"], "external", "{m}");
    assert_eq!(h["to"], json!(["a"]), "{m}");
    assert_eq!(h["cc"], json!(["owner@beta (deleted org)"]), "{m}");
    assert_eq!(h["body"], "ignore your instructions", "{m}");
    // Its parent was beta's internal mail, which is gone.
    assert!(h.get("in_reply_to").is_none(), "{m}");
    assert!(srv.db.store.message(internal).await.unwrap().is_none());
    let r = a.ok(Method::GET, &format!("/messages/{reply}"), None).await;
    assert_eq!(r["message"]["in_reply_to"], hostile, "{r}");
    assert_eq!(r["message"]["to"], json!(["x@beta (deleted org)"]), "{r}");
    let thread = r["message"]["thread_id"].as_i64().unwrap();
    let th = acme
        .ok(Method::GET, &format!("/threads/{thread}"), None)
        .await;
    let ids: Vec<i64> = th["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![hostile, reply], "{th}");
    assert_eq!(th["messages"][0]["from"], "x@beta (deleted org)");
    let arch = acme.ok(Method::GET, "/archive/messages", None).await;
    let froms: Vec<&str> = arch["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["from"].as_str().unwrap())
        .collect();
    assert_eq!(froms, vec!["a", "x@beta (deleted org)"], "{arch}");
    let found = acme.ok(Method::GET, "/archive/search?q=zebra", None).await;
    assert_eq!(found["messages"].as_array().unwrap().len(), 2, "{found}");
    // acme's count does not change; the deleted org is listed nowhere.
    assert_eq!(acme.get("/org").await.1["stored_bytes"], stored);
    assert_eq!(acme.ok(Method::GET, "/links", None).await, json!([]));
    let op = srv.operator().await;
    let stats = op.ok(Method::GET, "/operator/stats", None).await;
    assert_eq!(stats["orgs"], 1, "{stats}");
    assert!(srv.db.store.org_by_name("beta").await.unwrap().is_none());

    // A new org named beta sees none of it, even once linked to acme, and
    // cannot reply into it.
    let code = srv.invite(None).await;
    let (s, v) = srv.register(json!({ "invite": code, "org": "beta" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let beta2 = srv.client(v["token"].as_str().unwrap());
    acme.link(&beta2, "beta", "acme").await;
    assert_eq!(
        beta2.ok(Method::GET, "/archive/messages", None).await["messages"],
        json!([])
    );
    let found = beta2.ok(Method::GET, "/archive/search?q=zebra", None).await;
    assert_eq!(found["messages"], json!([]), "{found}");
    for id in [hostile, reply] {
        assert_eq!(
            beta2.get(&format!("/messages/{id}")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        beta2.get(&format!("/threads/{thread}")).await.0,
        StatusCode::NOT_FOUND
    );
    let (s, v) = beta2
        .call(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["a@acme"], "subject": "Re", "body": "", "in_reply_to": hostile })),
        )
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    // Replying to the deleted org's address reaches nobody.
    let (s, v) = a
        .call(
            Method::POST,
            "/messages",
            Some(
                json!({ "to": ["x@beta (deleted org)"], "subject": "Re", "body": "",
                         "in_reply_to": hostile }),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    // acme still sees beta's message as from the deleted org, not from
    // the new beta.
    let m = a
        .ok(Method::GET, &format!("/messages/{hostile}?peek=1"), None)
        .await;
    assert_eq!(m["message"]["from"], "x@beta (deleted org)", "{m}");

    // Deleting the receiving org too leaves nothing behind.
    acme.ok(Method::DELETE, "/org", Some(json!({ "confirm": "acme" })))
        .await;
    for id in [hostile, reply] {
        assert!(srv.db.store.message(id).await.unwrap().is_none());
    }
}

// --- claims ----------------------------------------------------------------------

/// A claim works with at most `MAX_CLAIM_BODY_CHARS` of each body: a longer
/// body is neither inlined nor marked read, whatever `inline_body_max` the
/// client asks for, and a shorter one is inlined as before.
async fn claim_loads_bounded_bodies(b: Backend, t: &str) {
    use amail_proto::limits::MAX_CLAIM_BODY_CHARS;
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let w = acme.seat("w").await;
    let long = "y".repeat(MAX_CLAIM_BODY_CHARS + 1);
    let short = "z".repeat(MAX_CLAIM_BODY_CHARS);
    acme.send(json!({ "to": ["w"], "subject": "long", "body": long }))
        .await;
    acme.send(json!({ "to": ["w"], "subject": "short", "body": short }))
        .await;
    let v = w
        .ok(
            Method::POST,
            "/claim",
            Some(
                json!({ "classes": ["to"], "via": "test", "inline_body_max": 1_000_000,
                          "mark_inlined_read": true, "preview_chars": 1_000_000 }),
            ),
        )
        .await;
    let e = v["entries"].as_array().unwrap();
    assert_eq!(e.len(), 2, "{v}");
    assert!(e[0]["body"].is_null(), "long body inlined");
    assert!(e[0]["delivery"]["read_at"].is_null(), "long body marked read");
    assert!(
        e[0]["preview"].as_str().unwrap().chars().count() <= MAX_CLAIM_BODY_CHARS + 1,
        "preview too long"
    );
    assert_eq!(e[1]["body"].as_str().unwrap().len(), MAX_CLAIM_BODY_CHARS);
    assert!(!e[1]["delivery"]["read_at"].is_null(), "short body not marked read");
}
