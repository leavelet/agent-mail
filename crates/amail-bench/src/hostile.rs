//! `--mode tenants --phases hostile`: can several orgs together make
//! another org's web interface refuse or slow down?
//!
//! Setup (directly in the database, like `amail admin create-org`):
//!
//! - `--hostile-orgs` orgs `<prefix>h<i>`, each with `--hostile-messages`
//!   messages of `--hostile-body-bytes` bytes of body text that does not
//!   compress (by default about 1 GiB each, the public storage quota), from
//!   the org's user seat to its agent seat `a`. An org that already exists
//!   from an earlier run keeps its mail; only new tokens are made.
//! - org `<prefix>hv` (the victim) with agent seats `v0`..`v3`, a list
//!   `team` of them, and a few threads sent through the API.
//!
//! The victim's user token then calls every endpoint the web interface
//! reads, each `--victim-rate` times per second (open loop, latency from
//! the scheduled start): first with no other load ("idle"), then while
//! `--hostile-conc` loops per hostile org run searches of
//! `--hostile-terms` terms that match nothing, following the cursor
//! through all of the org's mail and starting again. The table shows, per
//! endpoint, the victim's latency and its failed requests (429 counted
//! separately).

use crate::{Opts, pct};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

struct Victim {
    token: String,
    thread: i64,
    message: i64,
}

pub async fn run(o: &Opts, http: &reqwest::Client) -> Result<()> {
    let base = o.url.trim_end_matches('/').to_string();
    let t = Instant::now();
    let hostile = setup_hostile(o).await?;
    let vic = setup_victim(o, http, &base).await?;
    eprintln!("setup in {:.1}s", t.elapsed().as_secs_f64());
    let ops = victim_ops(&vic);

    let idle = measure(o, http, &base, &vic.token, &ops).await;

    let stop = Arc::new(AtomicBool::new(false));
    let codes: Arc<Mutex<BTreeMap<String, u64>>> = Default::default();
    let held: Arc<Mutex<Vec<u64>>> = Default::default();
    let q: Vec<String> = (0..o.hostile_terms.max(1))
        .map(|i| format!("zq{i:02}xv"))
        .collect();
    let q = q.join("+");
    let mut workers = Vec::new();
    for token in &hostile {
        for _ in 0..o.hostile_conc.max(1) {
            let (http, base, stop, codes, held, token, q) = (
                http.clone(),
                base.clone(),
                stop.clone(),
                codes.clone(),
                held.clone(),
                token.clone(),
                q.clone(),
            );
            workers.push(tokio::spawn(async move {
                let mut cursor: Option<i64> = None;
                while !stop.load(Ordering::Relaxed) {
                    let mut url = format!("{base}/v1/archive/search?q={q}&limit=50");
                    if let Some(c) = cursor {
                        url.push_str(&format!("&before_id={c}"));
                    }
                    let t = Instant::now();
                    let r = http
                        .get(url)
                        .bearer_auth(&token)
                        .timeout(Duration::from_secs(120))
                        .send()
                        .await;
                    let code = match r {
                        Ok(r) => {
                            let c = r.status();
                            let v: Value = r.json().await.unwrap_or(Value::Null);
                            if c.is_success() {
                                cursor = v["next_before_id"].as_i64();
                                held.lock().await.push(t.elapsed().as_micros() as u64);
                            }
                            c.as_u16().to_string()
                        }
                        Err(e) if e.is_timeout() => "timeout".into(),
                        Err(_) => "error".into(),
                    };
                    *codes.lock().await.entry(code).or_default() += 1;
                }
            }));
        }
    }
    tokio::time::sleep(Duration::from_secs(o.warmup)).await;
    codes.lock().await.clear();
    held.lock().await.clear();
    let t0 = Instant::now();
    let busy = measure(o, http, &base, &vic.token, &ops).await;
    let secs = t0.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        w.abort();
    }
    let codes = codes.lock().await.clone();
    let mut held = held.lock().await.clone();
    let n: u64 = codes.values().sum();
    println!(
        "hostile: {} orgs x {} loops, {:.1} searches/s {}, successful search p50 {:.0}ms p99 {:.0}ms max {:.0}ms",
        hostile.len(),
        o.hostile_conc.max(1),
        n as f64 / secs,
        serde_json::to_string(&codes)?,
        pct(&mut held, 0.5).unwrap_or(0.0),
        pct(&mut held, 0.99).unwrap_or(0.0),
        pct(&mut held, 1.0).unwrap_or(0.0),
    );
    println!(
        "{:<26} {:>9} {:>9} {:>9} {:>9} {:>7} {:>6} {:>6}",
        "victim op", "idle_p50", "idle_p99", "atk_p50", "atk_p99", "x_p50", "fail", "429"
    );
    for (name, _) in &ops {
        let i = &idle[name];
        let b = &busy[name];
        println!(
            "{:<26} {:>7.1}ms {:>7.1}ms {:>7.1}ms {:>7.1}ms {:>6.1}x {:>6} {:>6}",
            name,
            i.p50,
            i.p99,
            b.p50,
            b.p99,
            b.p50 / i.p50.max(0.001),
            b.failed,
            b.refused
        );
        if let Some(path) = &o.out {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(
                f,
                "{}",
                json!({"phase": "hostile", "op": name, "idle_p50": i.p50, "idle_p99": i.p99,
                       "attack_p50": b.p50, "attack_p99": b.p99, "failed": b.failed,
                       "refused_429": b.refused, "attacker": codes})
            )?;
        }
    }
    Ok(())
}

