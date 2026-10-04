//! `--mode tenants`: does one org's load slow down another org?
//!
//! Setup (directly in the database, like `amail admin create-org`):
//!
//! - org `<prefix>atk` with 199 agent seats, a list `all` of them and
//!   `--attack-tokens` user tokens. Its mail is bulk-inserted: messages
//!   from one agent seat to every other seat and the user seat, about
//!   `--rows` delivery rows in all, never claimed (pending), half of the
//!   user seat's read; the first `--thread-size` of them form one thread.
//! - org `<prefix>vic` with 4 agent seats and a thread of `--thread-size`
//!   messages from its user seat to `v0`.
//!
//! Each phase first measures the victim with no other load ("idle"), then
//! runs `--attack-conc` loops of the attacker over the phase's endpoints
//! (round robin over its tokens) and measures the victim again after
//! `--warmup` seconds. Victim operations, each at `--victim-rate` per second
//! (open loop, latency from the scheduled start): `GET /v1/inbox` of `v0`,
//! `POST /v1/messages` from `v1` to `v2` and `v3`, and `POST /v1/claim` of
//! `v2`.
//!
//! Phases (`--phases`, comma-separated): `pending`, `counts`, `overview`,
//! `attention`, `thread` (the victim's thread, which the attacker cannot
//! see), `own-thread`, `search`, `archive`, `lists`, `status`, `changes`,
//! `inbox`, `claim` (claims of `to` mail by a seat with only list mail
//! pending), `all` (every endpoint above at once).

use crate::{Opts, pct};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

struct Setup {
    atk_tokens: Vec<String>,
    /// Seat tokens of v0..v3.
    vic: Vec<String>,
    vic_thread: i64,
    atk_thread: i64,
    /// A bulk message of the attacker (with one delivery per seat).
    atk_message: i64,
}

