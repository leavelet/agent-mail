//! Documents over HTTP (M9): the calls, the write rules, `stale` answers,
//! candidates, the index budget, search lines, comments in the discussion
//! thread, `[d12]` links, notification mail, the batched reminder pass,
//! live events and other orgs' visibility (none). Both backends (Postgres
//! only when `AMAIL_TEST_PG_URL` is set).

mod common;

use amail_server::config::Config;
use amail_server::{AppState, hash_token, new_token};
use amail_store::testutil::{Backend, TestDb};
use futures::StreamExt;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

macro_rules! doc_tests {
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

doc_tests!(
    create_read_and_write,
    stale_answers_carry_since_and_diff,
    write_rules_and_kinds,
    candidates_on_create,
    index_budget_and_pinning,
    search_lines,
    comments_and_links,
    revision_mail_to_the_owner,
    reminders_one_mail_per_pass,
    split_extract_merge,
    live_doc_events,
    other_orgs_see_no_docs,
    public_doc_limits,
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
        let mut cfg = cfg;
        if test == "public_doc_limits" {
            cfg.limits.docs.docs_per_org = Some(2);
            cfg.limits.docs.search_hits = Some(1);
            cfg.limits.docs.kinds_per_org = Some(5);
        }
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
        self.ok(Method::PATCH, &format!("/docs/{num}"), Some(body))
            .await
    }
    async fn put_body(&self, num: i64, body: Value) -> Value {
        self.ok(Method::PUT, &format!("/docs/{num}/body"), Some(body))
            .await
    }
    /// Create a document; its number.
    async fn doc(&self, body: Value) -> i64 {
        self.post("/docs", body).await["doc"]["num"]
            .as_i64()
            .unwrap()
    }
    /// The status and error code of a refused call.
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
    async fn body_of(&self, id: &Value) -> String {
        self.get(&format!("/messages/{id}?peek=1")).await["message"]["body"]
            .as_str()
            .unwrap()
            .to_string()
    }
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

const FINDING: &str = "# Observed\n\nx\n\n# Evidence\n\ny\n\n# Applies to\n\nz\n";
const RUNBOOK: &str =
    "# When\n\nalways\n\n# Steps\n\nrun\n\n## Rollback\n\nundo\n\n# Verify\n\ncheck\n";

async fn create_read_and_write(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let (s, d) = w1
        .call(
            Method::POST,
            "/docs",
            Some(
                json!({ "title": " Deploy runbook ", "kind": "runbook", "body": RUNBOOK,
                         "description": "how to deploy", "slug": "runbook.deploy",
                         "labels": ["ops", "ops"], "client_key": "k1", "unknown_field": 1 }),
            ),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    let doc = &d["doc"];
    assert_eq!(doc["num"], 1);
    assert_eq!(doc["title"], "Deploy runbook");
    assert_eq!(doc["status"], "draft");
    assert_eq!(doc["owner"], "w1");
    assert_eq!(doc["rev"], 1);
    assert_eq!(doc["labels"], json!(["ops"]));
    assert_eq!(doc["body"], RUNBOOK);
    assert_eq!(d["events"][0]["kind"], "created");
    let anchors: Vec<&str> = d["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["anchor"].as_str().unwrap())
        .collect();
    assert_eq!(anchors, ["when", "steps", "rollback", "verify"]);
    // The same client key answers the same document.
    let (s, again) = w1
        .call(
            Method::POST,
            "/docs",
            Some(json!({ "title": "Deploy runbook", "kind": "runbook", "body": RUNBOOK, "client_key": "k1" })),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["existing"], true);
    assert_eq!(again["doc"]["num"], 1);
    // `d1`, `1` and the slug name it; a section or an old revision alone.
    for id in ["d1", "1", "runbook.deploy"] {
        assert_eq!(org.get(&format!("/docs/{id}")).await["doc"]["num"], 1);
    }
    let r = org.get("/docs/d1?section=rollback").await;
    assert_eq!(r["read"]["text"], "## Rollback\n\nundo\n\n");
    assert_eq!(r["read"]["path"], "Steps > Rollback");
    assert!(r["doc"].get("body").is_none());
    let r = org.get("/docs/d1%23steps").await;
    assert!(r["read"]["text"].as_str().unwrap().starts_with("# Steps"));
    // Writes: a section, then the same body (unchanged), then an old rev.
    let d = w1
        .put_body(
            1,
            json!({ "section": "rollback", "body": "undo it all", "message": "clearer" }),
        )
        .await;
    assert_eq!(d["doc"]["rev"], 2);
    let d = w1
        .put_body(1, json!({ "body": d["doc"]["body"].as_str().unwrap() }))
        .await;
    assert_eq!(d["unchanged"], true);
    assert_eq!(d["doc"]["rev"], 2);
    let old = org.get("/docs/1?rev=1").await;
    assert_eq!(old["read"]["text"], RUNBOOK);
    let h = org.get("/docs/1/history").await;
    assert_eq!(h["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(h["revisions"][1]["message"], "clearer");
    assert_eq!(h["revisions"][1]["author"], "w1");
    let diff = org.get("/docs/1/diff").await;
    assert_eq!(
        (diff["from"].as_i64(), diff["to"].as_i64()),
        (Some(1), Some(2))
    );
    assert!(
        diff["diff"].as_str().unwrap().contains("+undo it all"),
        "{diff}"
    );
    // Required sections and sizes.
    let (s, c) = w1
        .refused(
            Method::POST,
            "/docs",
            json!({ "title": "f", "kind": "finding", "body": "# Observed\n" }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "sections_missing")
    );
    let big = "a".repeat(512 * 1024 + 1);
    let (s, c) = w1
        .refused(
            Method::POST,
            "/docs",
            json!({ "title": "big", "body": big }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "body_too_large")
    );
    // Above 40,000 estimated tokens the answer advises splitting.
    let large = "word ".repeat(32_001);
    let d = w1
        .post("/docs", json!({ "title": "large", "body": large }))
        .await;
    let advice = d["doc"]["advice"].as_str().unwrap();
    assert!(
        advice.starts_with("the body is ~40,002 tokens; split it with amail doc split d"),
        "{advice}"
    );
    // Unknown documents and invalid input.
    let (s, _) = org.call(Method::GET, "/docs/d99", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, c) = w1
        .refused(Method::POST, "/docs", json!({ "title": "x", "slug": "d7" }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_slug")
    );
    let (s, c) = w1
        .refused(
            Method::POST,
            "/docs",
            json!({ "title": "x", "slug": "runbook.deploy" }),
        )
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::CONFLICT, "slug_taken"));
}

async fn stale_answers_carry_since_and_diff(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    w1.doc(json!({ "title": "Notes", "body": "line one\n" }))
        .await;
    w1.put_body(
        1,
        json!({ "body": "line one\nline two\n", "message": "second line", "if_rev": 1 }),
    )
    .await;
    let (s, v) = w1
        .call(
            Method::PUT,
            "/docs/1/body",
            Some(json!({ "body": "mine\n", "if_rev": 1 })),
        )
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "stale");
    assert_eq!(v["rev"], 2);
    assert_eq!(v["doc"]["doc"]["rev"], 2);
    let since = v["since"].as_array().unwrap();
    assert_eq!(since.len(), 1, "{v}");
    assert_eq!(since[0]["kind"], "revised");
    assert_eq!(since[0]["rev"], 2);
    assert_eq!(since[0]["actor"], "w1");
    assert_eq!(since[0]["note"], "second line");
    assert!(v["diff"].as_str().unwrap().contains("+line two"), "{v}");
    // PATCH takes if_rev too.
    let (s, c) = w1
        .refused(
            Method::PATCH,
            "/docs/1",
            json!({ "title": "x", "if_rev": 1 }),
        )
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::CONFLICT, "stale"));
}

async fn write_rules_and_kinds(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    w1.doc(json!({ "title": "Mine", "body": "x" })).await;
    let (s, c) = w2
        .refused(Method::PUT, "/docs/1/body", json!({ "body": "y" }))
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "not_owner"));
    let (s, c) = w2
        .refused(Method::PATCH, "/docs/1", json!({ "editors": ["w2"] }))
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "not_owner"));
    w1.patch(1, json!({ "editors": ["w2"] })).await;
    w2.put_body(1, json!({ "body": "y" })).await;
    let (s, c) = w2
        .refused(Method::PATCH, "/docs/1", json!({ "status": "active" }))
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "not_owner"));
    let (s, _) = w1
        .refused(Method::PATCH, "/docs/1", json!({ "editors": ["nobody"] }))
        .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // A decision: activated by the user only, then frozen.
    let body = "# Context\n\nc\n\n# Decision\n\nd\n\n# Alternatives\n\na\n\n# Consequences\n\nq\n";
    w1.doc(json!({ "title": "Use X", "kind": "decision", "body": body }))
        .await;
    let (s, c) = w1
        .refused(Method::PATCH, "/docs/2", json!({ "status": "active" }))
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "not_owner"));
    org.patch(2, json!({ "status": "active" })).await;
    let (s, c) = org
        .refused(
            Method::PUT,
            "/docs/2/body",
            json!({ "body": format!("{body}more") }),
        )
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::CONFLICT, "frozen"));
    let (s, c) = w1
        .refused(Method::PATCH, "/docs/2", json!({ "status": "draft" }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "invalid_transition")
    );
    // Kinds: seats read them; only the user adds one.
    let k = w1.get("/docs/kinds").await;
    assert_eq!(k["kinds"].as_array().unwrap().len(), 5);
    assert_eq!(k["kinds"][0]["name"], "decision");
    assert_eq!(k["kinds"][0]["docs"], 1);
    let (s, _) = w1
        .refused(Method::POST, "/docs/kinds", json!({ "name": "memo" }))
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let kind = org
        .post(
            "/docs/kinds",
            json!({ "name": "memo", "required_sections": ["Summary"], "activate_by": "human",
                    "in_index": false, "verify_days": 7 }),
        )
        .await;
    assert_eq!(kind["required_sections"], json!(["Summary"]));
    assert_eq!(kind["verify_days"], 7);
    let (s, c) = w1
        .refused(
            Method::POST,
            "/docs",
            json!({ "title": "m", "kind": "memo", "body": "x" }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "sections_missing")
    );
    org.post("/docs/kinds", json!({ "name": "memo", "disabled": true }))
        .await;
    let (s, c) = w1
        .refused(
            Method::POST,
            "/docs",
            json!({ "title": "m", "kind": "memo", "body": "# Summary\n" }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "kind_disabled")
    );
    // A team that wants the user to activate everything.
    org.ok(
        Method::PUT,
        "/teams/core",
        Some(json!({ "doc_activate_by": "human" })),
    )
    .await;
    org.ok(Method::PATCH, "/seats/w2", Some(json!({ "group": "core" })))
        .await;
    let n = w2.doc(json!({ "title": "core doc" })).await;
    let (s, _) = w2
        .refused(
            Method::PATCH,
            &format!("/docs/{n}"),
            json!({ "status": "active" }),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let t = org.get("/teams/core").await;
    assert_eq!(t["doc_activate_by"], "human");
}