/// The victim's requests: what the web interface reads.
fn victim_ops(v: &Victim) -> Vec<(String, String)> {
    let (t, m) = (v.thread, v.message);
    [
        ("overview", "/v1/overview".to_string()),
        ("attention", "/v1/attention".to_string()),
        ("counts", "/v1/counts".to_string()),
        ("inbox", "/v1/inbox?limit=50".to_string()),
        ("inbox v0", "/v1/inbox?seat=v0&limit=50".to_string()),
        ("sent", "/v1/sent?limit=50".to_string()),
        ("thread", format!("/v1/threads/{t}")),
        ("message", format!("/v1/messages/{m}")),
        ("status", format!("/v1/messages/{m}/status")),
        ("archive", "/v1/archive/messages?limit=50".to_string()),
        ("search", "/v1/archive/search?q=probe&limit=50".to_string()),
        ("lists", "/v1/lists".to_string()),
        ("list team", "/v1/lists/team".to_string()),
        ("seats", "/v1/seats".to_string()),
        ("seat v0", "/v1/seats/v0".to_string()),
        ("org", "/v1/org".to_string()),
        ("tokens", "/v1/tokens".to_string()),
        ("links", "/v1/links".to_string()),
        ("whoami", "/v1/whoami".to_string()),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b))
    .collect()
}

#[derive(Default)]
struct Stats {
    p50: f64,
    p99: f64,
    /// Requests that did not succeed.
    failed: u64,
    /// Of those, 429.
    refused: u64,
}

async fn measure(
    o: &Opts,
    http: &reqwest::Client,
    base: &str,
    token: &str,
    ops: &[(String, String)],
) -> BTreeMap<String, Stats> {
    let start = Instant::now() + Duration::from_millis(100);
    let end = start + Duration::from_secs(o.duration);
    let rate = o.victim_rate.max(0.1);
    let mut tasks = Vec::new();
    for (k, (name, path)) in ops.iter().enumerate() {
        let (http, url, token, name) = (
            http.clone(),
            format!("{base}{path}"),
            token.to_string(),
            name.clone(),
        );
        // Spread the operations over the first interval.
        let offset = Duration::from_secs_f64(k as f64 / ops.len() as f64 / rate);
        tasks.push(tokio::spawn(async move {
            let lat: Arc<Mutex<(Vec<u64>, u64, u64)>> = Default::default();
            let mut inflight = Vec::new();
            let mut i: u64 = 0;
            loop {
                let at = start + offset + Duration::from_secs_f64(i as f64 / rate);
                if at >= end {
                    break;
                }
                i += 1;
                tokio::time::sleep_until(at).await;
                let (http, url, token, lat) =
                    (http.clone(), url.clone(), token.clone(), lat.clone());
                inflight.push(tokio::spawn(async move {
                    let r = http
                        .get(&url)
                        .bearer_auth(&token)
                        .timeout(Duration::from_secs(60))
                        .send()
                        .await;
                    let (ok, refused) = match r {
                        Ok(r) => {
                            let s = r.status();
                            let _ = r.bytes().await;
                            (s.is_success(), s.as_u16() == 429)
                        }
                        Err(_) => (false, false),
                    };
                    let mut l = lat.lock().await;
                    l.0.push(at.elapsed().as_micros() as u64);
                    if !ok {
                        l.1 += 1;
                    }
                    if refused {
                        l.2 += 1;
                    }
                }));
            }
            for t in inflight {
                let _ = t.await;
            }
            let mut l = lat.lock().await;
            let s = Stats {
                p50: pct(&mut l.0, 0.5).unwrap_or(0.0),
                p99: pct(&mut l.0, 0.99).unwrap_or(0.0),
                failed: l.1,
                refused: l.2,
            };
            (name, s)
        }));
    }
    let mut out = BTreeMap::new();
    for t in tasks {
        if let Ok((name, s)) = t.await {
            out.insert(name, s);
        }
    }
    out
}

