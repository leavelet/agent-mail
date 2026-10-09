//! A server under a URL path prefix: `--base-path` (the server answers
//! below the prefix) and `X-Forwarded-Prefix` from a trusted proxy that
//! removes the prefix. Routes, the embedded app and its redirects, the
//! session cookie's path, the Origin check, the cookie's `Secure` flag and
//! `/v1/info`. Both backends (Postgres only when `AMAIL_TEST_PG_URL` is set).

mod common;

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use reqwest::header::HeaderMap;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

macro_rules! prefix_tests {
    ($($name:ident),* $(,)?) => {
        mod sqlite {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::common::deadline(stringify!($name), super::$name(amail_store::testutil::Backend::Sqlite, stringify!($name))).await;
                }
            )*
        }
        mod postgres {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
                async fn $name() {
                    super::common::deadline(stringify!($name), super::$name(amail_store::testutil::Backend::Postgres, stringify!($name))).await;
                }
            )*
        }
    };
}

prefix_tests!(
    base_path_routes_and_app,
    base_path_session_cookie_csrf_secure,
    forwarded_prefix_from_trusted_proxy,
    root_deep_links,
);

struct Server {
    /// `http://127.0.0.1:port` (without the prefix).
    root: String,
    #[allow(dead_code)]
    db: TestDb,
    token: String,
    http: reqwest::Client,
}

impl Server {
    async fn start(backend: Backend, test: &str, cfg: Config) -> Option<Server> {
        let db = amail_store::testutil::open(backend, test).await?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let mut state = AppState::new(db.store.clone());
        state.set_config(cfg);
        tokio::spawn(amail_server::serve(listener, state, std::future::pending()));
        let token = new_token();
        db.store
            .create_org("acme", "owner", &hash_token(&token))
            .await
            .unwrap();
        Some(Server {
            root,
            db,
            token,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        })
    }

    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut r = self.http.get(format!("{}{path}", self.root));
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        r.send().await.unwrap()
    }

    async fn login(&self, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut r = self
            .http
            .post(format!("{}{path}", self.root))
            .header("X-Amail-Csrf", "1")
            .json(&json!({ "token": self.token }));
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        r.send().await.unwrap()
    }

    fn host(&self) -> &str {
        self.root.trim_start_matches("http://")
    }
}

fn location(h: &HeaderMap) -> &str {
    h["location"].to_str().unwrap()
}

fn set_cookie(r: &reqwest::Response) -> String {
    r.headers()["set-cookie"].to_str().unwrap().to_string()
}

fn cfg(base: &str, trusted: &[&str]) -> Config {
    Config {
        base_path: base.into(),
        trusted_proxies: trusted.iter().map(|t| t.parse().unwrap()).collect(),
        ..Config::default()
    }
}

const HTML: (&str, &str) = ("Accept", "text/html,application/xhtml+xml,*/*;q=0.8");