pub async fn run(o: &Opts, http: &reqwest::Client) -> Result<()> {
    let base = o.url.trim_end_matches('/').to_string();
    let t = Instant::now();
    let s = setup(o).await?;
    eprintln!("setup in {:.1}s", t.elapsed().as_secs_f64());
    let phases: Vec<String> = o.phases.split(',').map(|p| p.trim().to_string()).collect();
    println!(
        "{:<11} {:<6} {:>9} {:>9} {:>9} {:>9} {:>7} {:>9}  attacker",
        "phase", "op", "idle_p50", "idle_p99", "atk_p50", "atk_p99", "x_p50", "errors"
    );
    for phase in &phases {
        let paths = attack_paths(phase, &s)?;
        let idle = victim(o, http, &base, &s).await;
        let stop = Arc::new(AtomicBool::new(false));
        let codes: Arc<Mutex<BTreeMap<String, u64>>> = Default::default();
        let workers: Vec<_> = (0..o.attack_conc)
            .map(|i| {
                let (http, base, stop, codes) =
                    (http.clone(), base.clone(), stop.clone(), codes.clone());
                let paths = paths.clone();
                let tokens = s.atk_tokens.clone();
                tokio::spawn(async move {
                    let mut n = i;
                    while !stop.load(Ordering::Relaxed) {
                        let path = &paths[n % paths.len()];
                        let token = &tokens[n % tokens.len()];
                        n += 1;
                        let req = match path.strip_prefix("POST ") {
                            Some(p) => http
                                .post(format!("{base}{p}"))
                                .json(&json!({"via": "bench", "classes": ["to"], "limit": 1})),
                            None => http.get(format!("{base}{path}")),
                        };
                        let code = match req
                            .bearer_auth(token)
                            .timeout(Duration::from_secs(60))
                            .send()
                            .await
                        {
                            Ok(r) => {
                                let c = r.status().as_u16().to_string();
                                let _ = r.bytes().await;
                                c
                            }
                            Err(e) if e.is_timeout() => "timeout".into(),
                            Err(_) => "error".into(),
                        };
                        *codes.lock().await.entry(code).or_default() += 1;
                    }
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_secs(o.warmup)).await;
        let t0 = Instant::now();
        codes.lock().await.clear();
        let busy = victim(o, http, &base, &s).await;
        let attack_s = t0.elapsed().as_secs_f64();
        let codes = codes.lock().await.clone();
        stop.store(true, Ordering::Relaxed);
        for w in workers {
            // Requests still running are dropped; the server finishes them.
            w.abort();
        }
        let atk: u64 = codes.values().sum();
        let atk = format!(
            "{:.0} req/s {}",
            atk as f64 / attack_s,
            serde_json::to_string(&codes)?
        );
        for op in ["inbox", "send", "claim"] {
            let (i50, i99) = (idle[op].0, idle[op].1);
            let (b50, b99) = (busy[op].0, busy[op].1);
            println!(
                "{:<11} {:<6} {:>8.1}ms {:>8.1}ms {:>8.1}ms {:>8.1}ms {:>6.1}x {:>9}  {}",
                phase,
                op,
                i50,
                i99,
                b50,
                b99,
                b50 / i50.max(0.001),
                busy[op].2,
                if op == "inbox" { atk.as_str() } else { "" }
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
                    json!({"phase": phase, "op": op, "idle_p50": i50, "idle_p99": i99,
                           "attack_p50": b50, "attack_p99": b99, "errors": busy[op].2,
                           "attacker": codes})
                )?;
            }
        }
        settle(http, &base, &s.vic[0]).await;
    }
    Ok(())
}

/// Wait until the server has finished what the attacker left running: ten
/// victim inbox reads in a row under 50 ms (at most three minutes).
async fn settle(http: &reqwest::Client, base: &str, token: &str) {
    let until = Instant::now() + Duration::from_secs(180);
    let mut fast = 0;
    while fast < 10 && Instant::now() < until {
        let t = Instant::now();
        let ok = http
            .get(format!("{base}/v1/inbox?limit=50"))
            .bearer_auth(token)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .is_ok();
        if ok && t.elapsed() < Duration::from_millis(50) {
            fast += 1;
        } else {
            fast = 0;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn attack_paths(phase: &str, s: &Setup) -> Result<Vec<String>> {
    let one = |p: &str| -> Result<Vec<String>> {
        Ok(vec![match p {
            "pending" => "/v1/pending".to_string(),
            "counts" => "/v1/counts".to_string(),
            "overview" => "/v1/overview".to_string(),
            "attention" => "/v1/attention".to_string(),
            "thread" => format!("/v1/threads/{}", s.vic_thread),
            "own-thread" => format!("/v1/threads/{}", s.atk_thread),
            "search" => "/v1/archive/search?q=zzzqqq".to_string(),
            "archive" => "/v1/archive/messages?limit=500".to_string(),
            "lists" => "/v1/lists".to_string(),
            "status" => format!("/v1/messages/{}/status", s.atk_message),
            "changes" => "/v1/changes?seat=a001&limit=5000".to_string(),
            "inbox" => "/v1/inbox?seat=a001&limit=1000".to_string(),
            // A claim of `to` mail only, of a seat with only list mail pending.
            "claim" => "POST /v1/claim?seat=a001".to_string(),
            other => bail!("unknown phase {other:?}"),
        }])
    };
    if phase == "all" {
        let mut v = Vec::new();
        for p in [
            "pending",
            "counts",
            "overview",
            "attention",
            "thread",
            "own-thread",
            "search",
            "archive",
            "lists",
            "status",
            "changes",
            "inbox",
            "claim",
        ] {
            v.extend(one(p)?);
        }
        return Ok(v);
    }
    one(phase)
}

/// Victim latencies per operation: (p50 ms, p99 ms, errors), over
/// `--duration` seconds.
async fn victim(
    o: &Opts,
    http: &reqwest::Client,
    base: &str,
    s: &Setup,
) -> BTreeMap<&'static str, (f64, f64, u64)> {
    let start = Instant::now() + Duration::from_millis(100);
    let end = start + Duration::from_secs(o.duration);
    let rate = o.victim_rate.max(0.1);
    let mut tasks = Vec::new();
    for op in ["inbox", "send", "claim"] {
        let (http, base) = (http.clone(), base.to_string());
        let token = match op {
            "inbox" => s.vic[0].clone(),
            "send" => s.vic[1].clone(),
            _ => s.vic[2].clone(),
        };
        tasks.push(tokio::spawn(async move {
            let lat: Arc<Mutex<(Vec<u64>, u64)>> = Default::default();
            let mut inflight = Vec::new();
            let mut i: u64 = 0;
            loop {
                let at = start + Duration::from_secs_f64(i as f64 / rate);
                if at >= end {
                    break;
                }
                i += 1;
                tokio::time::sleep_until(at).await;
                let (http, base, token, lat) =
                    (http.clone(), base.clone(), token.clone(), lat.clone());
                inflight.push(tokio::spawn(async move {
                    let req = match op {
                        "inbox" => http.get(format!("{base}/v1/inbox?limit=50")),
                        "send" => http.post(format!("{base}/v1/messages")).json(
                            &json!({"to": ["v2"], "cc": ["v3"], "subject": "probe", "body": "probe body"}),
                        ),
                        _ => http
                            .post(format!("{base}/v1/claim"))
                            .json(&json!({"via": "bench", "classes": ["to", "cc", "list"], "limit": 50})),
                    };
                    let r = req
                        .bearer_auth(&token)
                        .timeout(Duration::from_secs(60))
                        .send()
                        .await;
                    let ok = match r {
                        Ok(r) => {
                            let ok = r.status().is_success();
                            let _ = r.bytes().await;
                            ok
                        }
                        Err(_) => false,
                    };
                    let mut l = lat.lock().await;
                    l.0.push(at.elapsed().as_micros() as u64);
                    if !ok {
                        l.1 += 1;
                    }
                }));
            }
            for t in inflight {
                let _ = t.await;
            }
            let mut l = lat.lock().await;
            let errors = l.1;
            (
                op,
                (
                    pct(&mut l.0, 0.5).unwrap_or(0.0),
                    pct(&mut l.0, 0.99).unwrap_or(0.0),
                    errors,
                ),
            )
        }));
    }
    let mut out = BTreeMap::new();
    for t in tasks {
        if let Ok((op, v)) = t.await {
            out.insert(op, v);
        }
    }
    out
}

async fn setup(o: &Opts) -> Result<Setup> {
    use amail_proto::{NewList, NewSeat, TokenScope};
    use amail_store::{Member, NewToken};
    let store = amail_store::open(&o.db, &amail_store::OpenOptions { max_connections: 4 }).await?;
    let token = |org_id: i64, scope: TokenScope, seat_id: i64| {
        let t = amail_server::new_token();
        let rec = NewToken {
            org_id,
            scope,
            seat_id: Some(seat_id),
            hash: amail_server::hash_token(&t),
            label: Some("bench".into()),
        };
        (t, rec)
    };
    let atk_name = format!("{}atk", o.prefix);
    let vic_name = format!("{}vic", o.prefix);
    let hash = || amail_server::hash_token(&amail_server::new_token());
    let (atk, atk_user) = store
        .create_org(&atk_name, "owner", &hash())
        .await
        .context("creating the orgs (use a fresh database or another --prefix)")?;
    let (vic, vic_user) = store.create_org(&vic_name, "owner", &hash()).await?;
    // The victim sends at --victim-rate: lift its per-seat send budget.
    store
        .update_org(
            vic.id,
            &amail_store::OrgSettings {
                send_rate_per_min: Some(600),
                send_burst: Some(600),
                ..Default::default()
            },
        )
        .await?;
    let mut atk_tokens = Vec::new();
    for _ in 0..o.attack_tokens {
        let (t, rec) = token(atk.id, TokenScope::User, atk_user.id);
        store.create_token(&rec).await?;
        atk_tokens.push(t);
    }
    store
        .create_list(
            atk.id,
            &NewList {
                name: "all".into(),
                description: None,
                post_policy: None,
                managed: false,
            },
        )
        .await?;
    let all = store
        .list_by_name(atk.id, "all")
        .await?
        .context("list all")?;
    let mut sender = 0;
    for i in 0..199 {
        let seat = store
            .create_seat(
                atk.id,
                &NewSeat {
                    name: format!("a{i:03}"),
                    ..Default::default()
                },
            )
            .await?;
        if i == 0 {
            sender = seat.id;
        }
        store
            .put_member(
                atk.id,
                all.id,
                &Member {
                    address: seat.name.clone(),
                    seat_id: Some(seat.id),
                    mode: amail_proto::DeliveryMode::Immediate,
                },
            )
            .await?;
    }
    let mut vic_tokens = Vec::new();
    for i in 0..4 {
        let seat = store
            .create_seat(
                vic.id,
                &NewSeat {
                    name: format!("v{i}"),
                    ..Default::default()
                },
            )
            .await?;
        let (t, rec) = token(vic.id, TokenScope::Seat, seat.id);
        store.create_token(&rec).await?;
        vic_tokens.push(t);
    }
    drop(store);

    // Bulk mail, in plain SQL that both backends accept.
    let now = amail_store::now_ms();
    let per_msg = 199; // 198 other agent seats and the user seat
    let n_atk = (o.rows / per_msg).max(1);
    let n_thread = o.thread_size.min(n_atk);
    let msgs = |org: i64, sender: i64, n: usize, to: &str, since: i64| {
        format!(
            "WITH RECURSIVE g(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM g WHERE x < {n}) \
             INSERT INTO messages (org_id, sender_seat_id, thread_id, parent_id, depth, subject, body, \
             lines, to_addrs, cc_addrs, priority, ack_required, created_at, meta) \
             SELECT {org}, {sender}, 0, NULL, 0, 'bulk ' || x, 'bulk body ' || x, 1, '{to}', '', \
             CASE WHEN x % 10 = 0 THEN 'urgent' ELSE 'normal' END, false, {since} + x, '' FROM g"
        )
    };
    let mut sql: Vec<String> = Vec::new();
    // The attacker: the first n_thread messages in one thread.
    sql.push(msgs(
        atk.id,
        sender,
        n_atk,
        &format!("all@{atk_name},owner@{atk_name}"),
        now - 3_600_000,
    ));
    let first_atk = format!(
        "(SELECT MIN(id) FROM messages WHERE org_id = {} AND thread_id = 0)",
        atk.id
    );
    sql.push(format!(
        "UPDATE messages SET thread_id = CASE WHEN id < {first_atk} + {n_thread} THEN {first_atk} ELSE id END \
         WHERE org_id = {} AND thread_id = 0",
        atk.id
    ));
    sql.push(format!(
        "INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq) \
         SELECT {a}, m.id, s.id, CASE WHEN s.kind = 'user' THEN 'to' ELSE 'list' END, \
         CASE WHEN (m.id + s.id) % 20 = 0 THEN 'digest' ELSE 'immediate' END, \
         s.last_seq + ROW_NUMBER() OVER (PARTITION BY s.id ORDER BY m.id), \
         s.last_seq + ROW_NUMBER() OVER (PARTITION BY s.id ORDER BY m.id) \
         FROM messages m JOIN seats s ON s.org_id = {a} AND s.deleted_at IS NULL \
         AND s.kind IN ('agent', 'user') AND s.id <> m.sender_seat_id \
         WHERE m.org_id = {a}",
        a = atk.id
    ));
    sql.push(format!(
        "UPDATE deliveries SET read_at = {now} WHERE seat_id = {} AND message_id % 2 = 0",
        atk_user.id
    ));
    // The victim's thread.
    sql.push(msgs(
        vic.id,
        vic_user.id,
        o.thread_size,
        &format!("v0@{vic_name}"),
        now - 7_200_000,
    ));
    let first_vic = format!(
        "(SELECT MIN(id) FROM messages WHERE org_id = {} AND thread_id = 0)",
        vic.id
    );
    sql.push(format!(
        "UPDATE messages SET thread_id = {first_vic} WHERE org_id = {} AND thread_id = 0",
        vic.id
    ));
    sql.push(format!(
        "INSERT INTO deliveries (org_id, message_id, seat_id, class, mode, seq, mod_seq) \
         SELECT {v}, m.id, s.id, 'to', 'immediate', m.id, m.id FROM messages m \
         JOIN seats s ON s.org_id = {v} AND s.name = 'v0' WHERE m.org_id = {v}",
        v = vic.id
    ));
    for org in [atk.id, vic.id] {
        sql.push(format!(
            "UPDATE seats SET last_seq = COALESCE((SELECT MAX(d.seq) FROM deliveries d WHERE d.seat_id = seats.id), last_seq) \
             WHERE org_id = {org}"
        ));
    }
    sql.push("ANALYZE".into());
    let t = Instant::now();
    if o.db.starts_with("sqlite:") {
        use sqlx::ConnectOptions;
        let mut c =
            o.db.parse::<sqlx::sqlite::SqliteConnectOptions>()?
                .busy_timeout(Duration::from_secs(60))
                .connect()
                .await?;
        for q in &sql {
            sqlx::raw_sql(q)
                .execute(&mut c)
                .await
                .with_context(|| q.clone())?;
        }
    } else {
        use sqlx::Connection;
        let mut c = sqlx::PgConnection::connect(&o.db).await?;
        for q in &sql {
            sqlx::raw_sql(q)
                .execute(&mut c)
                .await
                .with_context(|| q.clone())?;
        }
    }
    eprintln!(
        "bulk: {} attacker messages, about {} delivery rows, in {:.1}s",
        n_atk,
        n_atk * per_msg,
        t.elapsed().as_secs_f64()
    );
    let first = |org: i64| async move {
        let store =
            amail_store::open(&o.db, &amail_store::OpenOptions { max_connections: 1 }).await?;
        let a = store
            .archive(
                org,
                &amail_store::ArchiveQuery {
                    limit: 1,
                    ..Default::default()
                },
            )
            .await?;
        anyhow::Ok(a.messages.first().map(|m| (m.id, m.thread_id)))
    };
    let (atk_message, _) = first(atk.id).await?.context("attacker mail")?;
    // The thread ids are the first message of each org.
    let ids = |org: i64| format!("SELECT MIN(id) FROM messages WHERE org_id = {org}");
    let (vic_thread, atk_thread) = if o.db.starts_with("sqlite:") {
        use sqlx::ConnectOptions;
        let mut c =
            o.db.parse::<sqlx::sqlite::SqliteConnectOptions>()?
                .connect()
                .await?;
        let v: i64 = sqlx::query_scalar(&ids(vic.id)).fetch_one(&mut c).await?;
        let a: i64 = sqlx::query_scalar(&ids(atk.id)).fetch_one(&mut c).await?;
        (v, a)
    } else {
        use sqlx::Connection;
        let mut c = sqlx::PgConnection::connect(&o.db).await?;
        let v: i64 = sqlx::query_scalar(&ids(vic.id)).fetch_one(&mut c).await?;
        let a: i64 = sqlx::query_scalar(&ids(atk.id)).fetch_one(&mut c).await?;
        (v, a)
    };
    Ok(Setup {
        atk_tokens,
        vic: vic_tokens,
        vic_thread,
        atk_thread,
        atk_message,
    })
}
