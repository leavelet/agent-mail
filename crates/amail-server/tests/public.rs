//! Public mode (M6): registration with invite codes, protective limits,
//! hardening (webhook targets, trusted proxies, reserved names, cookies),
//! the operator's view, deleting data, retention, token rotation, and one
//! cross-org negative test per kind of resource addressed by id or name.
//! Both backends (Postgres only when `AMAIL_TEST_PG_URL` is set).

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

macro_rules! public_tests {
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

public_tests!(
    registration_with_invites,
    invite_name_conflicts_are_counted,
    link_requests_expire,
    register_then_first_mail,
    registration_rate_limit_by_client_address,
    registration_rate_limit_per_ipv6_48,
    token_rotation_and_reset,
    seat_and_send_limits,
    stream_request_and_link_limits,
    list_token_and_sign_in_limits,
    webhook_targets_public_mode,
    webhook_redirects_not_followed,
    forwarded_headers_need_trusted_proxy,
    reserved_names,
    operator_view_and_suspension,
    operator_deletes_org,
    user_deletes_own_org,
    retention_clears_bodies,
    cross_org_messages,
    cross_org_threads_archive_search,
    cross_org_mailboxes_and_deliveries,
    cross_org_seats,
    cross_org_lists,
    cross_org_tokens,
    cross_org_links,
    cross_org_mirrors,
    cross_org_node_commands_and_presence,
    link_requests_do_not_reveal_org_names,
    webhooks_in_flight_per_org,
    webhooks_in_flight_fair_between_orgs,
    org_settings_bounded,
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

// --- registration --------------------------------------------------------------

async fn registration_with_invites(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.registrations_per_hour = None;
    let srv = start!(b, t, cfg);
    let code = srv.invite(None).await;

    // Names are checked before the invite is used.
    for (org, user, code_) in [
        ("Bad Name", None, "invalid_name"),
        ("admin", None, "reserved_name"),
        ("amail", None, "reserved_name"),
        ("acme", Some("system"), "reserved_name"),
        ("acme", Some("bad name"), "invalid_name"),
    ] {
        let (s, v) = srv
            .register(json!({ "invite": code, "org": org, "user_name": user }))
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{org}: {v}");
        assert_eq!(v["error"]["code"], code_);
    }
    // A wrong code: 403, the same answer as for a used one.
    let (s, v) = srv
        .register(json!({ "invite": "inv_000000000000000000000000", "org": "acme" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "invalid_invite");

    let (s, v) = srv.register(json!({ "invite": code, "org": "acme" })).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["org"], "acme");
    assert_eq!(v["user_seat"], "owner");
    let user = srv.client(v["token"].as_str().unwrap());
    assert_eq!(
        user.ok(Method::GET, "/whoami", None).await,
        json!({ "org": "acme", "scope": "user", "seat": "owner" })
    );
    // One-time.
    let (s, v) = srv
        .register(json!({ "invite": code, "org": "acme2" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "invalid_invite");

    // Without a valid invite, a taken name looks like any other name.
    let (s, v) = srv
        .register(json!({ "invite": "amail_inv_bogus", "org": "acme" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "invalid_invite");

    // A taken name does not use up the invite (up to a few times; see
    // invite_name_conflicts_are_counted).
    let code2 = srv.invite(None).await;
    let (s, v) = srv
        .register(json!({ "invite": code2, "org": "acme" }))
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "org_exists");
    let (s, _) = srv
        .register(json!({ "invite": code2, "org": "beta", "user_name": "alice" }))
        .await;
    assert_eq!(s, StatusCode::CREATED);

    // Expired and revoked invites are refused.
    let old = srv.invite(Some(amail_store::now_ms() - 1000)).await;
    assert_eq!(
        srv.register(json!({ "invite": old, "org": "gamma" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let revoked = srv.invite(None).await;
    let id = srv
        .db
        .store
        .invites()
        .await
        .unwrap()
        .iter()
        .map(|i| i.id)
        .max()
        .unwrap();
    assert!(srv.db.store.revoke_invite(id).await.unwrap());
    assert_eq!(
        srv.register(json!({ "invite": revoked, "org": "gamma" }))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // The store says what was used by whom; codes are only stored hashed.
    let inv = srv.db.store.invites().await.unwrap();
    let used: Vec<_> = inv.iter().filter_map(|i| i.used_by.clone()).collect();
    assert_eq!(used, vec!["acme".to_string(), "beta".to_string()]);

    // Information for the sign-in page.
    let (s, v) = srv.raw(Method::GET, "/info", None, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["public"], true);
    assert_eq!(v["registration"], true);

    // A self-hosted server has no registration.
    let srv = start!(b, &format!("{t}_selfhosted"), selfhosted());
    let code = srv.invite(None).await;
    assert_eq!(
        srv.register(json!({ "invite": code, "org": "acme" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, v) = srv.raw(Method::GET, "/info", None, &[]).await;
    assert_eq!(v["public"], false);
    assert_eq!(v["registration"], false);
}

/// From an invite to the first message between two seats (the CLI and the
/// web interface have their own end-to-end tests).
async fn register_then_first_mail(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let code = srv.invite(None).await;
    let (_, v) = srv.register(json!({ "invite": code, "org": "acme" })).await;
    let user = srv.client(v["token"].as_str().unwrap());
    let a = user.seat("alpha").await;
    let bb = user.seat("beta-1").await;
    let id = a
        .send(json!({ "to": ["beta-1"], "subject": "hello", "body": "first mail" }))
        .await;
    let inbox = bb.ok(Method::GET, "/inbox?unread=1", None).await;
    assert_eq!(inbox["entries"][0]["message"]["id"], id);
    let m = bb.ok(Method::GET, &format!("/messages/{id}"), None).await;
    assert_eq!(m["message"]["body"], "first mail");
    assert_eq!(m["message"]["from"], "alpha");
}

async fn registration_rate_limit_by_client_address(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.registrations_per_hour = Some(2.0); // a burst of one
    cfg.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let srv = start!(b, t, cfg);
    let reg = |ip: &'static str| {
        let srv = &srv;
        async move {
            srv.raw(
                Method::POST,
                "/register",
                Some(json!({ "invite": "inv_x", "org": "acme" })),
                &[("X-Forwarded-For", ip)],
            )
            .await
        }
    };
    // Failed attempts count too.
    assert_eq!(reg("203.0.113.1").await.0, StatusCode::FORBIDDEN);
    let (s, v) = reg("203.0.113.1").await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert!(v["error"]["retry_after_ms"].as_u64().unwrap() > 0);
    // Another client (as told by the trusted proxy) has its own budget.
    assert_eq!(reg("203.0.113.2").await.0, StatusCode::FORBIDDEN);

    // Without trusted proxies the header is ignored: all requests come from
    // 127.0.0.1.
    let mut cfg = public();
    cfg.limits.registrations_per_hour = Some(2.0);
    let srv2 = start!(b, &format!("{t}_untrusted"), cfg);
    let reg2 = |ip: &'static str| {
        let srv = &srv2;
        async move {
            srv.raw(
                Method::POST,
                "/register",
                Some(json!({ "invite": "inv_x", "org": "acme" })),
                &[("X-Forwarded-For", ip)],
            )
            .await
            .0
        }
    };
    assert_eq!(reg2("203.0.113.1").await, StatusCode::FORBIDDEN);
    assert_eq!(reg2("203.0.113.2").await, StatusCode::TOO_MANY_REQUESTS);
}

/// IPv6 clients are limited per /64 and, together, per /48: many /64
/// networks of one /48 cannot fill the limiter. A /64 that is out of
/// attempts does not use up its /48's.
async fn registration_rate_limit_per_ipv6_48(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.registrations_per_hour = Some(2.0); // a burst of one per /64
    cfg.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let srv = start!(b, t, cfg);
    let reg = |ip: String| {
        let srv = &srv;
        async move {
            srv.raw(
                Method::POST,
                "/register",
                Some(json!({ "invite": "inv_x", "org": "acme" })),
                &[("X-Forwarded-For", ip.as_str())],
            )
            .await
            .0
        }
    };
    assert_eq!(reg("2001:db8:1:1::1".into()).await, StatusCode::FORBIDDEN);
    for _ in 0..5 {
        assert_eq!(
            reg("2001:db8:1:1::2".into()).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
    // The /48 allows four times the per-/64 burst.
    for n in 2..=4 {
        assert_eq!(
            reg(format!("2001:db8:1:{n}::1")).await,
            StatusCode::FORBIDDEN,
            "/64 number {n}"
        );
    }
    assert_eq!(
        reg("2001:db8:1:5::1".into()).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    // Other /48 networks and IPv4 clients are not affected.
    assert_eq!(reg("2001:db8:2:1::1".into()).await, StatusCode::FORBIDDEN);
    assert_eq!(reg("203.0.113.1".into()).await, StatusCode::FORBIDDEN);
}

async fn token_rotation_and_reset(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let (_, cookie) = srv.login(&acme.token, &[]).await;
    assert_eq!(
        srv.with_cookie(Method::GET, "/whoami", &cookie).await.0,
        StatusCode::OK
    );
    let v = acme
        .ok(
            Method::POST,
            "/tokens/rotate",
            Some(json!({ "label": "laptop" })),
        )
        .await;
    assert_eq!(v["scope"], "user");
    let new = srv.client(v["token"].as_str().unwrap());
    assert_eq!(acme.get("/whoami").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(new.get("/whoami").await.0, StatusCode::OK);
    // The browser session made with the old token ended.
    assert_eq!(
        srv.with_cookie(Method::GET, "/whoami", &cookie).await.0,
        StatusCode::UNAUTHORIZED
    );
    // Rotating through a session works the same way.
    let (_, cookie) = srv.login(&new.token, &[]).await;
    let (s, v) = srv
        .with_cookie(Method::POST, "/tokens/rotate", &cookie)
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let newer = srv.client(v["token"].as_str().unwrap());
    assert_eq!(new.get("/whoami").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(newer.get("/whoami").await.0, StatusCode::OK);
    let (_, cookie) = srv.login(&newer.token, &[]).await;
    // Seat and node tokens cannot rotate into user tokens.
    let a = newer.seat("a").await;
    assert_eq!(
        a.status(Method::POST, "/tokens/rotate", None).await,
        StatusCode::FORBIDDEN
    );
    // The operator resets lost tokens: every user token and session ends.
    let extra = srv.client(
        newer
            .ok(Method::POST, "/tokens", Some(json!({ "scope": "user" })))
            .await["token"]
            .as_str()
            .unwrap(),
    );
    let org = srv.db.store.org_by_name("acme").await.unwrap().unwrap();
    let reset = new_token();
    srv.db
        .store
        .reset_user_tokens(org.id, &hash_token(&reset))
        .await
        .unwrap();
    for c in [&newer, &extra] {
        assert_eq!(c.get("/whoami").await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        srv.with_cookie(Method::GET, "/whoami", &cookie).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(srv.client(&reset).get("/whoami").await.0, StatusCode::OK);
    // Seat tokens are not affected.
    assert_eq!(a.get("/whoami").await.0, StatusCode::OK);
}

// --- protective limits -----------------------------------------------------------

async fn seat_and_send_limits(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.seats_per_org = Some(3);
    cfg.limits.org_send_per_sec = Some(2.0);
    cfg.limits.global_send_per_sec = Some(3.0);
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    for n in ["s1", "s2", "s3"] {
        acme.ok(Method::POST, "/seats", Some(json!({ "name": n })))
            .await;
    }
    let (s, v) = acme
        .call(Method::POST, "/seats", Some(json!({ "name": "s4" })))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "seat_limit");
    // The user seat does not count; deleting a seat makes room.
    acme.ok(Method::DELETE, "/seats/s3", None).await;
    acme.ok(Method::POST, "/seats", Some(json!({ "name": "s4" })))
        .await;

    // Per org: two per second (a burst of two); the user is not exempt.
    let msg = json!({ "to": ["s1"], "subject": "x", "body": "" });
    acme.send(msg.clone()).await;
    acme.send(msg.clone()).await;
    let resp = acme
        .http
        .post(format!("{}/v1/messages", acme.base))
        .bearer_auth(&acme.token)
        .json(&msg)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().contains_key("retry-after"));
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "org_rate_limited");
    assert!(v["error"]["retry_after_ms"].as_u64().unwrap() <= 1000);
    // Another org has its own budget, but all orgs share the global one
    // (three per second: two were taken by acme, one is left).
    beta.ok(Method::POST, "/seats", Some(json!({ "name": "x" })))
        .await;
    let bmsg = json!({ "to": ["x"], "subject": "y", "body": "" });
    beta.send(bmsg.clone()).await;
    let (s, v) = beta
        .call(Method::POST, "/messages", Some(bmsg.clone()))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "server_busy");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    beta.send(bmsg).await;
    acme.send(msg).await;
}

async fn stream_request_and_link_limits(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.org_streams = Some(2);
    cfg.limits.token_requests_per_sec = Some(5.0); // a burst of ten
    cfg.limits.link_requests_per_hour = Some(4.0); // a burst of one
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let node = acme.node().await;
    let (s1, l1) = node.stream("/events").await;
    let (s2, _l2) = acme.stream("/events").await;
    assert_eq!((s1, s2), (StatusCode::OK, StatusCode::OK));
    let (s3, _) = acme.stream("/events").await;
    assert_eq!(s3, StatusCode::TOO_MANY_REQUESTS);
    // Other orgs are not affected.
    assert_eq!(beta.stream("/events").await.0, StatusCode::OK);

    // Requests per token: the burst, then 429 with a retry time.
    let tok = acme.seat("a").await;
    let mut codes = Vec::new();
    for _ in 0..14 {
        codes.push(tok.get("/whoami").await.0);
    }
    assert!(
        codes[..10].iter().all(|c| *c == StatusCode::OK),
        "{codes:?}"
    );
    assert_eq!(codes[13], StatusCode::TOO_MANY_REQUESTS);
    // A token's limit is its own.
    assert_eq!(beta.get("/whoami").await.0, StatusCode::OK);

    // Link requests per org (also to orgs that do not exist).
    let (s, _) = beta
        .call(Method::POST, "/links", Some(json!({ "org": "nowhere" })))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = beta
        .call(Method::POST, "/links", Some(json!({ "org": "acme" })))
        .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "{v}");
    drop(l1);
}

async fn list_token_and_sign_in_limits(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.lists_per_org = Some(2);
    cfg.limits.tokens_per_org = Some(3); // the user token counts
    cfg.limits.token_requests_per_sec = Some(1.0); // a burst of two
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    for n in ["l1", "l2"] {
        acme.ok(Method::POST, "/lists", Some(json!({ "name": n })))
            .await;
    }
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let (s, v) = acme
        .call(Method::POST, "/lists", Some(json!({ "name": "l3" })))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "list_limit");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    acme.ok(Method::POST, "/seats", Some(json!({ "name": "a" })))
        .await;
    acme.ok(Method::POST, "/seats/a/tokens", None).await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let node = acme
        .ok(Method::POST, "/tokens", Some(json!({ "scope": "node" })))
        .await;
    let (s, v) = acme.call(Method::POST, "/seats/a/tokens", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["error"]["code"], "token_limit");
    tokio::time::sleep(Duration::from_millis(2100)).await;
    // Revoking makes room.
    let id = acme
        .ok(Method::GET, "/tokens", None)
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["scope"] == "node")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    acme.ok(Method::DELETE, &format!("/tokens/{id}"), None)
        .await;
    let _ = node;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    acme.ok(Method::POST, "/seats/a/tokens", None).await;
    // Other orgs are not affected.
    beta.ok(Method::POST, "/lists", Some(json!({ "name": "l1" })))
        .await;
    // Sign-ins count against the token's request rate.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let codes: Vec<StatusCode> = {
        let mut v = Vec::new();
        for _ in 0..3 {
            v.push(srv.login(&acme.token, &[]).await.0);
        }
        v
    };
    assert_eq!(
        codes,
        vec![
            StatusCode::OK,
            StatusCode::OK,
            StatusCode::TOO_MANY_REQUESTS
        ]
    );
}

/// A tiny HTTP server that records requests; `redirect` answers every
/// request with a 302 to that URL.
async fn sink(redirect: Option<String>) -> (u16, Arc<Mutex<Vec<String>>>) {
    use axum::routing::any;
    let hits: Arc<Mutex<Vec<String>>> = Arc::default();
    let h = hits.clone();
    let app = axum::Router::new().fallback(any(move |uri: axum::http::Uri| {
        let (h, redirect) = (h.clone(), redirect.clone());
        async move {
            h.lock().unwrap().push(uri.to_string());
            match redirect {
                Some(to) => axum::response::Response::builder()
                    .status(302)
                    .header("location", to)
                    .body(axum::body::Body::empty())
                    .unwrap(),
                None => axum::response::Response::builder()
                    .status(204)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            }
        }
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (port, hits)
}

async fn set_webhook_in_store(srv: &Server, org: &str, url: &str) {
    let o = srv.db.store.org_by_name(org).await.unwrap().unwrap();
    srv.db
        .store
        .update_org(
            o.id,
            &amail_store::OrgSettings {
                webhook_url: Some(Some(url.to_string())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

async fn webhook_targets_public_mode(b: Backend, t: &str) {
    let (port, hits) = sink(None).await;
    // The resolver answers `rebind.test` with loopback, as a rebinding DNS
    // server would when the webhook fires.
    let srv = start!(b, t, public(), |st: &mut AppState| {
        let mut r = amail_server::netguard::GuardedResolver::default();
        r.overrides
            .insert("rebind.test".into(), vec!["127.0.0.1".parse().unwrap()]);
        r.overrides
            .insert("private.test".into(), vec!["10.0.0.1".parse().unwrap()]);
        st.http = amail_server::netguard::webhook_client(false, Some(r));
    });
    let acme = srv.org("acme").await;
    let a = acme.seat("a").await;
    // Literal loopback, private and link-local addresses and localhost are
    // refused when saved.
    for url in [
        format!("http://127.0.0.1:{port}/hook"),
        format!("http://localhost:{port}/hook"),
        "http://10.1.2.3/hook".to_string(),
        "http://169.254.169.254/latest/meta-data".to_string(),
        "http://[::1]:9/hook".to_string(),
        "http://[::ffff:127.0.0.1]:9/hook".to_string(),
        "http://2130706433/hook".to_string(),
        // Public addresses, but not a web port.
        "http://8.8.8.8:22/hook".to_string(),
        "https://hooks.example.com:8443/hook".to_string(),
    ] {
        let (s, v) = acme
            .call(Method::PATCH, "/org", Some(json!({ "webhook_url": url })))
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{url}: {v}");
        assert_eq!(v["error"]["code"], "webhook_target");
    }
    // A name is accepted when saved and checked whenever the webhook fires:
    // resolving to loopback, nothing is sent.
    acme.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "webhook_url": "http://rebind.test/hook" })),
    )
    .await;
    a.send(json!({ "to": ["owner"], "subject": "q1", "body": "" }))
        .await;
    // A literal address stored before public mode was turned on is not
    // tried either.
    tokio::time::sleep(Duration::from_millis(400)).await;
    set_webhook_in_store(&srv, "acme", &format!("http://127.0.0.1:{port}/hook")).await;
    a.send(json!({ "to": ["owner"], "subject": "q2", "body": "" }))
        .await;
    set_webhook_in_store(&srv, "acme", &format!("http://private.test:{port}/hook")).await;
    a.send(json!({ "to": ["owner"], "subject": "q3", "body": "" }))
        .await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        hits.lock().unwrap().is_empty(),
        "{:?}",
        hits.lock().unwrap()
    );

    // The same server allows private targets when the operator says so.
    let mut cfg = public();
    cfg.webhook_private_targets = true;
    let srv = start!(b, &format!("{t}_allowed"), cfg);
    let acme = srv.org("acme").await;
    let a = acme.seat("a").await;
    acme.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "webhook_url": format!("http://127.0.0.1:{port}/hook") })),
    )
    .await;
    a.send(json!({ "to": ["owner"], "subject": "q", "body": "" }))
        .await;
    eventually("the webhook", async || hits.lock().unwrap().len() == 1).await;
}

async fn webhook_redirects_not_followed(b: Backend, t: &str) {
    let (target_port, target_hits) = sink(None).await;
    let (port, hits) = sink(Some(format!("http://127.0.0.1:{target_port}/stolen"))).await;
    let srv = start!(b, t, selfhosted());
    let acme = srv.org("acme").await;
    let a = acme.seat("a").await;
    acme.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "webhook_url": format!("http://127.0.0.1:{port}/hook") })),
    )
    .await;
    a.send(json!({ "to": ["owner"], "subject": "q", "body": "" }))
        .await;
    // A redirect is a failure: retried (three attempts), never followed.
    eventually("three attempts", async || hits.lock().unwrap().len() == 3).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(target_hits.lock().unwrap().is_empty());
}

async fn forwarded_headers_need_trusted_proxy(b: Backend, t: &str) {
    // Self-hosted behind a proxy on the same host.
    let mut cfg = selfhosted();
    cfg.trusted_proxies = vec!["127.0.0.0/8".parse().unwrap()];
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    acme.seat("a").await;
    let (s, set) = srv
        .login(&acme.token, &[("X-Forwarded-Proto", "https")])
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(set.contains("; Secure"), "{set}");
    let (_, plain) = srv.login(&acme.token, &[]).await;
    assert!(!plain.contains("Secure"), "{plain}");
    let cookie = plain.split(';').next().unwrap().to_string();
    let post = |headers: Vec<(&'static str, String)>| {
        let srv = &srv;
        let cookie = cookie.clone();
        async move {
            let mut r = srv
                .http
                .post(format!("{}/v1/messages", srv.base))
                .header("Cookie", cookie)
                .header("X-Amail-Csrf", "1")
                .json(&json!({ "to": ["a"], "subject": "x", "body": "" }));
            for (k, v) in headers {
                r = r.header(k, v);
            }
            r.send().await.unwrap().status()
        }
    };
    // The proxy names the original host: accepted from a trusted peer.
    assert_eq!(
        post(vec![
            ("Origin", "https://mail.example".into()),
            ("X-Forwarded-Host", "mail.example".into())
        ])
        .await,
        StatusCode::OK
    );
    assert_eq!(
        post(vec![
            ("Origin", "https://evil.example".into()),
            ("X-Forwarded-Host", "mail.example".into())
        ])
        .await,
        StatusCode::FORBIDDEN
    );

    // Public mode, no trusted proxy: forwarded headers mean nothing, and
    // the cookie is always Secure.
    let srv = start!(b, &format!("{t}_public"), public());
    let acme = srv.org("acme").await;
    acme.seat("a").await;
    let (_, set) = srv.login(&acme.token, &[]).await;
    assert!(set.contains("; Secure"), "{set}");
    let cookie = set.split(';').next().unwrap().to_string();
    let r = srv
        .http
        .post(format!("{}/v1/messages", srv.base))
        .header("Cookie", cookie)
        .header("X-Amail-Csrf", "1")
        .header("Origin", "https://mail.example")
        .header("X-Forwarded-Host", "mail.example")
        .json(&json!({ "to": ["a"], "subject": "x", "body": "" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
}

async fn reserved_names(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    for n in [
        "admin",
        "system",
        "amail",
        "owner",
        "operator",
        "postmaster",
    ] {
        let (s, v) = acme
            .call(Method::POST, "/seats", Some(json!({ "name": n })))
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{n}: {v}");
        assert_eq!(v["error"]["code"], "reserved_name");
        let (s, _) = acme
            .call(Method::POST, "/lists", Some(json!({ "name": n })))
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "list {n}");
    }
    // Ordinary names, including the ones `amail up` generates.
    for n in ["root", "leads", "all", "be-lead"] {
        acme.ok(
            Method::POST,
            "/lists",
            Some(json!({ "name": format!("{n}-l") })),
        )
        .await;
        acme.ok(Method::POST, "/seats", Some(json!({ "name": n })))
            .await;
    }
    // Self-hosted servers keep accepting them (and existing data is never
    // affected).
    let srv = start!(b, &format!("{t}_selfhosted"), selfhosted());
    let acme = srv.org("acme").await;
    acme.ok(Method::POST, "/seats", Some(json!({ "name": "admin" })))
        .await;
}

// --- the operator ------------------------------------------------------------------

async fn operator_view_and_suspension(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    acme.seat("b").await;
    let x = beta.seat("x").await;
    acme.link(&beta, "beta", "acme").await;
    a.send(json!({ "to": ["b"], "subject": "top secret subject", "body": "top secret body" }))
        .await;
    let node = acme.node().await;
    let (_, node_events) = node.stream("/events").await;
    let op = srv.operator().await;

    let stats = op.ok(Method::GET, "/operator/stats", None).await;
    assert_eq!(stats["orgs"], 2);
    assert_eq!(stats["seats"], 3);
    assert_eq!(stats["nodes_online"], 1);
    assert_eq!(stats["mail_last_hour"], 1);
    assert_eq!(stats["list"][0]["name"], "acme");
    assert_eq!(stats["list"][0]["seats"], 2);
    assert_eq!(stats["list"][0]["messages"], 1);
    assert_eq!(stats["list"][0]["nodes"], 1);
    assert!(stats["list"][0]["last_activity"].is_string());
    // Counts and names only, never mail.
    assert!(!stats.to_string().contains("secret"), "{stats}");
    // An operator has no org and no mail; users cannot use operator
    // endpoints.
    assert_eq!(op.get("/inbox").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        op.get("/archive/messages").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        op.get("/seats/a/transcript").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        op.ok(Method::GET, "/whoami", None).await,
        json!({ "org": "", "scope": "operator" })
    );
    for c in [&acme, &a, &node] {
        assert_eq!(c.get("/operator/stats").await.0, StatusCode::FORBIDDEN);
        assert_eq!(
            c.status(Method::POST, "/operator/orgs/beta/suspend", None)
                .await,
            StatusCode::FORBIDDEN
        );
    }
    // Operators sign in to the web interface like users.
    let (s, cookie) = srv.login(&op.token, &[]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        srv.with_cookie(Method::GET, "/operator/stats", &cookie)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        srv.with_cookie(Method::GET, "/whoami", &cookie).await.1["scope"],
        "operator"
    );
    let (_, user_cookie) = srv.login(&acme.token, &[]).await;

    // Suspend: tokens and sessions stop working, open streams end, mail to
    // the org is refused, data stays.
    op.ok(Method::POST, "/operator/orgs/acme/suspend", None)
        .await;
    for c in [&acme, &a, &node] {
        let (s, v) = c.get("/whoami").await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(v["error"]["code"], "org_suspended");
    }
    assert_eq!(
        srv.with_cookie(Method::GET, "/whoami", &user_cookie)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    eventually("the node's stream to end", async || {
        joined(&node_events).contains("<closed>")
    })
    .await;
    let (s, v) = x
        .call(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["a@acme"], "subject": "hi", "body": "" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["rejected"][0]["reason"], "not_linked");
    let stats = op.ok(Method::GET, "/operator/stats", None).await;
    assert_eq!(stats["suspended"], 1);
    assert!(
        stats["list"][0]["suspended_at"].is_string()
            || stats["list"][1]["suspended_at"].is_string()
    );

    op.ok(Method::POST, "/operator/orgs/acme/resume", None)
        .await;
    assert_eq!(a.get("/whoami").await.0, StatusCode::OK);
    x.send(json!({ "to": ["a@acme"], "subject": "hi", "body": "" }))
        .await;
    assert_eq!(
        op.status(Method::POST, "/operator/orgs/nowhere/suspend", None)
            .await,
        StatusCode::NOT_FOUND
    );
}

/// Suspending on the database (`amail admin org suspend`) ends open
/// streams too, at the next sweep.
async fn operator_deletes_org(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    let x = beta.seat("x").await;
    acme.link(&beta, "beta", "acme").await;
    let to_beta = a
        .send(json!({ "to": ["x@beta"], "subject": "from acme", "body": "" }))
        .await;
    let reply = x
        .send(json!({ "to": ["a@acme"], "subject": "Re: from acme", "body": "", "in_reply_to": to_beta }))
        .await;
    let beta_own = x
        .send(json!({ "to": ["owner"], "subject": "beta internal", "body": "" }))
        .await;
    let (_, user_events) = acme.stream("/events").await;
    let org = srv.db.store.org_by_name("acme").await.unwrap().unwrap();
    srv.db
        .store
        .set_org_suspended(org.id, Some(amail_store::now_ms()))
        .await
        .unwrap();
    eventually("the sweep to end acme's stream", async || {
        joined(&user_events).contains("<closed>")
    })
    .await;

    let op = srv.operator().await;
    assert_eq!(
        op.status(
            Method::DELETE,
            "/operator/orgs/acme",
            Some(json!({ "confirm": "beta" }))
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        op.status(
            Method::DELETE,
            "/operator/orgs/acme",
            Some(json!({ "confirm": "acme" }))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert!(srv.db.store.org_by_name("acme").await.unwrap().is_none());
    assert_eq!(a.get("/whoami").await.0, StatusCode::UNAUTHORIZED);
    // Mail acme sent stays with beta, from a deleted org; beta's reply
    // keeps its parent; beta's own mail is untouched; the link is gone.
    let m = x
        .ok(Method::GET, &format!("/messages/{to_beta}"), None)
        .await;
    assert_eq!(m["message"]["from"], "a@acme (deleted org)", "{m}");
    let r = x.ok(Method::GET, &format!("/messages/{reply}"), None).await;
    assert_eq!(r["message"]["in_reply_to"], to_beta, "{r}");
    x.ok(Method::GET, &format!("/messages/{beta_own}"), None)
        .await;
    assert_eq!(beta.ok(Method::GET, "/links", None).await, json!([]));
    let stats = op.ok(Method::GET, "/operator/stats", None).await;
    assert_eq!(stats["orgs"], 1);
    // The name can be registered again.
    let code = srv.invite(None).await;
    assert_eq!(
        srv.register(json!({ "invite": code, "org": "acme" }))
            .await
            .0,
        StatusCode::CREATED
    );
}

async fn user_deletes_own_org(b: Backend, t: &str) {
    let srv = start!(b, t, selfhosted());
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    let x = beta.seat("x").await;
    acme.link(&beta, "beta", "acme").await;
    acme.ok(Method::POST, "/lists", Some(json!({ "name": "mixed" })))
        .await;
    acme.ok(Method::PUT, "/lists/mixed/members/a", None).await;
    beta.ok(Method::POST, "/lists", Some(json!({ "name": "partners" })))
        .await;
    beta.ok(Method::PUT, "/lists/partners/members/a@acme", None)
        .await;
    beta.ok(Method::PUT, "/lists/partners/members/x", None)
        .await;
    a.send(json!({ "to": ["owner"], "subject": "searchable zebra", "body": "zebra body" }))
        .await;
    x.send(json!({ "to": ["a@acme"], "subject": "to acme", "body": "" }))
        .await;
    let (_, cookie) = srv.login(&acme.token, &[]).await;

    let (s, v) = acme
        .call(Method::DELETE, "/org", Some(json!({ "confirm": "acm" })))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["code"], "confirm");
    assert_eq!(
        a.status(Method::DELETE, "/org", Some(json!({ "confirm": "acme" })))
            .await,
        StatusCode::FORBIDDEN
    );
    let resp = srv
        .http
        .delete(format!("{}/v1/org", srv.base))
        .header("Cookie", cookie.split(';').next().unwrap())
        .header("X-Amail-Csrf", "1")
        .json(&json!({ "confirm": "acme" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        resp.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    for c in [&acme, &a] {
        assert_eq!(c.get("/whoami").await.0, StatusCode::UNAUTHORIZED);
    }
    // Beta: its list lost the external member; its mail to acme stays in
    // its archive (sent by beta), now delivered to nobody in acme.
    let l = beta.ok(Method::GET, "/lists/partners", None).await;
    assert_eq!(
        l["members"],
        json!([{ "address": "x", "mode": "immediate" }])
    );
    let arch = beta.ok(Method::GET, "/archive/messages", None).await;
    let subjects: Vec<&str> = arch["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, vec!["to acme"]);
    // The search index forgot acme's mail (SQLite's index is separate).
    let gamma = srv.org("gamma").await;
    let s = gamma.ok(Method::GET, "/archive/search?q=zebra", None).await;
    assert_eq!(s["messages"], json!([]));
    // The name is free again; nothing of the old org comes back with it.
    let acme2 = srv.org("acme").await;
    assert_eq!(
        acme2.ok(Method::GET, "/archive/messages", None).await["messages"],
        json!([])
    );
    let seats = acme2.ok(Method::GET, "/seats", None).await;
    assert_eq!(seats.as_array().unwrap().len(), 1);
}

async fn retention_clears_bodies(b: Backend, t: &str) {
    let mut cfg = selfhosted();
    cfg.retention_days = Some(0);
    cfg.retention_every = Duration::from_millis(200);
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let a = acme.seat("a").await;
    let id = a
        .send(json!({ "to": ["owner"], "subject": "keep the subject", "body": "quokka body", "meta": { "branch": "x" } }))
        .await;
    let reply = acme
        .send(json!({ "to": ["a"], "subject": "Re: keep the subject", "body": "reply", "in_reply_to": id }))
        .await;
    eventually("the bodies to expire", async || {
        acme.ok(Method::GET, &format!("/messages/{id}?peek=1"), None)
            .await["message"]["expired"]
            == true
    })
    .await;
    let m = acme
        .ok(Method::GET, &format!("/messages/{id}?peek=1"), None)
        .await["message"]
        .clone();
    assert_eq!(m["body"], "");
    assert_eq!(m["subject"], "keep the subject");
    assert_eq!(m["meta"]["branch"], "x");
    // Threads keep their structure.
    let th = acme.ok(Method::GET, &format!("/threads/{id}"), None).await;
    assert_eq!(th["messages"].as_array().unwrap().len(), 2);
    assert_eq!(th["messages"][1]["id"], reply);
    assert_eq!(th["messages"][1]["in_reply_to"], id);
    // The body is no longer found; the subject is.
    let s = acme.ok(Method::GET, "/archive/search?q=quokka", None).await;
    assert_eq!(s["messages"], json!([]));
    let s = acme
        .ok(Method::GET, "/archive/search?q=subject", None)
        .await;
    assert_eq!(s["messages"].as_array().unwrap().len(), 2);
    let (_, info) = srv.raw(Method::GET, "/info", None, &[]).await;
    assert_eq!(info["retention_days"], 0);
}

// --- cross-org access ------------------------------------------------------------
//
// Org `acme` tries to reach org `beta`'s resources by id or by name. Both
// orgs have a seat called `same`, so that a lookup by name in the wrong org
// would be noticed.

struct TwoOrgs {
    srv: Server,
    acme: Client,
    a: Client,
    beta: Client,
    x: Client,
    /// A message internal to beta.
    beta_msg: i64,
    beta_thread: i64,
}

async fn two_orgs(b: Backend, t: &str) -> Option<TwoOrgs> {
    let srv = Server::start(b, t, selfhosted(), |st: &mut AppState| {
        st.mirror_config.lease = Duration::from_millis(900);
    })
    .await?;
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    acme.seat("same").await;
    let x = beta.seat("x").await;
    beta.seat("same").await;
    beta.seat("y").await;
    let beta_msg = x
        .send(json!({ "to": ["y", "owner"], "subject": "beta secret", "body": "beta secret body", "ack_required": true }))
        .await;
    Some(TwoOrgs {
        srv,
        acme,
        a,
        beta,
        x,
        beta_msg,
        beta_thread: beta_msg,
    })
}

macro_rules! two_orgs {
    ($b:expr, $t:expr) => {
        match two_orgs($b, $t).await {
            Some(o) => o,
            None => return,
        }
    };
}

async fn cross_org_messages(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let m = o.beta_msg;
    for c in [&o.acme, &o.a] {
        for (method, path) in [
            (Method::GET, format!("/messages/{m}")),
            (Method::GET, format!("/messages/{m}?peek=1")),
            (Method::GET, format!("/messages/{m}/status")),
            (Method::POST, format!("/messages/{m}/ack")),
            (Method::POST, format!("/messages/{m}/read")),
        ] {
            let (s, v) = c.call(method.clone(), &path, None).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
            assert!(!v.to_string().contains("secret"));
        }
        // Replying to it (which would join its thread) is refused.
        let (s, _) = c
            .call(
                Method::POST,
                "/messages",
                Some(json!({ "to": ["a"], "subject": "Re", "body": "", "in_reply_to": m })),
            )
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
    // Once linked, a message beta sends to acme is visible to acme, but
    // beta's internal reply in the same thread is not.
    o.acme.link(&o.beta, "beta", "acme").await;
    let shared =
        o.x.send(json!({ "to": ["a@acme"], "subject": "shared", "body": "" }))
            .await;
    let internal = o
        .x
        .send(json!({ "to": ["y"], "subject": "Re: shared (internal)", "body": "beta only", "in_reply_to": shared }))
        .await;
    o.a.ok(Method::GET, &format!("/messages/{shared}"), None)
        .await;
    for c in [&o.acme, &o.a] {
        assert_eq!(
            c.get(&format!("/messages/{internal}")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    // The recipient org cannot see the sender's delivery status.
    assert_eq!(
        o.acme.get(&format!("/messages/{shared}/status")).await.0,
        StatusCode::FORBIDDEN
    );
}

async fn cross_org_threads_archive_search(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let th = o.beta_thread;
    for c in [&o.acme, &o.a] {
        assert_eq!(
            c.get(&format!("/threads/{th}")).await.0,
            StatusCode::NOT_FOUND
        );
    }
    let arch = o
        .acme
        .ok(Method::GET, &format!("/archive/messages?thread={th}"), None)
        .await;
    assert_eq!(arch["messages"], json!([]));
    let arch = o
        .acme
        .ok(Method::GET, "/archive/messages?from=x@beta", None)
        .await;
    assert_eq!(arch["messages"], json!([]));
    let arch = o
        .acme
        .ok(Method::GET, "/archive/messages?to=y@beta", None)
        .await;
    assert_eq!(arch["messages"], json!([]));
    for q in ["secret", "be"] {
        let s = o
            .acme
            .ok(Method::GET, &format!("/archive/search?q={q}"), None)
            .await;
        assert_eq!(s["messages"], json!([]), "{q}");
    }
    assert_eq!(o.acme.get("/sent?seat=x").await.0, StatusCode::NOT_FOUND);
    // Linked: the thread shows only what acme may see.
    o.acme.link(&o.beta, "beta", "acme").await;
    let shared =
        o.x.send(json!({ "to": ["a@acme"], "subject": "shared", "body": "", "in_reply_to": th }))
            .await;
    let v = o
        .acme
        .ok(Method::GET, &format!("/threads/{th}"), None)
        .await;
    let ids: Vec<i64> = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![shared]);
}

async fn cross_org_mailboxes_and_deliveries(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let node = o.acme.node().await;
    for c in [&o.acme, &node] {
        for path in [
            "/inbox?seat=x",
            "/changes?seat=x",
            "/pending?seat=x",
            "/events?seat=x",
        ] {
            assert_eq!(c.get(path).await.0, StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(
            c.status(
                Method::POST,
                "/claim?seat=x",
                Some(json!({ "classes": ["to"], "via": "test" }))
            )
            .await,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(o.acme.get("/counts?seat=x").await.0, StatusCode::NOT_FOUND);
    // `same` names acme's own seat: beta's mailbox of that name stays out
    // of reach.
    o.beta
        .send(json!({ "to": ["same"], "subject": "for beta's same", "body": "" }))
        .await;
    let inbox = o.acme.ok(Method::GET, "/inbox?seat=same", None).await;
    assert_eq!(inbox["entries"], json!([]));
    let claim = node
        .ok(
            Method::POST,
            "/claim?seat=same",
            Some(json!({ "classes": ["to", "cc"], "via": "test" })),
        )
        .await;
    assert_eq!(claim["entries"], json!([]));
    let pending = node.ok(Method::GET, "/pending", None).await;
    let seats: Vec<&str> = pending["seats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["seat"].as_str().unwrap())
        .collect();
    assert_eq!(seats, vec!["a", "same"]);
    // The event stream of acme carries no beta mailbox events.
    let (_, lines) = o.acme.stream("/events").await;
    o.x.send(json!({ "to": ["y"], "subject": "beta again", "body": "" }))
        .await;
    o.a.send(json!({ "to": ["same"], "subject": "acme", "body": "" }))
        .await;
    eventually("acme's event", async || {
        joined(&lines).contains("\"seat\":\"same\"")
    })
    .await;
    assert!(!joined(&lines).contains("\"seat\":\"y\""));
    // Beta's delivery stays queued: nothing in acme touched it.
    let st =
        o.x.ok(
            Method::GET,
            &format!("/messages/{}/status", o.beta_msg),
            None,
        )
        .await;
    assert!(
        st["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["state"] == "queued")
    );
}

async fn cross_org_seats(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let node = o.acme.node().await;
    for (method, path, body) in [
        (Method::GET, "/seats/x", None),
        (Method::PATCH, "/seats/x", Some(json!({ "role": "pwned" }))),
        (Method::DELETE, "/seats/x", None),
        (Method::POST, "/seats/x/tokens", None),
        (Method::GET, "/seats/x/launch", None),
        (Method::PUT, "/focus?seat=x", Some(json!({ "for_s": 60 }))),
        (Method::DELETE, "/focus?seat=x", None),
        (Method::POST, "/seats/x/sleep", None),
        (Method::POST, "/seats/x/wake", None),
    ] {
        let (s, v) = o.acme.call(method.clone(), path, body.clone()).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{method} {path}: {v}");
    }
    assert_eq!(node.get("/seats/x/launch").await.0, StatusCode::NOT_FOUND);
    // Beta's seat is unchanged and still has no launch token.
    let x = o.beta.ok(Method::GET, "/seats/x", None).await;
    assert!(x.get("role").is_none());
    let toks = o.beta.ok(Method::GET, "/tokens", None).await;
    assert!(
        toks.as_array()
            .unwrap()
            .iter()
            .all(|t| t["label"] != "launch")
    );
    // Acme's directory lists its own seats only.
    let names: Vec<String> =
        o.a.ok(Method::GET, "/seats", None)
            .await
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_string())
            .collect();
    assert_eq!(names, vec!["a", "owner", "same"]);
}

async fn cross_org_lists(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    o.beta
        .ok(
            Method::POST,
            "/lists",
            Some(json!({ "name": "team", "post_policy": "members" })),
        )
        .await;
    o.beta.ok(Method::PUT, "/lists/team/members/x", None).await;
    o.beta
        .ok(
            Method::POST,
            "/lists",
            Some(json!({ "name": "open", "post_policy": "anyone" })),
        )
        .await;
    o.beta.ok(Method::PUT, "/lists/open/members/y", None).await;
    for (method, path) in [
        (Method::GET, "/lists/team"),
        (Method::DELETE, "/lists/team"),
        (Method::PUT, "/lists/team/members/a"),
        (Method::DELETE, "/lists/team/members/x"),
    ] {
        assert_eq!(
            o.acme.status(method.clone(), path, None).await,
            StatusCode::NOT_FOUND,
            "{method} {path}"
        );
    }
    let mine = o.acme.ok(Method::GET, "/lists", None).await;
    assert_eq!(mine, json!([]));
    // Not linked: beta's lists cannot be mailed or joined.
    for to in ["team@beta", "open@beta"] {
        let (s, v) =
            o.a.call(
                Method::POST,
                "/messages",
                Some(json!({ "to": [to], "subject": "x", "body": "" })),
            )
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["error"]["rejected"][0]["reason"], "not_linked");
    }
    o.acme
        .ok(Method::POST, "/lists", Some(json!({ "name": "mine" })))
        .await;
    assert_eq!(
        o.acme
            .status(Method::PUT, "/lists/mine/members/x@beta", None)
            .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // Linked: only `anyone` lists take mail from outside.
    o.acme.link(&o.beta, "beta", "acme").await;
    let (s, v) =
        o.a.call(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["team@beta"], "subject": "x", "body": "" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(v["error"]["rejected"][0]["reason"], "post_denied");
    o.a.send(json!({ "to": ["open@beta"], "subject": "x", "body": "" }))
        .await;
    let team = o.beta.ok(Method::GET, "/lists/team", None).await;
    assert_eq!(team["members"].as_array().unwrap().len(), 1);
}

async fn cross_org_tokens(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let beta_tokens = o.beta.ok(Method::GET, "/tokens", None).await;
    let ids: Vec<i64> = beta_tokens
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_i64().unwrap())
        .collect();
    assert!(!ids.is_empty());
    for id in &ids {
        assert_eq!(
            o.acme
                .status(Method::DELETE, &format!("/tokens/{id}"), None)
                .await,
            StatusCode::NOT_FOUND
        );
    }
    // All of beta's tokens still work.
    assert_eq!(o.beta.get("/whoami").await.0, StatusCode::OK);
    assert_eq!(o.x.get("/whoami").await.0, StatusCode::OK);
    let mine: Vec<i64> = o
        .acme
        .ok(Method::GET, "/tokens", None)
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_i64().unwrap())
        .collect();
    assert!(mine.iter().all(|i| !ids.contains(i)));
    // Rotation revokes the caller's own token only.
    o.acme.ok(Method::POST, "/tokens/rotate", None).await;
    assert_eq!(o.beta.get("/whoami").await.0, StatusCode::OK);
}

async fn cross_org_links(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let gamma = o.srv.org("gamma").await;
    // Acme asks beta; neither acme itself nor a third org can accept it.
    o.acme
        .ok(Method::POST, "/links", Some(json!({ "org": "beta" })))
        .await;
    assert_eq!(
        o.acme
            .status(Method::POST, "/links/beta/accept", None)
            .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        gamma.status(Method::POST, "/links/acme/accept", None).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        gamma.status(Method::POST, "/links/beta/accept", None).await,
        StatusCode::NOT_FOUND
    );
    // Still pending: mail across is refused.
    let (s, v) =
        o.a.call(
            Method::POST,
            "/messages",
            Some(json!({ "to": ["x@beta"], "subject": "x", "body": "" })),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    // Seat tokens cannot accept for their org.
    assert_eq!(
        o.x.status(Method::POST, "/links/acme/accept", None).await,
        StatusCode::FORBIDDEN
    );
    o.beta.ok(Method::POST, "/links/acme/accept", None).await;
    // A third org sees nothing of the link and cannot end it.
    assert_eq!(gamma.ok(Method::GET, "/links", None).await, json!([]));
    assert_eq!(
        gamma.status(Method::DELETE, "/links/beta", None).await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        gamma.status(Method::DELETE, "/links/acme", None).await,
        StatusCode::NOT_FOUND
    );
    o.a.send(json!({ "to": ["x@beta"], "subject": "still linked", "body": "" }))
        .await;
    // Mail from gamma to either is refused.
    let g = gamma.seat("g").await;
    for to in ["x@beta", "a@acme"] {
        let (s, v) = g
            .call(
                Method::POST,
                "/messages",
                Some(json!({ "to": [to], "subject": "x", "body": "" })),
            )
            .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{to}: {v}");
        assert_eq!(v["error"]["rejected"][0]["reason"], "not_linked");
    }
}

async fn cross_org_mirrors(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    assert_eq!(
        o.acme.get("/seats/x/transcript").await.0,
        StatusCode::NOT_FOUND
    );
    // Beta watches its `same`; acme's node posts for acme's `same`.
    let beta_node = o.beta.node().await;
    let acme_node = o.acme.node().await;
    let (_, beta_cmds) = beta_node.stream("/events").await;
    let (s, watched) = o.beta.stream("/seats/same/transcript").await;
    assert_eq!(s, StatusCode::OK);
    eventually("beta's node to be asked", async || {
        joined(&beta_cmds).contains("\"op\":\"transcript\"")
    })
    .await;
    let post =
        json!({ "events": [{ "kind": "text", "role": "assistant", "text": "acme's transcript" }] });
    let v = acme_node
        .ok(Method::POST, "/seats/same/transcript", Some(post))
        .await;
    assert_eq!(v["subscribers"], 0);
    let v = acme_node
        .ok(
            Method::POST,
            "/seats/x/transcript",
            Some(json!({ "events": [{ "kind": "text", "role": "assistant", "text": "acme again" }] })),
        )
        .await;
    assert_eq!(v["subscribers"], 0);
    // Beta's own node reaches the watcher.
    let v = beta_node
        .ok(
            Method::POST,
            "/seats/same/transcript",
            Some(json!({ "events": [{ "kind": "text", "role": "assistant", "text": "beta's transcript" }] })),
        )
        .await;
    assert_eq!(v["subscribers"], 1);
    eventually("beta's event", async || {
        joined(&watched).contains("beta's transcript")
    })
    .await;
    assert!(!joined(&watched).contains("acme"));
}

async fn cross_org_node_commands_and_presence(b: Backend, t: &str) {
    let o = two_orgs!(b, t);
    let beta_node = o.beta.node().await;
    let acme_node = o.acme.node().await;
    let (_, beta_cmds) = beta_node.stream("/events").await;
    let (_, acme_cmds) = acme_node.stream("/events").await;
    // Acme's own `same`: the command goes to acme's node only.
    assert_eq!(
        o.acme.status(Method::POST, "/seats/same/sleep", None).await,
        StatusCode::ACCEPTED
    );
    eventually("acme's node to get it", async || {
        joined(&acme_cmds).contains("\"op\":\"sleep\"")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!joined(&beta_cmds).contains("\"op\":\"sleep\""));
    // Acme's node reports presence for beta's seat names: dropped.
    let v = acme_node
        .ok(
            Method::POST,
            "/presence",
            Some(json!({ "seats": [{ "seat": "x", "state": "busy" }, { "seat": "same", "state": "idle" }] })),
        )
        .await;
    assert_eq!(v["accepted"], 1);
    let x = o.beta.ok(Method::GET, "/seats/x", None).await;
    assert!(x.get("presence").is_none(), "{x}");
    let same = o.beta.ok(Method::GET, "/seats/same", None).await;
    assert!(same.get("presence").is_none(), "{same}");
    let same = o.acme.ok(Method::GET, "/seats/same", None).await;
    assert_eq!(same["presence"]["state"], "idle");
    // Acme's node cannot send mail at all.
    assert_eq!(
        acme_node
            .status(
                Method::POST,
                "/messages",
                Some(json!({ "to": ["x@beta"], "subject": "x", "body": "" }))
            )
            .await,
        StatusCode::FORBIDDEN
    );
    let _ = &o.srv;
}

/// A link request, its listing, withdrawing it and accepting look the same
/// whether or not an org of that name exists.
async fn link_requests_do_not_reveal_org_names(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.link_requests_per_hour = None;
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let _beta = srv.org("beta").await;
    for name in ["beta", "nosuchorg"] {
        let (s, v) = acme
            .call(Method::POST, "/links", Some(json!({ "org": name })))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        assert_eq!(v, json!({ "org": name, "state": "requested" }));
        // Asking again answers the same.
        let (_, v2) = acme
            .call(Method::POST, "/links", Some(json!({ "org": name })))
            .await;
        assert_eq!(v, v2);
    }
    assert_eq!(
        acme.ok(Method::GET, "/links", None).await,
        json!([
            { "org": "beta", "state": "requested" },
            { "org": "nosuchorg", "state": "requested" },
        ])
    );
    // Accepting where nothing was asked: one answer for both.
    let (s1, v1) = acme.call(Method::POST, "/links/beta/accept", None).await;
    let (s2, v2) = acme
        .call(Method::POST, "/links/nosuchorg/accept", None)
        .await;
    assert_eq!((s1, &v1["error"]), (s2, &v2["error"]));
    // Withdrawing works for both, then neither is known.
    for name in ["beta", "nosuchorg"] {
        assert_eq!(
            acme.call(Method::DELETE, &format!("/links/{name}"), None)
                .await
                .0,
            StatusCode::NO_CONTENT
        );
        let (s, v) = acme
            .call(Method::DELETE, &format!("/links/{name}"), None)
            .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["message"], format!("no link with {name:?}"));
    }
    assert_eq!(acme.ok(Method::GET, "/links", None).await, json!([]));
    // A request to a name that is registered later does not carry over,
    // and it stays in the requesting org's list, so that org cannot watch
    // for the name being registered.
    acme.ok(Method::POST, "/links", Some(json!({ "org": "later" })))
        .await;
    let later = srv.org("later").await;
    assert_eq!(later.ok(Method::GET, "/links", None).await, json!([]));
    let requested = json!([{ "org": "later", "state": "requested" }]);
    assert_eq!(acme.ok(Method::GET, "/links", None).await, requested);
    // Asking again answers the same, and now reaches the org.
    assert_eq!(
        acme.ok(Method::POST, "/links", Some(json!({ "org": "later" })))
            .await,
        json!({ "org": "later", "state": "requested" })
    );
    assert_eq!(acme.ok(Method::GET, "/links", None).await, requested);
    assert_eq!(
        later.ok(Method::GET, "/links", None).await,
        json!([{ "org": "acme", "state": "invited" }])
    );
    later.ok(Method::POST, "/links/acme/accept", None).await;
    assert_eq!(
        acme.ok(Method::GET, "/links", None).await,
        json!([{ "org": "later", "state": "active" }])
    );
    // A request to a name registered later, where the new org asks first:
    // one entry, the invitation.
    acme.ok(Method::POST, "/links", Some(json!({ "org": "later2" })))
        .await;
    let later2 = srv.org("later2").await;
    later2
        .ok(Method::POST, "/links", Some(json!({ "org": "acme" })))
        .await;
    assert_eq!(
        acme.ok(Method::GET, "/links", None).await,
        json!([
            { "org": "later", "state": "active" },
            { "org": "later2", "state": "invited" },
        ])
    );
    acme.ok(Method::POST, "/links/later2/accept", None).await;
    assert_eq!(
        acme.ok(Method::GET, "/links", None).await,
        json!([
            { "org": "later", "state": "active" },
            { "org": "later2", "state": "active" },
        ])
    );
}

/// An invite can be refused with `org_exists` only a few times; then it is
/// spent, so one invite cannot be used to test many org names.
async fn invite_name_conflicts_are_counted(b: Backend, t: &str) {
    let mut cfg = public();
    cfg.limits.registrations_per_hour = None;
    let srv = start!(b, t, cfg);
    let _taken = srv.org("taken").await;
    let code = srv.invite(None).await;
    let max = amail_proto::limits::MAX_INVITE_NAME_CONFLICTS;
    for _ in 0..max {
        let (s, v) = srv
            .register(json!({ "invite": code, "org": "taken" }))
            .await;
        assert_eq!(s, StatusCode::CONFLICT, "{v}");
        assert_eq!(v["error"]["code"], "org_exists");
    }
    // Spent: taken and free names get the answer of an unknown code.
    for org in ["taken", "free"] {
        let (s, v) = srv.register(json!({ "invite": code, "org": org })).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{org}: {v}");
        assert_eq!(v["error"]["code"], "invalid_invite");
    }
    let inv = srv.db.store.invites().await.unwrap();
    assert_eq!(inv[0].name_conflicts, max);
    assert_eq!(inv[0].used_at, None);
    // Fewer conflicts leave the invite usable.
    let code2 = srv.invite(None).await;
    for _ in 1..max {
        assert_eq!(
            srv.register(json!({ "invite": code2, "org": "taken" }))
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    assert_eq!(
        srv.register(json!({ "invite": code2, "org": "free" }))
            .await
            .0,
        StatusCode::CREATED
    );
}

/// Link requests that were not accepted are dropped after
/// `LINK_REQUEST_DAYS`, to existing orgs and to unknown names alike.
async fn link_requests_expire(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let gamma = srv.org("gamma").await;
    for name in ["beta", "nosuchorg", "gamma"] {
        acme.ok(Method::POST, "/links", Some(json!({ "org": name })))
            .await;
    }
    gamma.ok(Method::POST, "/links/acme/accept", None).await;
    // Everything was made more than LINK_REQUEST_DAYS ago.
    let age = (amail_proto::limits::LINK_REQUEST_DAYS + 1) * 86_400_000;
    srv.db
        .exec(&[format!("UPDATE links SET created_at = created_at - {age}")])
        .await;
    srv.db
        .exec(&[format!(
            "UPDATE link_requests SET created_at = created_at - {age}"
        )])
        .await;
    srv.db.store.cleanup().await.unwrap();
    // The active link stays; both pending requests are gone.
    assert_eq!(
        acme.ok(Method::GET, "/links", None).await,
        json!([{ "org": "gamma", "state": "active" }])
    );
    assert_eq!(beta.ok(Method::GET, "/links", None).await, json!([]));
}

/// An org has a bounded number of webhook notices in flight; further ones
/// are dropped while a slow receiver holds them.
async fn webhooks_in_flight_per_org(b: Backend, t: &str) {
    use axum::routing::any;
    let hits: Arc<Mutex<usize>> = Arc::default();
    let h = hits.clone();
    let app = axum::Router::new().fallback(any(move || {
        let h = h.clone();
        async move {
            *h.lock().unwrap() += 1;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            StatusCode::NO_CONTENT
        }
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let mut cfg = public();
    cfg.webhook_private_targets = true;
    cfg.limits.org_webhooks_in_flight = Some(2);
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let a = acme.seat("a").await;
    acme.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "webhook_url": format!("http://127.0.0.1:{port}/hook") })),
    )
    .await;
    for i in 0..6 {
        a.send(json!({ "to": ["owner"], "subject": format!("m{i}"), "body": "" }))
            .await;
    }
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(*hits.lock().unwrap(), 2);
    // Once those are done, notices go out again.
    a.send(json!({ "to": ["owner"], "subject": "later", "body": "" }))
        .await;
    eventually("the next webhook", async || *hits.lock().unwrap() == 3).await;
}

/// A linked org's burst of mail cannot use all of the receiving org's
/// webhook notices in flight: each other org may hold a quarter of them,
/// and the org's own mail has its own share.
async fn webhooks_in_flight_fair_between_orgs(b: Backend, t: &str) {
    use axum::routing::any;
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let g = got.clone();
    let app = axum::Router::new().fallback(any(move |body: String| {
        let g = g.clone();
        async move {
            let v: Value = serde_json::from_str(&body).unwrap();
            g.lock()
                .unwrap()
                .push(v["message"]["subject"].as_str().unwrap().to_string());
            tokio::time::sleep(Duration::from_millis(3000)).await;
            StatusCode::NO_CONTENT
        }
    }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let mut cfg = public();
    cfg.webhook_private_targets = true;
    cfg.limits.org_webhooks_in_flight = Some(4);
    cfg.limits.link_requests_per_hour = None;
    let srv = start!(b, t, cfg);
    let acme = srv.org("acme").await;
    let a = acme.seat("a").await;
    let beta = srv.org("beta").await;
    let gamma = srv.org("gamma").await;
    for (other, name) in [(&beta, "beta"), (&gamma, "gamma")] {
        other
            .ok(Method::POST, "/links", Some(json!({ "org": "acme" })))
            .await;
        acme.ok(Method::POST, &format!("/links/{name}/accept"), None)
            .await;
    }
    acme.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "webhook_url": format!("http://127.0.0.1:{port}/hook") })),
    )
    .await;
    for i in 0..6 {
        beta.send(json!({ "to": ["owner@acme"], "subject": format!("beta{i}"), "body": "" }))
            .await;
    }
    gamma
        .send(json!({ "to": ["owner@acme"], "subject": "gamma", "body": "" }))
        .await;
    for i in 0..2 {
        a.send(json!({ "to": ["owner"], "subject": format!("own{i}"), "body": "" }))
            .await;
    }
    eventually("the notices", async || got.lock().unwrap().len() >= 4).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let mut got = got.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, ["beta0", "gamma", "own0", "own1"]);
}

/// Org settings have upper bounds: in public mode all of them, on a
/// self-hosted server the times.
async fn org_settings_bounded(b: Backend, t: &str) {
    let srv = start!(b, t, public());
    let acme = srv.org("acme").await;
    for (k, too_big, ok) in [
        ("send_rate_per_min", 601, 600),
        ("send_burst", 1_000_000, 600),
        ("max_recipients", 1_000_000, 1000),
        ("max_thread_depth", i64::MAX, 1000),
        ("ack_timeout_min", i64::MAX, 10_080),
        ("focus_max_min", 10_081, 10_080),
        ("digest_interval_min", 1_000_000, 60),
    ] {
        let (s, v) = acme
            .call(Method::PATCH, "/org", Some(json!({ k: too_big })))
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{k}: {v}");
        let v = acme.ok(Method::PATCH, "/org", Some(json!({ k: ok }))).await;
        assert_eq!(v[k], ok);
    }
    let srv = start!(b, &format!("{t}_self"), selfhosted());
    let acme = srv.org("acme").await;
    acme.ok(
        Method::PATCH,
        "/org",
        Some(json!({ "send_burst": 1_000_000 })),
    )
    .await;
    assert_eq!(
        acme.call(
            Method::PATCH,
            "/org",
            Some(json!({ "ack_timeout_min": i64::MAX }))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
}