/// Routes below `--base-path /amail`, the root's `/healthz`, redirects,
/// and the app's page carrying the prefix.
async fn base_path_routes_and_app(b: Backend, test: &str) {
    let Some(srv) = Server::start(b, test, cfg("/amail", &[])).await else {
        return;
    };
    // Health checks answer at the root and below the prefix.
    for p in ["/healthz", "/amail/healthz"] {
        let r = srv.get(p, &[]).await;
        assert_eq!(r.status(), StatusCode::OK, "{p}");
        assert_eq!(r.text().await.unwrap(), "ok");
    }
    // Nothing else outside the prefix (also not a longer name).
    for p in [
        "/",
        "/v1/info",
        "/assets/x.js",
        "/amailx/",
        "/amailx/v1/info",
    ] {
        let r = srv.get(p, &[HTML]).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{p}");
        assert!(r.headers().contains_key("content-security-policy"), "{p}");
    }
    // The API below it, with the prefix in /v1/info.
    let r = srv.get("/amail/v1/info", &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["cache-control"], "no-store");
    let info: Value = r.json().await.unwrap();
    assert_eq!(info["base_path"], "/amail");
    let r = srv
        .http
        .get(format!("{}/amail/v1/whoami", srv.root))
        .bearer_auth(&srv.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        srv.get("/amail/v1/nope", &[]).await.status(),
        StatusCode::NOT_FOUND
    );

    // `/amail` goes to `/amail/` (keeping the query), where the app is.
    let r = srv.get("/amail", &[HTML]).await;
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(location(r.headers()), "/amail/");
    let r = srv.get("/amail?lang=en", &[]).await;
    assert_eq!(location(r.headers()), "/amail/?lang=en");
    for p in ["/amail/", "/amail/register", "/amail/admin"] {
        let r = srv.get(p, &[HTML]).await;
        assert_eq!(r.status(), StatusCode::OK, "{p}");
        assert!(
            r.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        assert_eq!(r.headers()["cache-control"], "no-cache");
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("script-src 'self'"));
        let body = r.text().await.unwrap();
        assert!(
            body.contains(r#"<meta name="amail-base" content="/amail/" />"#),
            "{body}"
        );
        if amail_server::web::UI_BUILT {
            // Assets are referenced relative to the page, and served below
            // the prefix.
            assert!(!body.contains("\"/assets/"), "{body}");
            let asset = body
                .split('"')
                .find(|s| s.starts_with("./assets/") && s.ends_with(".js"))
                .expect("an asset in index.html");
            let r = srv
                .get(&format!("/amail/{}", asset.trim_start_matches("./")), &[])
                .await;
            assert_eq!(r.status(), StatusCode::OK);
            assert!(
                r.headers()["cache-control"]
                    .to_str()
                    .unwrap()
                    .contains("immutable")
            );
        }
    }
    // A browser's link to an app route as a path goes to the same route as
    // a fragment, below the prefix; other clients and file names get 404.
    let r = srv.get("/amail/org/seat-1?x=1", &[HTML]).await;
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(location(r.headers()), "/amail/#/org/seat-1?x=1");
    for (p, h) in [
        ("/amail/org/seat-1", None),
        ("/amail/org/x.js", Some(HTML)),
        ("/amail/assets/nope", Some(HTML)),
        ("/amail/v1/nope", Some(HTML)),
    ] {
        let r = srv.get(p, h.as_slice()).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{p}");
    }
    // X-Forwarded-Prefix from a peer that is not a trusted proxy changes
    // nothing.
    let r = srv
        .get("/amail/v1/info", &[("X-Forwarded-Prefix", "/evil")])
        .await;
    assert_eq!(r.json::<Value>().await.unwrap()["base_path"], "/amail");
    let r = srv
        .get("/amail/", &[HTML, ("X-Forwarded-Prefix", "/evil")])
        .await;
    assert!(r.text().await.unwrap().contains(r#"content="/amail/""#));
}

/// The session cookie is scoped to the prefix; the Origin check and the
/// `Secure` flag work as at the root.
async fn base_path_session_cookie_csrf_secure(b: Backend, test: &str) {
    let Some(srv) = Server::start(b, test, cfg("/amail", &["127.0.0.1"])).await else {
        return;
    };
    let r = srv.login("/amail/v1/session", &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    let set = set_cookie(&r);
    for attr in ["Path=/amail;", "HttpOnly", "SameSite=Strict", "Max-Age="] {
        assert!(set.contains(attr), "{set}");
    }
    assert!(!set.contains("Secure"), "{set}");
    let cookie = set.split(';').next().unwrap().to_string();
    let who = srv
        .get("/amail/v1/whoami", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(who.status(), StatusCode::OK);

    // A write with the cookie: same origin passes, another origin does not.
    let send = |origin: String, extra: Vec<(&'static str, String)>| {
        let mut r = srv
            .http
            .post(format!("{}/amail/v1/messages", srv.root))
            .header("Cookie", &cookie)
            .header("X-Amail-Csrf", "1")
            .header("Origin", origin)
            .json(&json!({ "to": ["owner"], "subject": "x", "body": "" }));
        for (k, v) in extra {
            r = r.header(k, v);
        }
        r.send()
    };
    let r = send(format!("http://{}", srv.host()), vec![])
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = send("http://evil.example".into(), vec![]).await.unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // Through the trusted proxy (the test's own address), the public host
    // counts as X-Forwarded-Host.
    let r = send(
        "https://mail.example.com".into(),
        vec![("X-Forwarded-Host", "mail.example.com".into())],
    )
    .await
    .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let r = send(
        "https://evil.example".into(),
        vec![("X-Forwarded-Host", "mail.example.com".into())],
    )
    .await
    .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    // Sign-in from another origin is refused too.
    let r = srv
        .login("/amail/v1/session", &[("Origin", "http://evil.example")])
        .await;
    assert_eq!(r.status(), StatusCode::FORBIDDEN);

    // Secure: X-Forwarded-Proto from the trusted proxy, or an HTTPS origin.
    let r = srv
        .login("/amail/v1/session", &[("X-Forwarded-Proto", "https")])
        .await;
    let set = set_cookie(&r);
    assert!(
        set.contains("Path=/amail;") && set.contains("; Secure"),
        "{set}"
    );
    let origin = format!("https://{}", srv.host());
    let r = srv
        .login("/amail/v1/session", &[("Origin", origin.as_str())])
        .await;
    assert!(set_cookie(&r).contains("; Secure"));

    // Signing out clears the cookie at the same path.
    let r = srv
        .http
        .delete(format!("{}/amail/v1/session", srv.root))
        .header("Cookie", &cookie)
        .header("X-Amail-Csrf", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let set = set_cookie(&r);
    assert!(
        set.contains("Max-Age=0") && set.contains("Path=/amail;"),
        "{set}"
    );
    let who = srv
        .get("/amail/v1/whoami", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(who.status(), StatusCode::UNAUTHORIZED);
}

/// A proxy that removes the prefix says so with X-Forwarded-Prefix: the
/// server uses it when the proxy is trusted and the value is valid.
async fn forwarded_prefix_from_trusted_proxy(b: Backend, test: &str) {
    let Some(srv) = Server::start(b, test, cfg("", &["127.0.0.1"])).await else {
        return;
    };
    let px = ("X-Forwarded-Prefix", "/tools/amail");
    let r = srv.get("/v1/info", &[px]).await;
    assert_eq!(
        r.json::<Value>().await.unwrap()["base_path"],
        "/tools/amail"
    );
    let r = srv.get("/", &[HTML, px]).await;
    assert_eq!(r.headers()["vary"], "X-Forwarded-Prefix");
    assert!(
        r.text()
            .await
            .unwrap()
            .contains(r#"<meta name="amail-base" content="/tools/amail/" />"#)
    );
    let r = srv.get("/org/root", &[HTML, px]).await;
    assert_eq!(location(r.headers()), "/tools/amail/#/org/root");
    let r = srv
        .login(
            "/v1/session",
            &[
                px,
                ("X-Forwarded-Proto", "https"),
                ("X-Forwarded-Host", "mail.example.com"),
                ("Origin", "https://mail.example.com"),
            ],
        )
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    let set = set_cookie(&r);
    assert!(
        set.contains("Path=/tools/amail;") && set.contains("; Secure"),
        "{set}"
    );
    // The first of several (proxies in a chain) counts; a trailing slash
    // is dropped; invalid values are ignored (the root).
    let r = srv
        .get("/v1/info", &[("X-Forwarded-Prefix", "/a, /b")])
        .await;
    assert_eq!(r.json::<Value>().await.unwrap()["base_path"], "/a");
    let r = srv
        .get("/v1/info", &[("X-Forwarded-Prefix", "/amail/")])
        .await;
    assert_eq!(r.json::<Value>().await.unwrap()["base_path"], "/amail");
    for bad in ["amail", "//amail", "/a/../b", "/a\"><script>", "/a?x"] {
        let r = srv.get("/", &[HTML, ("X-Forwarded-Prefix", bad)]).await;
        let body = r.text().await.unwrap();
        assert!(body.contains(r#"content="/" />"#), "{bad}: {body}");
        assert!(!body.contains("<script>\""), "{bad}");
    }
    // Without the header: the root.
    let r = srv.get("/v1/info", &[]).await;
    assert_eq!(r.json::<Value>().await.unwrap()["base_path"], "/");

    // With --base-path too: the proxy's prefix comes first.
    let Some(both) = Server::start(b, &format!("{test}_both"), cfg("/amail", &["127.0.0.1"])).await
    else {
        return;
    };
    let r = both
        .get("/amail/v1/info", &[("X-Forwarded-Prefix", "/px")])
        .await;
    assert_eq!(r.json::<Value>().await.unwrap()["base_path"], "/px/amail");
    let r = both.get("/amail", &[("X-Forwarded-Prefix", "/px")]).await;
    assert_eq!(location(r.headers()), "/px/amail/");
    let r = both
        .login("/amail/v1/session", &[("X-Forwarded-Prefix", "/px")])
        .await;
    assert!(set_cookie(&r).contains("Path=/px/amail;"));

    // Not from a trusted proxy: ignored.
    let Some(untrusted) = Server::start(b, &format!("{test}_untrusted"), cfg("", &[])).await else {
        return;
    };
    let r = untrusted.get("/v1/info", &[px]).await;
    assert_eq!(r.json::<Value>().await.unwrap()["base_path"], "/");
    let r = untrusted.login("/v1/session", &[px]).await;
    assert!(set_cookie(&r).contains("Path=/;"));
}

/// At the root: the page says `/`, and app routes given as paths go to
/// fragments.
async fn root_deep_links(b: Backend, test: &str) {
    let Some(srv) = Server::start(b, test, Config::default()).await else {
        return;
    };
    let r = srv.get("/", &[HTML]).await;
    assert!(
        r.text()
            .await
            .unwrap()
            .contains(r#"<meta name="amail-base" content="/" />"#)
    );
    let r = srv.get("/org/root", &[HTML]).await;
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(location(r.headers()), "/#/org/root");
    let r = srv.get("/thread/7", &[HTML]).await;
    assert_eq!(location(r.headers()), "/#/thread/7");
    for p in ["/nope.js", "/assets/nope.js", "/v1/nope"] {
        assert_eq!(
            srv.get(p, &[HTML]).await.status(),
            StatusCode::NOT_FOUND,
            "{p}"
        );
    }
    let r = srv
        .http
        .request(Method::POST, format!("{}/org/root", srv.root))
        .header("Accept", "text/html")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let r = srv.login("/v1/session", &[]).await;
    assert!(set_cookie(&r).contains("Path=/;"));
}