async fn candidates_on_create(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    w1.doc(json!({ "title": "Deploy rollback procedure", "kind": "reference" }))
        .await;
    w1.doc(json!({ "title": "Database schema", "kind": "reference" }))
        .await;
    let (s, d) = w1
        .call(
            Method::POST,
            "/docs",
            Some(json!({ "title": "Rollback of a deploy", "kind": "reference" })),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "never refused");
    assert_eq!(d["candidates"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["candidates"][0]["doc"]["num"], 1);
    assert_eq!(
        d["candidates_line"],
        "similar documents exist: d1 Deploy rollback procedure; supersede or supplement one of them if this is the same subject"
    );
    // Supplementing at creation records the relation and tells the owner.
    let w2 = org.seat("w2").await;
    let n = w2
        .doc(json!({ "title": "More on rollback", "supplements": "d1" }))
        .await;
    let d = org.get("/docs/1").await;
    assert_eq!(d["supplemented_by"][0]["num"], n);
    eventually("the owner's mail", || async {
        w1.reminders().await.iter().any(|s| {
            s == &format!("d{n} More on rollback supplements your d1: Deploy rollback procedure")
        })
    })
    .await;
}

async fn index_budget_and_pinning(srv: Server) {
    let org = srv.org("alpha").await;
    org.ok(Method::PUT, "/teams/core", Some(json!({}))).await;
    let w1 = org.seat("w1").await;
    org.ok(Method::PATCH, "/seats/w1", Some(json!({ "group": "core" })))
        .await;
    let mut nums = Vec::new();
    for i in 0..5 {
        let n = w1
            .doc(json!({ "title": format!("Runbook {i}"), "kind": "runbook", "body": "# When\n# Steps\n# Verify\n",
                         "description": format!("step list {i}") }))
            .await;
        w1.patch(n, json!({ "status": "active" })).await;
        nums.push(n);
    }
    // Drafts and kinds outside the index are left out.
    w1.doc(json!({ "title": "draft", "kind": "index" })).await;
    let r = w1.doc(json!({ "title": "ref", "kind": "reference" })).await;
    w1.patch(r, json!({ "status": "active" })).await;
    w1.patch(nums[0], json!({ "pinned": true })).await;
    let ix = w1.get("/docs/index").await;
    assert_eq!(ix["team"], "core");
    let lines = ix["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 5);
    assert_eq!(
        lines[0],
        format!("d{}  runbook  Runbook 0 — step list 0", nums[0])
    );
    assert!(ix.get("more_line").is_none());
    // The budget the node asks for.
    let ix = w1.get("/docs/index?max_lines=2").await;
    assert_eq!(ix["lines"].as_array().unwrap().len(), 2);
    assert_eq!(ix["more"], 3);
    assert_eq!(ix["more_line"], "… and 3 more: amail doc list --team core");
    assert!(
        ix["text"]
            .as_str()
            .unwrap()
            .ends_with("… and 3 more: amail doc list --team core")
    );
    let ix = w1.get("/docs/index?max_chars=120").await;
    assert!(ix["text"].as_str().unwrap().chars().count() <= 120, "{ix}");
    // A node token names the seat.
    let node = org
        .ok(Method::POST, "/tokens", Some(json!({ "scope": "node" })))
        .await;
    let node = Client {
        token: node["token"].as_str().unwrap().to_string(),
        ..org.clone()
    };
    let ix = node.get("/docs/index?seat=w1").await;
    assert_eq!(ix["lines"].as_array().unwrap().len(), 5);
    let (s, _) = node.call(Method::GET, "/docs", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Stale documents are marked.
    srv.sql(&format!(
        "UPDATE docs SET expires_at = 1 WHERE num = {}",
        nums[1]
    ))
    .await;
    let ix = w1.get("/docs/index").await;
    assert!(
        ix["lines"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .ends_with("(stale)"),
        "{ix}"
    );
}

async fn search_lines(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    w1.doc(json!({ "title": "Deploy", "kind": "runbook", "body": "# When\n\nalways\n\n# Steps\n\nrun\n\n## Rollback\n\nRun the rollback script. Then check.\n\n# Verify\n\n部署之后检查日志。\n" }))
        .await;
    w1.patch(1, json!({ "status": "active" })).await;
    w1.post("/docs/1/verify", json!({})).await;
    w1.doc(json!({ "title": "Draft rollback", "body": "rollback notes" }))
        .await;
    let r = w1.get("/docs/search?q=rollback%20script").await;
    let hits = r["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{r}");
    let line = hits[0]["line"].as_str().unwrap();
    assert!(
        line.starts_with("d1#rollback  runbook  active  Steps > Rollback — Run the rollback script.  (verified 20"),
        "{line}"
    );
    // Every status, marked.
    let r = w1.get("/docs/search?q=rollback&status=all").await;
    assert!(
        r["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["status"] == "draft")
    );
    // CJK.
    let r = w1
        .get("/docs/search?q=%E6%A3%80%E6%9F%A5%E6%97%A5%E5%BF%97")
        .await;
    assert_eq!(r["hits"][0]["anchor"], "verify", "{r}");
    // Short terms: titles and descriptions.
    let r = w1.get("/docs/search?q=De").await;
    assert_eq!(r["titles_only"], true);
    assert_eq!(r["hits"][0]["num"], 1);
    // Find: titles and slugs, every status.
    let r = w1.get("/docs/find?q=draft").await;
    assert_eq!(r["docs"][0]["num"], 2);
    let (s, _) = w1.call(Method::GET, "/docs/search", None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

async fn comments_and_links(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    w1.doc(json!({ "title": "Deploy", "body": "# Deploy\n\n## Rollback\n\nundo\n" }))
        .await;
    // A comment on a section: to the owner, with the anchor line.
    let c1 = w2
        .post(
            "/docs/d1/comment",
            json!({ "body": "Which script?", "anchor": "rollback" }),
        )
        .await;
    let thread = c1["thread_id"].as_i64().unwrap();
    let m = org.get(&format!("/messages/{}?peek=1", c1["id"])).await;
    assert_eq!(m["message"]["subject"], "[d1] Deploy");
    assert_eq!(m["message"]["to"], json!(["w1"]));
    assert_eq!(
        m["message"]["body"],
        "On d1#rollback (Deploy > Rollback):\n\nWhich script?"
    );
    // The owner answers: Cc the earlier commenter.
    let c2 = w1
        .post("/docs/1/comment", json!({ "body": "rollback.sh" }))
        .await;
    assert_eq!(c2["thread_id"], thread);
    let m = org.get(&format!("/messages/{}?peek=1", c2["id"])).await;
    assert_eq!(m["message"]["cc"], json!(["w2"]));
    // A reply in the thread is a comment too; the thread shows the document.
    org.post(
        "/messages",
        json!({ "to": ["w1"], "subject": "Re: [d1] Deploy", "body": "ok", "in_reply_to": c2["id"] }),
    )
    .await;
    let d = org.get("/docs/1").await;
    assert_eq!(d["doc"]["thread"], thread);
    assert_eq!(d["comments"].as_array().unwrap().len(), 3);
    assert_eq!(d["comments"][0]["anchor"], "rollback");
    assert!(d["comments"][0].get("removed").is_none());
    let t = w1.get(&format!("/threads/{thread}")).await;
    assert_eq!(t["docs"][0]["num"], 1);
    // A later revision removes the section: the comment says so.
    w1.put_body(1, json!({ "body": "# Deploy\n\nno rollback\n" }))
        .await;
    let d = org.get("/docs/1").await;
    assert_eq!(d["comments"][0]["removed"], true);
    assert_eq!(d["comments"][0]["path"], "Deploy > Rollback");
    let (s, c) = w2
        .refused(
            Method::POST,
            "/docs/1/comment",
            json!({ "body": "x", "anchor": "rollback" }),
        )
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "no_section")
    );
    // `[d1]` in a subject links the thread; a task title naming d1 links
    // the task.
    let m = w2
        .post(
            "/messages",
            json!({ "to": ["w1"], "subject": "[d1] question", "body": "?" }),
        )
        .await;
    let other =
        org.get(&format!("/messages/{}?peek=1", m["id"])).await["message"]["thread_id"].clone();
    org.post("/tasks", json!({ "title": "Review d1" })).await;
    let d = org.get("/docs/1").await;
    assert_eq!(d["threads"][0]["thread_id"], other);
    assert_eq!(d["tasks"][0]["title"], "Review d1");
    // Links out and in, dangling marked.
    w2.doc(json!({ "title": "Index", "body": "See d1#deploy, d1#gone and d42.\n" }))
        .await;
    let l = org.get("/docs/2/links").await;
    let dangling: Vec<&str> = l["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["dangling"] == true)
        .map(|x| x["target"].as_str().unwrap())
        .collect();
    assert_eq!(dangling, ["d1#gone", "d42"]);
    assert_eq!(org.get("/docs/1/links").await["linked_from"][0]["src"], 2);
    let ck = org.get("/docs/check").await;
    assert_eq!(ck["problems"].as_array().unwrap().len(), 2);
}

async fn revision_mail_to_the_owner(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let w2 = org.seat("w2").await;
    w1.doc(json!({ "title": "Notes", "body": "a", "editors": ["w2"] }))
        .await;
    w2.put_body(1, json!({ "body": "b", "silent": true })).await;
    w2.put_body(1, json!({ "body": "c" })).await;
    eventually("the revision mail", || async {
        w1.reminders()
            .await
            .iter()
            .any(|s| s == "d1 was revised by w2: Notes")
    })
    .await;
    assert_eq!(w1.reminders().await.len(), 1, "silent writes send nothing");
    // The owner's own writes send nothing.
    w1.put_body(1, json!({ "body": "d" })).await;
    // Supersession tells the owner of the replaced document.
    w2.doc(json!({ "title": "Better notes" })).await;
    org.post("/docs/1/supersede", json!({ "by": "d2" })).await;
    eventually("the supersession mail", || async {
        w1.reminders()
            .await
            .iter()
            .any(|s| s == "d1 Notes is superseded by d2 Better notes")
    })
    .await;
    let d = org.get("/docs/1").await;
    assert_eq!(d["doc"]["status"], "superseded");
    assert_eq!(d["superseded_by"]["num"], 2);
    assert_eq!(org.get("/docs/2").await["replaces"][0]["num"], 1);
    assert_eq!(w1.reminders().await.len(), 2);
}

async fn reminders_one_mail_per_pass(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    org.post(
        "/tasks",
        json!({ "title": "late", "assignee": "w1", "due": "2100-01-01T00:00:00Z" }),
    )
    .await;
    w1.doc(json!({ "title": "Fact", "kind": "finding", "body": FINDING }))
        .await;
    w1.post("/docs/1/verify", json!({ "for": "30d" })).await;
    w1.doc(json!({ "title": "Target", "body": "# Later\n\nx\n" }))
        .await;
    w1.doc(json!({ "title": "Linker", "body": "see d2#later\n" }))
        .await;
    eventually("the assignment mail", || async {
        w1.reminders().await.len() == 1
    })
    .await;
    // Everything becomes due at once: the next pass sends one mail.
    srv.sql(
        "BEGIN; UPDATE tasks SET due = 1; UPDATE docs SET expires_at = 1 WHERE num = 1; \
         DELETE FROM doc_sections WHERE anchor = 'later'; COMMIT;",
    )
    .await;
    // Everything is due at once, so one pass sends one mail. A pass that
    // started before the update committed may take part of it and the next
    // pass the rest (the claims are separate statements), so a second mail
    // is allowed; every item is reminded once.
    let items = [
        "t1 is past its due time",
        "d1 is due to be confirmed: Fact",
        "d3 has 1 dangling link: d2#later",
    ];
    let system_bodies = || async {
        let inbox = w1.inbox().await;
        let mut bodies = Vec::new();
        for e in inbox.iter().filter(|e| e["message"]["source"] == "system") {
            assert_eq!(e["delivery"]["class"], "to");
            bodies.push(w1.body_of(&e["message"]["id"]).await);
        }
        bodies
    };
    eventually("the pass's mail", || async {
        let all = system_bodies().await.join("\n");
        items.iter().all(|l| all.contains(l))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bodies = system_bodies().await;
    assert!(bodies.len() <= 3, "one mail per pass: {bodies:?}");
    let all = bodies.join("\n");
    for l in items {
        assert_eq!(all.matches(l).count(), 1, "{l} once in {all}");
    }
    assert!(all.contains("amail doc verify d1"), "{all}");
    // Each once: the expiry until the next confirmation, the dangling link
    // once a day.
    let h = w1.get("/docs/1/history").await;
    let sent: Vec<&Value> = h["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "reminder_sent")
        .collect();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["field"], "expired");
}

async fn split_extract_merge(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    w1.doc(json!({ "title": "Ops", "kind": "runbook", "body": RUNBOOK }))
        .await;
    w1.doc(json!({ "title": "Refs", "body": "see d1#rollback and d1#verify\n" }))
        .await;
    let (s, c) = w1
        .refused(Method::POST, "/docs/1/split", json!({ "level": 4 }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "nothing_to_split")
    );
    let d = w1.post("/docs/1/split", json!({})).await;
    let moved: Vec<(String, String)> = d["moved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["from"].as_str().unwrap().to_string(),
                m["to"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        moved,
        [
            ("#when".to_string(), "d3".to_string()),
            ("#steps".to_string(), "d4".to_string()),
            ("#verify".to_string(), "d5".to_string())
        ]
    );
    assert_eq!(d["children"].as_array().unwrap().len(), 3);
    assert_eq!(
        org.get("/docs/2").await["doc"]["body"],
        "see d4#rollback and d5\n"
    );
    // Extract a section of a part, then merge it back.
    let d = w1
        .post(
            "/docs/4/extract",
            json!({ "anchor": "rollback", "into": "Rolling back" }),
        )
        .await;
    assert_eq!(d["moved"][0]["to"], "d6");
    assert_eq!(org.get("/docs/2").await["doc"]["body"], "see d6 and d5\n");
    let d6 = org.get("/docs/6").await;
    assert_eq!(d6["supplements"]["num"], 4);
    let d = w1.post("/docs/6/merge", json!({ "into": "d4" })).await;
    assert_eq!(d["doc"]["status"], "superseded");
    assert_eq!(d["moved"][0]["to"], "d4#rolling-back");
    assert_eq!(
        org.get("/docs/2").await["doc"]["body"],
        "see d4#rolling-back and d5\n"
    );
    let w2 = org.seat("w2").await;
    w2.doc(json!({ "title": "w2's" })).await;
    let (s, c) = w1
        .refused(Method::POST, "/docs/5/merge", json!({ "into": "d7" }))
        .await;
    assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "not_owner"));
}

async fn live_doc_events(srv: Server) {
    let org = srv.org("alpha").await;
    let w1 = org.seat("w1").await;
    let resp = common::open_stream(
        common::stream_client()
            .get(format!("{}/v1/events", srv.base))
            .bearer_auth(&w1.token),
    )
    .await;
    let mut body = resp.bytes_stream();
    org.doc(json!({ "title": "live" })).await;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !buf.contains("event: doc") {
        let chunk = tokio::time::timeout_at(deadline, body.next())
            .await
            .expect("a doc event")
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(buf.contains(r#""num":1"#), "{buf}");
    assert!(buf.contains(r#""change":"created""#), "{buf}");
}

async fn other_orgs_see_no_docs(srv: Server) {
    let a = srv.org("alpha").await;
    let b = srv.org("bravo").await;
    a.doc(json!({ "title": "secret", "slug": "secret.plan" }))
        .await;
    a.doc(json!({ "title": "second secret" })).await;
    let (s, _) = b.call(Method::GET, "/docs/2", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = b.call(Method::GET, "/docs/secret.plan", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(
        b.get("/docs?status=all").await["docs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        b.get("/docs/search?q=secret&status=all").await["hits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(b.doc(json!({ "title": "bravo's own" })).await, 1);
}

async fn public_doc_limits(srv: Server) {
    let org = srv.org("alpha").await;
    org.doc(json!({ "title": "one", "body": "needle" })).await;
    org.doc(json!({ "title": "two", "body": "needle" })).await;
    let (s, c) = org
        .refused(Method::POST, "/docs", json!({ "title": "three" }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "too_many_docs")
    );
    let r = org.get("/docs/search?q=needle&status=all&limit=10").await;
    assert_eq!(r["hits"].as_array().unwrap().len(), 1);
    let (s, c) = org
        .refused(Method::POST, "/docs/kinds", json!({ "name": "memo" }))
        .await;
    assert_eq!(
        (s, c.as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "too_many_kinds")
    );
}