/// The hostile orgs' user tokens; their mail is inserted when the org is
/// new.
async fn setup_hostile(o: &Opts) -> Result<Vec<String>> {
    use amail_proto::{NewSeat, TokenScope};
    use amail_store::NewToken;
    let store = amail_store::open(&o.db, &amail_store::OpenOptions { max_connections: 4 }).await?;
    let mut tokens = Vec::new();
    let mut fill = Vec::new();
    for i in 0..o.hostile_orgs {
        let name = format!("{}h{i}", o.prefix);
        let (org, user) = match store.org_by_name(&name).await? {
            Some(org) => {
                let user = store.user_seat(org.id).await?.context("user seat")?;
                (org, user)
            }
            None => {
                let hash = amail_server::hash_token(&amail_server::new_token());
                let (org, user) = store.create_org(&name, "owner", &hash).await?;
                let a = store
                    .create_seat(
                        org.id,
                        &NewSeat {
                            name: "a".into(),
                            ..Default::default()
                        },
                    )
                    .await?;
                fill.push((org.id, user.id, a.id, name.clone()));
                (org, user)
            }
        };
        let t = amail_server::new_token();
        store
            .create_token(&NewToken {
                org_id: org.id,
                scope: TokenScope::User,
                seat_id: Some(user.id),
                hash: amail_server::hash_token(&t),
                label: Some("bench".into()),
            })
            .await?;
        tokens.push(t);
    }
    drop(store);
    let t = Instant::now();
    let n = fill.len();
    let jobs = fill
        .iter()
        .map(|(org, user, a, name)| fill_org(o, *org, *user, *a, name));
    for r in futures::future::join_all(jobs).await {
        r?;
    }
    if n > 0 {
        eprintln!(
            "hostile mail: {n} orgs x {} messages of {} bytes in {:.1}s",
            o.hostile_messages,
            o.hostile_body_bytes,
            t.elapsed().as_secs_f64()
        );
    }
    Ok(tokens)
}

