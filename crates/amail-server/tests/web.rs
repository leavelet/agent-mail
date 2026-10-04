//! HTTP tests of what the web interface needs (M4): browser sessions and
//! CSRF, search, the "needs you" queue, overview and counts, webhooks, seat
//! commands, the transcript relay and the embedded app. Both backends
//! (Postgres only when `AMAIL_TEST_PG_URL` is set).

use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

macro_rules! web_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    if let Some(srv) = super::Server::start(amail_store::testutil::Backend::Sqlite, stringify!($name)).await {
                        super::$name(srv).await;
                    }
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    if let Some(srv) = super::Server::start(amail_store::testutil::Backend::Postgres, stringify!($name)).await {
                        super::$name(srv).await;
                    }
                }
            )*
        }
    };
}

web_tests!(
    search_english_chinese_short_and_visibility,
    session_cookie_and_csrf,
    webhook_on_user_mail,
    attention_overview_counts,
    sent_and_mark_read,
    archive_by_recipient,
    seat_commands_reach_nodes,
    live_directory_and_node_events,
    mark_unread_and_notices,
    transcript_relay,
    embedded_app_and_headers,
);

struct Server {
    base: String,
    db: TestDb,
    http: reqwest::Client,
    state: AppState,
}

#[derive(Clone)]
struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(backend: Backend, test: &str) -> Option<Server> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut state = AppState::new(db.store.clone());
        state.mirror_config.lease = Duration::from_millis(900);
        state.mirror_config.answer_timeout = Duration::from_millis(500);
        state.webhook_config.backoff = vec![Duration::from_millis(100), Duration::from_millis(100)];
        tokio::spawn(amail_server::serve(
            listener,
            state.clone(),
            std::future::pending(),
        ));
        Some(Server {
            base,
            db,
            // No idle connections: with keep-alive connections next to
            // several open event streams, a request occasionally waited
            // forever for a pooled connection (seen about once in 25 runs
            // of transcript_relay, in the HTTP client, not the server).
            http: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
            state,
        })
    }

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
    async fn search(&self, q: &str) -> Value {
        let q: String = q
            .bytes()
            .map(|b| match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        self.ok(Method::GET, &format!("/archive/search?q={q}"), None)
            .await
    }
    async fn stream(&self, path: &str) -> reqwest::Response {
        let r = self
            .http
            .get(format!("{}/v1{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "GET {path}");
        r
    }
}

fn subjects(v: &Value) -> Vec<String> {
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["subject"].as_str().unwrap().to_string())
        .collect()
}

/// Reads server-sent events as `(event, data)`.
struct Sse {
    stream: futures::stream::BoxStream<'static, reqwest::Result<Vec<u8>>>,
    buf: String,
}

impl Sse {
    fn new(r: reqwest::Response) -> Sse {
        Sse {
            stream: r.bytes_stream().map(|c| c.map(|b| b.to_vec())).boxed(),
            buf: String::new(),
        }
    }

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

async fn search_english_chinese_short_and_visibility(srv: Server) {
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    let x = beta.seat("x").await;
    acme.send(json!({ "to": ["a"], "subject": "Parser plan", "body": "Build the tokenizer first.\nThen the AST." }))
        .await;
    acme.send(json!({ "to": ["a"], "subject": "数据库迁移方案", "body": "先迁移用户表，再迁移订单表。接口保持不变。" }))
        .await;
    a.send(json!({ "to": ["owner"], "subject": "Status", "body": "The TOKENIZER is done; 接口文档已更新。" }))
        .await;
    beta.send(
        json!({ "to": ["x"], "subject": "beta secret tokenizer", "body": "迁移方案 internal" }),
    )
    .await;

    // English, case-insensitive, subject or body, newest first.
    let r = acme.search("tokenizer").await;
    assert_eq!(r["mode"], "scan");
    assert_eq!(r["scan_limit"], 50_000);
    assert_eq!(subjects(&r), vec!["Status", "Parser plan"]);
    assert!(
        r["messages"][1]["snippet"]
            .as_str()
            .unwrap()
            .contains("tokenizer")
    );
    // Several terms must all match.
    assert_eq!(
        subjects(&acme.search("tokenizer AST").await),
        vec!["Parser plan"]
    );
    // Chinese substrings.
    let r = acme.search("迁移方案").await;
    assert_eq!(subjects(&r), vec!["数据库迁移方案"]);
    assert_eq!(
        subjects(&acme.search("订单表").await),
        vec!["数据库迁移方案"]
    );
    // Shorter queries look at fewer recent messages and say so.
    let r = acme.search("接口").await;
    assert_eq!(r["scan_limit"], 2000);
    assert_eq!(subjects(&r), vec!["Status", "数据库迁移方案"]);
    assert!(r.get("next_before_id").is_none());
    assert_eq!(subjects(&acme.search("ast").await), vec!["Parser plan"]);
    // LIKE wildcards are literal.
    assert!(subjects(&acme.search("%").await).is_empty());
    assert!(subjects(&acme.search("___").await).is_empty());
    // Other orgs' internal mail is invisible; the reverse too.
    assert_eq!(
        subjects(&beta.search("tokenizer").await),
        vec!["beta secret tokenizer"]
    );
    assert!(subjects(&acme.search("secret").await).is_empty());
    assert!(subjects(&acme.search("internal").await).is_empty());
    // Mail between linked orgs is visible to both.
    acme.ok(Method::POST, "/links", Some(json!({ "org": "beta" })))
        .await;
    beta.ok(Method::POST, "/links/acme/accept", None).await;
    x.send(json!({ "to": ["a@acme"], "subject": "cross-org note", "body": "跨组织的检索测试" }))
        .await;
    assert_eq!(
        subjects(&acme.search("检索测试").await),
        vec!["cross-org note"]
    );
    assert_eq!(
        subjects(&beta.search("检索测试").await),
        vec!["cross-org note"]
    );
    // Paging.
    for i in 0..5 {
        acme.send(
            json!({ "to": ["a"], "subject": format!("page {i}"), "body": "pagination marker" }),
        )
        .await;
    }
    let p1 = acme
        .ok(Method::GET, "/archive/search?q=pagination&limit=3", None)
        .await;
    assert_eq!(subjects(&p1), vec!["page 4", "page 3", "page 2"]);
    let next = p1["next_before_id"].as_i64().unwrap();
    let p2 = acme
        .ok(
            Method::GET,
            &format!("/archive/search?q=pagination&limit=3&before_id={next}"),
            None,
        )
        .await;
    assert_eq!(subjects(&p2), vec!["page 1", "page 0"]);
    assert!(p2.get("next_before_id").is_none());
    // Only user tokens; an empty query is an error.
    assert_eq!(
        a.get("/archive/search?q=plan").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        acme.get("/archive/search?q=%20").await.0,
        StatusCode::BAD_REQUEST
    );
}

async fn session_cookie_and_csrf(srv: Server) {
    let user = srv.org("acme").await;
    let a = user.seat("a").await;
    let node = user.node().await;
    let url = |p: &str| format!("{}/v1{p}", srv.base);
    let http = reqwest::Client::new();
    let login = |token: &str, csrf: bool| {
        let mut r = http.post(url("/session")).json(&json!({ "token": token }));
        if csrf {
            r = r.header("X-Amail-Csrf", "1");
        }
        r.send()
    };
    // The custom header is required; only user tokens may sign in.
    assert_eq!(
        login(&user.token, false).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        login(&a.token, true).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        login(&node.token, true).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        login("amail_bogus", true).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let r = login(&user.token, true).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let set = r.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(set.starts_with("amail_session=amail_"));
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age="] {
        assert!(set.contains(attr), "{set}");
    }
    assert!(!set.contains("Secure"));
    let who: Value = r.json().await.unwrap();
    assert_eq!(
        who,
        json!({ "org": "acme", "scope": "user", "seat": "owner" })
    );
    let cookie = set.split(';').next().unwrap().to_string();

    // X-Forwarded-Proto counts only from a trusted proxy (none here; see
    // tests/public.rs).
    let r = http
        .post(url("/session"))
        .header("X-Amail-Csrf", "1")
        .header("X-Forwarded-Proto", "https")
        .json(&json!({ "token": user.token }))
        .send()
        .await
        .unwrap();
    assert!(
        !r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );
    // An HTTPS origin makes it Secure.
    let r = http
        .post(url("/session"))
        .header("X-Amail-Csrf", "1")
        .header(
            "Origin",
            format!("https://{}", srv.base.trim_start_matches("http://")),
        )
        .header("Host", srv.base.trim_start_matches("http://"))
        .json(&json!({ "token": user.token }))
        .send()
        .await
        .unwrap();
    assert!(
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("; Secure")
    );

    // Reads work with the cookie alone.
    let r = http
        .get(url("/whoami"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // Writes need the header...
    let send = json!({ "to": ["a"], "subject": "via cookie", "body": "" });
    let r = http
        .post(url("/messages"))
        .header("Cookie", &cookie)
        .json(&send)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["code"], "csrf");
    let r = http
        .post(url("/messages"))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .json(&send)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // ...and a foreign Origin is refused even with it.
    let r = http
        .post(url("/messages"))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .header("Origin", "https://evil.example")
        .json(&send)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let host = srv.base.trim_start_matches("http://");
    let r = http
        .post(url("/messages"))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .header("Origin", format!("http://{host}"))
        .json(&send)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // X-Forwarded-Host counts only from a trusted proxy (none here).
    let r = http
        .post(url("/messages"))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .header("Origin", "https://amail.example")
        .header("X-Forwarded-Host", "amail.example")
        .json(&send)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // A bearer token is not subject to the cookie rules.
    user.ok(Method::POST, "/messages", Some(send.clone())).await;
    // Server-sent events work with the cookie.
    let r = http
        .get(url("/events"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    drop(r);

    // Sign out: the cookie no longer works and is cleared.
    let r = http
        .delete(url("/session"))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let r = http
        .get(url("/whoami"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Revoking the token behind a session ends the session.
    let extra = user
        .ok(Method::POST, "/tokens", Some(json!({ "scope": "user" })))
        .await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let r = login(&extra, true).await.unwrap();
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let r = http
        .get(url("/whoami"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let tokens = user.ok(Method::GET, "/tokens", None).await;
    let id = tokens
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["scope"] == "user")
        .map(|t| t["id"].as_i64().unwrap())
        .max()
        .unwrap();
    user.ok(Method::DELETE, &format!("/tokens/{id}"), None)
        .await;
    let r = http
        .get(url("/whoami"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
}

/// A webhook receiver: records bodies; fails the first `fail` requests.
async fn receiver(fail: usize) -> (String, Arc<Mutex<Vec<Value>>>) {
    use axum::routing::post;
    let got: Arc<Mutex<Vec<Value>>> = Arc::default();
    let tries = Arc::new(Mutex::new(0usize));
    let g = got.clone();
    let app = axum::Router::new().route(
        "/hook",
        post(move |axum::Json(v): axum::Json<Value>| {
            let (g, tries) = (g.clone(), tries.clone());
            async move {
                let mut t = tries.lock().unwrap();
                *t += 1;
                if *t <= fail {
                    return StatusCode::INTERNAL_SERVER_ERROR;
                }
                g.lock().unwrap().push(v);
                StatusCode::NO_CONTENT
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, got)
}

async fn wait_for(got: &Arc<Mutex<Vec<Value>>>, n: usize) -> Vec<Value> {
    for _ in 0..100 {
        if got.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    got.lock().unwrap().clone()
}

async fn webhook_on_user_mail(srv: Server) {
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    let b = acme.seat("b").await;
    let x = beta.seat("x").await;
    // Invalid URLs are rejected; the URL is shown and can be cleared.
    assert_eq!(
        acme.call(
            Method::PATCH,
            "/org",
            Some(json!({ "webhook_url": "ftp://x" }))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let (url, got) = receiver(1).await;
    let org = acme
        .ok(Method::PATCH, "/org", Some(json!({ "webhook_url": url })))
        .await;
    assert_eq!(org["webhook_url"], url);
    assert_eq!(acme.ok(Method::GET, "/org", None).await["webhook_url"], url);
    // Agents read the org's settings but not the webhook URL, which may
    // carry credentials.
    let seen = a.ok(Method::GET, "/org", None).await;
    assert_eq!(seen["name"], "acme");
    assert!(seen.get("webhook_url").is_none(), "{seen}");

    // Mail between agents: no notice. Mail to the user: one, after a retry.
    a.send(json!({ "to": ["b"], "subject": "between agents", "body": "x" }))
        .await;
    let id = a
        .send(json!({ "to": ["owner"], "cc": ["b"], "subject": "question", "body": "secret body", "ack_required": true }))
        .await;
    let v = wait_for(&got, 1).await;
    assert_eq!(v.len(), 1, "{v:?}");
    assert_eq!(v[0]["event"], "message");
    assert_eq!(v[0]["org"], "acme");
    assert_eq!(v[0]["seat"], "owner");
    assert_eq!(v[0]["message"]["id"], id);
    assert_eq!(v[0]["message"]["from"], "a");
    assert_eq!(v[0]["message"]["subject"], "question");
    assert!(!v[0].to_string().contains("secret body"));
    // The user's own mail and list mail to agents: none.
    acme.send(json!({ "to": ["a"], "subject": "from the user", "body": "" }))
        .await;
    b.send(json!({ "to": ["a"], "subject": "x", "body": "" }))
        .await;
    // From a linked org: the receiving org's webhook, addresses relative
    // to it.
    acme.ok(Method::POST, "/links", Some(json!({ "org": "beta" })))
        .await;
    beta.ok(Method::POST, "/links/acme/accept", None).await;
    x.send(json!({ "to": ["owner@acme"], "subject": "hello from beta", "body": "" }))
        .await;
    let v = wait_for(&got, 2).await;
    assert_eq!(v.len(), 2);
    assert_eq!(v[1]["message"]["from"], "x@beta");
    assert_eq!(v[1]["message"]["source"], "external");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(got.lock().unwrap().len(), 2);
    // Cleared: no more notices.
    let org = acme
        .ok(Method::PATCH, "/org", Some(json!({ "webhook_url": null })))
        .await;
    assert!(org.get("webhook_url").is_none());
    a.send(json!({ "to": ["owner"], "subject": "after clearing", "body": "" }))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(got.lock().unwrap().len(), 2);
    // A receiver that keeps failing does not affect sending.
    let (bad, bad_got) = receiver(100).await;
    acme.ok(Method::PATCH, "/org", Some(json!({ "webhook_url": bad })))
        .await;
    a.send(json!({ "to": ["owner"], "subject": "unlucky", "body": "" }))
        .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(bad_got.lock().unwrap().is_empty());
}

async fn attention_overview_counts(srv: Server) {
    let user = srv.org("acme").await;
    let a = user.seat("a").await;
    let b = user.seat("b").await;
    let node = user.node().await;
    let items = |v: &Value| -> Vec<(String, String)> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let what = i["message"]["subject"]
                    .as_str()
                    .or(i["seat"].as_str())
                    .unwrap()
                    .to_string();
                (i["kind"].as_str().unwrap().to_string(), what)
            })
            .collect()
    };
    let q1 = a
        .send(json!({ "to": ["owner"], "subject": "question", "body": "which db?" }))
        .await;
    let fyi = a
        .send(json!({ "to": ["owner"], "subject": "fyi", "body": "" }))
        .await;
    b.send(
        json!({ "to": ["owner"], "subject": "please confirm", "body": "", "ack_required": true }),
    )
    .await;
    b.send(json!({ "to": ["a"], "cc": ["owner"], "subject": "cc only", "body": "" }))
        .await;
    b.send(json!({ "to": ["owner"], "subject": "on fire", "body": "", "priority": "urgent" }))
        .await;
    user.send(json!({ "to": ["a"], "subject": "user to agent", "body": "" }))
        .await;
    node.ok(
        Method::POST,
        "/presence",
        Some(json!({ "node": "h1", "seats": [
            {"seat": "a", "state": "awaiting", "attention": "permission", "attention_hint": "answer it in the pane"},
            {"seat": "b", "state": "idle", "wake_failed": true},
        ]})),
    )
    .await;

    let v = user.ok(Method::GET, "/attention", None).await;
    assert_eq!(
        items(&v),
        vec![
            ("reply".into(), "on fire".into()),
            ("seat".into(), "a".into()),
            ("seat".into(), "b".into()),
            ("reply".into(), "question".into()),
            ("reply".into(), "fyi".into()),
            ("ack".into(), "please confirm".into()),
        ]
    );
    let seat_a = &v["items"][1];
    assert_eq!(seat_a["attention"], "permission");
    assert_eq!(seat_a["hint"], "answer it in the pane");
    assert_eq!(seat_a["state"], "awaiting");
    assert_eq!(v["items"][2]["attention"], "wake-failed");
    assert_eq!(v["items"][3]["seat"], "a");
    assert_eq!(v["items"][3]["message"]["source"], "agent");
    assert!(v["items"][3]["delivery"]["seq"].as_i64().is_some());

    // Replying takes a question out; marking read takes an FYI out; an ack
    // request stays until acknowledged.
    user.send(
        json!({ "to": ["a"], "subject": "Re: question", "body": "sqlite", "in_reply_to": q1 }),
    )
    .await;
    user.ok(Method::POST, &format!("/messages/{fyi}/read"), None)
        .await;
    let v = user.ok(Method::GET, "/attention", None).await;
    let names: Vec<String> = items(&v).into_iter().map(|x| x.1).collect();
    assert_eq!(names, vec!["on fire", "a", "b", "please confirm"]);
    let confirm = v["items"][3]["message"]["id"].as_i64().unwrap();
    user.ok(Method::GET, &format!("/messages/{confirm}"), None)
        .await;
    assert_eq!(
        items(&user.ok(Method::GET, "/attention", None).await).len(),
        4
    );
    user.ok(Method::POST, &format!("/messages/{confirm}/ack"), None)
        .await;
    assert_eq!(
        items(&user.ok(Method::GET, "/attention", None).await).len(),
        3
    );

    // Through a list in To: in the queue for an immediate member, not for
    // a digest member.
    for (list, mode) in [("now", "immediate"), ("later", "digest")] {
        user.ok(Method::POST, "/lists", Some(json!({ "name": list })))
            .await;
        user.ok(
            Method::PUT,
            &format!("/lists/{list}/members/owner"),
            Some(json!({ "mode": mode })),
        )
        .await;
        let id = a
            .send(json!({ "to": [list], "subject": format!("via {list}"), "body": "" }))
            .await;
        let v = user.ok(Method::GET, "/attention", None).await;
        let listed = items(&v).iter().any(|x| x.1 == format!("via {list}"));
        assert_eq!(listed, mode == "immediate", "{list}");
        user.ok(Method::POST, &format!("/messages/{id}/read"), None)
            .await;
    }

    // Overview: states (seats without presence are offline), recent mail,
    // the user's unread count, the queue size.
    user.seat("c").await;
    let o = user.ok(Method::GET, "/overview", None).await;
    assert_eq!(o["seats"], 3);
    assert_eq!(
        o["states"],
        json!({"busy": 0, "idle": 1, "awaiting": 1, "dormant": 0, "offline": 1})
    );
    assert_eq!(o["needs_person"], 1);
    assert_eq!(o["wake_failed"], 1);
    assert_eq!(o["mail_last_hour"], 9);
    assert_eq!(o["mail_last_day"], 9);
    assert_eq!(o["attention"], 3);
    assert_eq!(o["user_unread"], 3);

    // Counts per seat.
    let c = user.ok(Method::GET, "/counts", None).await;
    let get = |name: &str| {
        c["seats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["seat"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(get("a")["unread"], 3);
    assert_eq!(get("a")["pending"], 3);
    assert_eq!(get("owner")["unread"], 3);
    assert_eq!(get("c")["unread"], 0);
    let one = user.ok(Method::GET, "/counts?seat=b", None).await;
    assert_eq!(one["seats"].as_array().unwrap().len(), 1);
    // User tokens only.
    for p in ["/attention", "/overview", "/counts"] {
        assert_eq!(a.get(p).await.0, StatusCode::FORBIDDEN, "{p}");
        assert_eq!(node.get(p).await.0, StatusCode::FORBIDDEN, "{p}");
    }
}

async fn archive_by_recipient(srv: Server) {
    let acme = srv.org("acme").await;
    let beta = srv.org("beta").await;
    let a = acme.seat("a").await;
    acme.seat("b").await;
    let x = beta.seat("x").await;
    acme.send(json!({ "to": ["a"], "subject": "to a", "body": "" }))
        .await;
    acme.send(json!({ "to": ["b"], "cc": ["a"], "subject": "cc a", "body": "" }))
        .await;
    a.send(json!({ "to": ["b"], "subject": "from a", "body": "" }))
        .await;
    acme.ok(Method::POST, "/links", Some(json!({ "org": "beta" })))
        .await;
    beta.ok(Method::POST, "/links/acme/accept", None).await;
    a.send(json!({ "to": ["x@beta"], "subject": "to x", "body": "" }))
        .await;
    x.send(json!({ "to": ["a@acme"], "subject": "x to a", "body": "" }))
        .await;
    let get = |q: &'static str| {
        let acme = acme.clone();
        async move {
            subjects(
                &acme
                    .ok(Method::GET, &format!("/archive/messages?{q}"), None)
                    .await,
            )
        }
    };
    assert_eq!(get("to=a").await, vec!["x to a", "cc a", "to a"]);
    assert_eq!(get("to=b").await, vec!["from a", "cc a"]);
    assert_eq!(get("to=x@beta").await, vec!["to x"]);
    assert_eq!(get("to=a&from=x@beta").await, vec!["x to a"]);
    assert!(get("to=nobody").await.is_empty());
    assert_eq!(
        acme.get("/archive/messages?to=bad%20address").await.0,
        StatusCode::BAD_REQUEST
    );
}

async fn sent_and_mark_read(srv: Server) {
    let user = srv.org("acme").await;
    let a = user.seat("a").await;
    let b = user.seat("b").await;
    let m1 = a
        .send(json!({ "to": ["b"], "subject": "one", "body": "" }))
        .await;
    a.send(json!({ "to": ["owner"], "subject": "two", "body": "" }))
        .await;
    b.send(json!({ "to": ["a"], "subject": "three", "body": "" }))
        .await;
    // The user may read any seat's sent mail and inbox.
    let s = user.ok(Method::GET, "/sent?seat=a", None).await;
    assert_eq!(subjects(&s), vec!["two", "one"]);
    let s = user.ok(Method::GET, "/sent?seat=a&limit=1", None).await;
    assert_eq!(subjects(&s), vec!["two"]);
    assert!(s["next_before_id"].as_i64().is_some());
    let inbox = user.ok(Method::GET, "/inbox?seat=a", None).await;
    assert_eq!(inbox["entries"][0]["message"]["subject"], "three");
    // A seat sees its own sent mail only.
    assert_eq!(
        subjects(&a.ok(Method::GET, "/sent", None).await),
        vec!["two", "one"]
    );
    assert_eq!(a.get("/sent?seat=b").await.0, StatusCode::FORBIDDEN);
    // Mark read: only the caller's delivery.
    let d = b
        .ok(Method::POST, &format!("/messages/{m1}/read"), None)
        .await;
    assert!(d["delivery"]["read_at"].is_string());
    assert_eq!(
        b.ok(Method::GET, "/inbox?unread=1", None).await["entries"],
        json!([])
    );
    assert_eq!(
        user.call(Method::POST, &format!("/messages/{m1}/read"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

async fn seat_commands_reach_nodes(srv: Server) {
    let user = srv.org("acme").await;
    let _a = user.seat("a").await;
    let node = user.node().await;
    // No node connected: refused, and presence says so.
    let (s, e) = user.call(Method::POST, "/seats/a/sleep", None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(e["error"]["code"], "node_offline");
    assert_eq!(
        user.call(Method::POST, "/seats/owner/wake", None).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut events = Sse::new(node.stream("/events").await);
    let user_events = user.stream("/events").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    node.ok(
        Method::POST,
        "/presence",
        Some(json!({ "node": "h1", "seats": [{"seat": "a", "state": "idle", "terminal_url": "http://127.0.0.1:9/amail-acme-main"}] })),
    )
    .await;
    let seat = user.ok(Method::GET, "/seats/a", None).await;
    assert_eq!(seat["presence"]["node_connected"], true);
    assert_eq!(
        seat["presence"]["terminal_url"],
        "http://127.0.0.1:9/amail-acme-main"
    );
    let (s, v) = user.call(Method::POST, "/seats/a/sleep", None).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(v, json!({ "seat": "a", "op": "sleep", "nodes": 1 }));
    user.ok(Method::POST, "/seats/a/wake", None).await;
    let (_, c1) = events.next_of(&["command"], 5).await.unwrap();
    assert_eq!(c1, json!({ "op": "sleep", "seat": "a", "node": "h1" }));
    let (_, c2) = events.next_of(&["command"], 5).await.unwrap();
    assert_eq!(c2["op"], "wake");
    // Users never get commands.
    let mut ue = Sse::new(user_events);
    assert!(ue.next_of(&["command"], 1).await.is_none());
    // Seat tokens cannot command.
    let a = user.seat("b").await;
    assert_eq!(
        a.call(Method::POST, "/seats/b/wake", None).await.0,
        StatusCode::FORBIDDEN
    );
    // The node went away: offline again.
    drop(events);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        user.call(Method::POST, "/seats/a/wake", None).await.0,
        StatusCode::CONFLICT
    );
    let seat = user.ok(Method::GET, "/seats/a", None).await;
    assert_eq!(seat["presence"]["node_connected"], false);
}

/// User event streams follow seat and list changes (`directory`) and the
/// number of connected nodes (`nodes`); presence events say whether a node
/// is connected, like `GET /v1/seats`.
async fn live_directory_and_node_events(srv: Server) {
    let user = srv.org("acme").await;
    let _a = user.seat("a").await;
    let seat_events = user.seat("s").await.stream("/events").await;
    let mut ue = Sse::new(user.stream("/events").await);
    // The current number first.
    let (_, n) = ue.next_of(&["nodes"], 5).await.unwrap();
    assert_eq!(n, json!({ "connected": 0 }));

    user.ok(Method::POST, "/seats", Some(json!({ "name": "c" })))
        .await;
    let (_, d) = ue.next_of(&["directory"], 5).await.unwrap();
    assert_eq!(
        d,
        json!({ "kind": "seat", "name": "c", "change": "created" })
    );
    user.ok(
        Method::PATCH,
        "/seats/c",
        Some(json!({ "display_name": "C" })),
    )
    .await;
    let (_, d) = ue.next_of(&["directory"], 5).await.unwrap();
    assert_eq!(d["change"], "updated");
    user.ok(Method::DELETE, "/seats/c", None).await;
    let (_, d) = ue.next_of(&["directory"], 5).await.unwrap();
    assert_eq!(
        d,
        json!({ "kind": "seat", "name": "c", "change": "deleted" })
    );
    user.ok(Method::POST, "/lists", Some(json!({ "name": "team" })))
        .await;
    let (_, d) = ue.next_of(&["directory"], 5).await.unwrap();
    assert_eq!(
        d,
        json!({ "kind": "list", "name": "team", "change": "created" })
    );
    user.ok(Method::PUT, "/lists/team/members/a", None).await;
    let (_, d) = ue.next_of(&["directory"], 5).await.unwrap();
    assert_eq!(d["change"], "updated");

    // A node connects, reports, and goes away.
    let node = user.node().await;
    let events = node.stream("/events").await;
    let (_, n) = ue.next_of(&["nodes"], 5).await.unwrap();
    assert_eq!(n, json!({ "connected": 1 }));
    node.ok(
        Method::POST,
        "/presence",
        Some(json!({ "node": "h1", "seats": [{"seat": "a", "state": "dormant"}] })),
    )
    .await;
    let (_, p) = ue.next_of(&["presence"], 5).await.unwrap();
    assert_eq!(p["seat"], "a");
    assert_eq!(p["node_connected"], true);
    drop(events);
    let (_, n) = ue.next_of(&["nodes"], 5).await.unwrap();
    assert_eq!(n, json!({ "connected": 0 }));

    // Seat tokens get neither.
    let mut se = Sse::new(seat_events);
    user.ok(Method::POST, "/seats", Some(json!({ "name": "d" })))
        .await;
    assert!(se.next_of(&["directory", "nodes"], 1).await.is_none());
}

/// The user marks mail unread again; mail from the server is a notice in
/// the queue, and queue entries carry the start of the message.
async fn mark_unread_and_notices(srv: Server) {
    let user = srv.org("acme").await;
    let a = user.seat("a").await;
    let id = a
        .send(json!({ "to": ["owner"], "subject": "report", "body": "Done.\nAll tests pass.\n\nDetails follow." }))
        .await;
    let queue = user.ok(Method::GET, "/attention", None).await;
    let item = &queue["items"][0];
    assert_eq!(item["kind"], "reply");
    assert_eq!(item["preview"], "Done. All tests pass. Details follow.");

    user.ok(Method::POST, &format!("/messages/{id}/read"), None)
        .await;
    assert_eq!(
        user.ok(Method::GET, "/attention", None).await["items"],
        json!([])
    );
    let d = user
        .ok(Method::POST, &format!("/messages/{id}/unread"), None)
        .await;
    assert!(d["delivery"]["read_at"].is_null(), "{d}");
    assert_eq!(
        user.ok(Method::GET, "/attention", None).await["items"][0]["kind"],
        "reply"
    );
    // Only user tokens; only the user's own deliveries.
    assert_eq!(
        a.call(Method::POST, &format!("/messages/{id}/unread"), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let own = user
        .send(json!({ "to": ["a"], "subject": "x", "body": "y" }))
        .await;
    assert_eq!(
        user.call(Method::POST, &format!("/messages/{own}/unread"), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // Acknowledged mail stays read.
    let ackd = a
        .send(json!({ "to": ["owner"], "subject": "ack me", "body": "b", "ack_required": true }))
        .await;
    user.ok(Method::POST, &format!("/messages/{ackd}/ack"), None)
        .await;
    let d = user
        .ok(Method::POST, &format!("/messages/{ackd}/unread"), None)
        .await;
    assert!(d["delivery"]["read_at"].is_string(), "{d}");

    // A node's notice is system mail: a notice, not mail to answer.
    let node = user.node().await;
    node.ok(
        Method::POST,
        "/node/notices",
        Some(json!({
            "node": "box",
            "notices": [{
                "kind": "settings",
                "changes": [{"setting": "local_policy", "old": "restricted", "new": "open"}],
            }],
        })),
    )
    .await;
    let queue = user.ok(Method::GET, "/attention", None).await;
    let kinds: Vec<(&str, &str)> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap(),
                i["message"]["source"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(kinds, vec![("reply", "agent"), ("notice", "system")]);
}

fn ev(role: &str, kind: &str, text: &str) -> Value {
    json!({ "role": role, "kind": kind, "text": text })
}

async fn transcript_relay(srv: Server) {
    let user = srv.org("acme").await;
    let _a = user.seat("a").await;
    let node = user.node().await;
    // No node connected: the watcher is told at once.
    let mut w = Sse::new(user.stream("/seats/a/transcript").await);
    let (_, s) = w.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "waiting");
    let (_, s) = w.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "node_offline");
    drop(w);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(srv.state.mirror.watched(1).is_empty());

    // A node is connected: watching sends it a lease with a backfill
    // request, renewed while someone watches.
    let mut cmds = Sse::new(node.stream("/events").await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut w1 = Sse::new(user.stream("/seats/a/transcript").await);
    let (_, c) = cmds.next_of(&["command"], 5).await.unwrap();
    assert_eq!(c["op"], "transcript");
    assert_eq!(c["seat"], "a");
    assert_eq!(c["lease_ms"], 900);
    assert_eq!(c["backfill"], 200);
    let (_, c) = cmds.next_of(&["command"], 5).await.unwrap();
    assert_eq!(c["op"], "transcript");
    assert!(
        c.get("backfill").is_none(),
        "renewals do not ask for a backfill"
    );
    let r = node
        .ok(
            Method::POST,
            "/seats/a/transcript",
            Some(json!({
                "node": "h1", "reset": true,
                "status": {"state": "streaming", "harness": "claude", "path": "/x/s.jsonl"},
                "events": [ev("user", "text", "hello <b>bold</b>"), ev("assistant", "text", "hi")],
            })),
        )
        .await;
    assert_eq!(r["subscribers"], 1);
    let (_, s) = w1.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "waiting");
    assert_eq!(w1.next_of(&["reset"], 5).await.unwrap().1, json!({}));
    let (_, s) = w1.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "streaming");
    assert_eq!(s["harness"], "claude");
    let (_, e) = w1.next_of(&["events"], 5).await.unwrap();
    assert_eq!(e["events"][0]["text"], "hello <b>bold</b>");
    assert_eq!(e["events"].as_array().unwrap().len(), 2);
    // Another node's posts for the same seat are ignored.
    node.ok(
        Method::POST,
        "/seats/a/transcript",
        Some(json!({ "node": "h2", "events": [ev("user", "text", "other node")] })),
    )
    .await;
    // A second watcher starts from the buffer.
    let mut w2 = Sse::new(user.stream("/seats/a/transcript").await);
    let (_, s) = w2.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "streaming");
    let (_, e) = w2.next_of(&["events"], 5).await.unwrap();
    assert_eq!(e["events"].as_array().unwrap().len(), 2);
    // Increments reach both; long text is cut.
    let long = "x".repeat(20_000);
    node.ok(
        Method::POST,
        "/seats/a/transcript",
        Some(json!({ "node": "h1", "events": [ev("tool", "tool_result", &long)] })),
    )
    .await;
    for w in [&mut w1, &mut w2] {
        let (_, e) = w.next_of(&["events"], 5).await.unwrap();
        assert_eq!(e["events"][0]["text"].as_str().unwrap().len(), 16_000);
        assert_eq!(e["events"][0]["truncated"], true);
    }
    // When everyone leaves, the buffer goes and renewals stop.
    drop(w1);
    drop(w2);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(srv.state.mirror.watched(1).is_empty());
    let r = node
        .ok(
            Method::POST,
            "/seats/a/transcript",
            Some(json!({ "node": "h1", "events": [ev("user", "text", "late")] })),
        )
        .await;
    assert_eq!(r["subscribers"], 0);
    // At most one renewal was already on its way.
    let mut late = 0;
    while cmds.next_of(&["command"], 1).await.is_some() {
        late += 1;
        assert!(late <= 1, "renewals go on after the watchers left");
    }
    // Nobody answers: the watcher is told.
    let mut w3 = Sse::new(user.stream("/seats/a/transcript").await);
    let (_, s) = w3.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "waiting");
    let (_, s) = w3.next_of(&["status"], 5).await.unwrap();
    assert_eq!(s["state"], "unknown");
    // Only users watch; only nodes post; only agent seats.
    let a = user.seat("b").await;
    assert_eq!(a.get("/seats/a/transcript").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        node.get("/seats/a/transcript").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        user.call(
            Method::POST,
            "/seats/a/transcript",
            Some(json!({ "events": [] }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        user.get("/seats/owner/transcript").await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        user.get("/seats/zz/transcript").await.0,
        StatusCode::NOT_FOUND
    );
}

async fn embedded_app_and_headers(srv: Server) {
    let http = reqwest::Client::new();
    let r = http.get(format!("{}/", srv.base)).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let h = r.headers().clone();
    assert!(h["content-type"].to_str().unwrap().starts_with("text/html"));
    assert_eq!(h["cache-control"], "no-cache");
    let csp = h["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'self'") && csp.contains("script-src 'self'"));
    assert!(!csp.contains("unsafe-eval"));
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "DENY");
    let body = r.text().await.unwrap();
    assert!(body.contains("<html"));
    if amail_server::web::UI_BUILT {
        // Hashed assets are cached for long.
        let asset = body
            .split('"')
            .find(|s| s.starts_with("/assets/") && s.ends_with(".js"))
            .expect("an asset in index.html");
        let r = http
            .get(format!("{}{asset}", srv.base))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert!(
            r.headers()["cache-control"]
                .to_str()
                .unwrap()
                .contains("immutable")
        );
    } else {
        assert!(body.contains("npm run build"));
    }
    // API responses are not cached and carry the headers too.
    let r = http
        .get(format!("{}/v1/whoami", srv.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert!(r.headers().contains_key("content-security-policy"));
    // Unknown paths are 404 JSON, under /v1 and outside it.
    for p in ["/v1/nope", "/nope.js", "/assets/nope.js"] {
        let r = http.get(format!("{}{p}", srv.base)).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{p}");
    }
    let r = http.post(format!("{}/", srv.base)).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}