/// Bulk mail of one hostile org, in SQL: bodies of hex digits that do not
/// compress, one delivery each to seat `a`, and the org's storage count.
async fn fill_org(o: &Opts, org: i64, user: i64, a: i64, name: &str) -> Result<()> {
    let n = o.hostile_messages;
    let pg = !o.db.starts_with("sqlite:");
    let body = if pg {
        // 32 hex digits per md5, different for every message.
        format!(
            "(SELECT string_agg(md5(x::text || '-' || g::text), '') FROM generate_series(1, {}) g)",
            o.hostile_body_bytes.div_ceil(32)
        )
    } else {
        format!(
            "substr(hex(randomblob({})), 1, {})",
            o.hostile_body_bytes.div_ceil(2),
            o.hostile_body_bytes
        )
    };
    let now = amail_store::now_ms();
    let sql = [
        format!(
            "WITH RECURSIVE g(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM g WHERE x < {n}) \
             INSERT INTO messages (org_id, sender_seat_id, thread_id, parent_id, depth, subject, body, \
             lines, to_addrs, cc_addrs, priority, ack_required, created_at, meta) \
             SELECT {org}, {user}, 0, NULL, 0, 'bulk ' || x, {body}, 1, 'a@{name}', '', 'normal', false, \
             {now} - 86400000 + x, '' FROM g"
        ),
        format!("UPDATE messages SET thread_id = id WHERE org_id = {org} AND thread_id = 0"),
        format!(
            "INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq, read_at, notified_at) \
             SELECT {org}, m.id, {a}, 'to', 'immediate', \
             ROW_NUMBER() OVER (ORDER BY m.id), ROW_NUMBER() OVER (ORDER BY m.id), {now}, {now} \
             FROM messages m WHERE m.org_id = {org}"
        ),
        format!(
            "UPDATE seats SET last_seq = (SELECT COUNT(*) FROM deliveries d WHERE d.seat_id = seats.id) \
             WHERE id = {a}"
        ),
        format!(
            "UPDATE orgs SET stored_bytes = (SELECT SUM(512 + 384 + {len}(m.subject) + {len}(m.body) \
             + {len}(m.to_addrs)) FROM messages m WHERE m.org_id = {org}) WHERE id = {org}",
            len = if pg { "octet_length" } else { "length" }
        ),
    ];
    if pg {
        use sqlx::Connection;
        let mut c = sqlx::PgConnection::connect(&o.db).await?;
        for q in &sql {
            sqlx::raw_sql(q)
                .execute(&mut c)
                .await
                .with_context(|| q.clone())?;
        }
        sqlx::raw_sql("ANALYZE messages").execute(&mut c).await?;
    } else {
        use sqlx::ConnectOptions;
        let mut c =
            o.db.parse::<sqlx::sqlite::SqliteConnectOptions>()?
                .busy_timeout(Duration::from_secs(600))
                .connect()
                .await?;
        for q in &sql {
            sqlx::raw_sql(q)
                .execute(&mut c)
                .await
                .with_context(|| q.clone())?;
        }
    }
    Ok(())
}

/// The victim org, its seats, list and a few threads (sent through the
/// API, so that its counters are right).
async fn setup_victim(o: &Opts, http: &reqwest::Client, base: &str) -> Result<Victim> {
    use amail_proto::TokenScope;
    use amail_store::NewToken;
    let store = amail_store::open(&o.db, &amail_store::OpenOptions { max_connections: 2 }).await?;
    let name = format!("{}hv{}", o.prefix, amail_store::now_ms() % 1_000_000);
    let hash = amail_server::hash_token(&amail_server::new_token());
    let (org, user) = store.create_org(&name, "owner", &hash).await?;
    let token = amail_server::new_token();
    store
        .create_token(&NewToken {
            org_id: org.id,
            scope: TokenScope::User,
            seat_id: Some(user.id),
            hash: amail_server::hash_token(&token),
            label: Some("bench".into()),
        })
        .await?;
    drop(store);
    let call = |method: reqwest::Method, path: &str, body: Option<Value>| {
        let mut r = http
            .request(method, format!("{base}/v1{path}"))
            .bearer_auth(&token);
        if let Some(b) = body {
            r = r.json(&b);
        }
        async move {
            let r = r.send().await?;
            let s = r.status();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            anyhow::ensure!(s.is_success(), "{s}: {v}");
            anyhow::Ok(v)
        }
    };
    use reqwest::Method;
    for i in 0..4 {
        call(
            Method::POST,
            "/seats",
            Some(json!({"name": format!("v{i}")})),
        )
        .await?;
    }
    call(Method::POST, "/lists", Some(json!({"name": "team"}))).await?;
    for i in 0..4 {
        call(Method::PUT, &format!("/lists/team/members/v{i}"), None).await?;
    }
    let first = call(
        Method::POST,
        "/messages",
        Some(
            json!({"to": ["v0"], "cc": ["team"], "subject": "probe thread", "body": "probe body"}),
        ),
    )
    .await?;
    let (thread, message) = (
        first["thread_id"].as_i64().context("thread_id")?,
        first["id"].as_i64().context("id")?,
    );
    for i in 0..30 {
        call(
            Method::POST,
            "/messages",
            Some(
                json!({"to": [format!("v{}", i % 4)], "subject": format!("probe {i}"),
                        "body": format!("probe text {i}"), "in_reply_to": message}),
            ),
        )
        .await?;
    }
    Ok(Victim {
        token,
        thread,
        message,
    })
}
