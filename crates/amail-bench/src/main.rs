//! `amail-bench`: load generator for an `amail serve` instance.
//!
//! Setup: creates `--orgs` orgs directly in the database (like `amail admin
//! create-org`), then over HTTP `--seats-per-org` agent seats with tokens and
//! lists of `--list-size` consecutive seats per org.
//!
//! Load, for each rate in `--rates`: an open-loop sender issues messages at
//! the target rate from random seats. A `--list-share` fraction goes to one of
//! the org's lists (one the sender is not on); the rest goes directly to
//! `--fanout` seats of the org (1 To + rest Cc). Concurrently every seat
//! claims its pending notifications once per `--claim-interval` seconds.
//! Latencies are measured from each request's scheduled start, so queueing
//! inside the client (when `--max-inflight` is exhausted) counts against the
//! server.

mod e2e;
mod tenants;

use anyhow::{Context, Result, bail};
use clap::Parser;
use futures::{StreamExt, stream};
use rand::seq::IndexedRandom;
use rand::{Rng, SeedableRng};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

#[derive(Parser, Clone)]
#[command(about = "Load generator for agent-mail")]
pub struct Opts {
    /// Base URL of a running `amail serve` (`e2e`: several, comma-separated,
    /// for several instances; nodes and sends are spread over them).
    #[arg(long, default_value = "http://127.0.0.1:8025")]
    url: String,
    /// `server`: sends and periodic claims against the API. `e2e`: one
    /// real node per org with simulated sessions (see `e2e.rs`). `tenants`:
    /// one org's load against another org's latency (see `tenants.rs`).
    #[arg(long, default_value = "server")]
    mode: String,
    /// e2e: where nodes keep state and seats' inbox sockets.
    #[arg(long)]
    run_dir: Option<String>,
    /// e2e: mean seconds between turns a seat starts on its own.
    #[arg(long, default_value_t = 300.0)]
    prompt_every: f64,
    /// e2e: seconds between tool calls in a turn (uniform in this range).
    #[arg(long, default_value_t = 1.0)]
    tool_gap_min: f64,
    #[arg(long, default_value_t = 5.0)]
    tool_gap_max: f64,
    /// e2e: orgs with a browser-like event subscriber.
    #[arg(long, default_value_t = 30)]
    browsers: usize,
    /// e2e: seconds after each step's window during which late deliveries
    /// still count.
    #[arg(long, default_value_t = 10)]
    drain: u64,
    /// Database of that server (used only to create the orgs).
    #[arg(long)]
    db: String,
    #[arg(long, default_value_t = 100)]
    orgs: usize,
    #[arg(long, default_value_t = 100)]
    seats_per_org: usize,
    #[arg(long, default_value_t = 10)]
    list_size: usize,
    /// Recipients of a direct (non-list) message.
    #[arg(long, default_value_t = 10)]
    fanout: usize,
    /// Fraction of messages sent to a list.
    #[arg(long, default_value_t = 0.5)]
    list_share: f64,
    /// Comma-separated target send rates (messages per second).
    #[arg(long, value_delimiter = ',', default_value = "170")]
    rates: Vec<f64>,
    /// Measured seconds per rate.
    #[arg(long, default_value_t = 30)]
    duration: u64,
    /// Unmeasured seconds before each measurement.
    #[arg(long, default_value_t = 5)]
    warmup: u64,
    /// Every seat claims once per this many seconds (0 disables claims).
    #[arg(long, default_value_t = 10.0)]
    claim_interval: f64,
    /// Bound on concurrent requests of each kind.
    #[arg(long, default_value_t = 2048)]
    max_inflight: usize,
    #[arg(long, default_value_t = 200)]
    body_bytes: usize,
    /// Prefix for org names, so several runs can share a database.
    #[arg(long, default_value = "bench")]
    prefix: String,
    /// tenants: delivery rows of the attacking org (bulk-inserted).
    #[arg(long, default_value_t = 2_000_000)]
    rows: usize,
    /// tenants: messages in the victim's thread and in the attacker's.
    #[arg(long, default_value_t = 60_000)]
    thread_size: usize,
    /// tenants: user tokens of the attacking org.
    #[arg(long, default_value_t = 64)]
    attack_tokens: usize,
    /// tenants: concurrent request loops of the attacking org.
    #[arg(long, default_value_t = 128)]
    attack_conc: usize,
    /// tenants: requests per second of each victim operation.
    #[arg(long, default_value_t = 5.0)]
    victim_rate: f64,
    /// tenants: comma-separated phases (see `tenants.rs`).
    #[arg(
        long,
        default_value = "pending,counts,overview,attention,thread,own-thread,search,archive,lists,status,changes,inbox,all"
    )]
    phases: String,
    /// Write per-rate results as JSON lines to this file.
    #[arg(long)]
    out: Option<String>,
}

pub struct SeatW {
    org: usize,
    idx: usize,
    token: String,
}

pub struct World {
    seats: Vec<SeatW>,
    /// Per org: list names; list j holds seats [j*list_size, (j+1)*list_size).
    lists: Vec<Vec<String>>,
    seat_names: Vec<Vec<String>>,
    user_tokens: Arc<Vec<String>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let opts = Opts::parse();
    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(opts.max_inflight * 2 + 64)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(120))
        .build()?;
    if opts.mode == "tenants" {
        return tenants::run(&opts, &http).await;
    }
    let t = Instant::now();
    let world = Arc::new(setup(&opts, &http).await?);
    if opts.mode == "e2e" {
        return run_e2e(&opts, &http, world).await;
    }
    eprintln!(
        "setup: {} orgs, {} seats, {} lists in {:.1}s",
        opts.orgs,
        world.seats.len(),
        world.lists.iter().map(Vec::len).sum::<usize>(),
        t.elapsed().as_secs_f64()
    );
    println!(
        "{:>8} {:>9} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>7}",
        "target",
        "sent/s",
        "deliv/s",
        "send_p50",
        "send_p99",
        "send_max",
        "claim/s",
        "clm_p50",
        "clm_p99",
        "claimed/s",
        "errors"
    );
    for &rate in &opts.rates {
        let r = run_step(&opts, &http, world.clone(), rate).await;
        println!(
            "{:>8.0} {:>9.1} {:>10.1} {:>9} {:>9} {:>9} {:>9.1} {:>9} {:>9} {:>9.1} {:>7}",
            rate,
            r["sent_per_s"].as_f64().unwrap_or(0.0),
            r["deliveries_per_s"].as_f64().unwrap_or(0.0),
            ms(&r["send_ms"]["p50"]),
            ms(&r["send_ms"]["p99"]),
            ms(&r["send_ms"]["max"]),
            r["claims_per_s"].as_f64().unwrap_or(0.0),
            ms(&r["claim_ms"]["p50"]),
            ms(&r["claim_ms"]["p99"]),
            r["claimed_per_s"].as_f64().unwrap_or(0.0),
            r["errors"].as_u64().unwrap_or(0),
        );
        if let Some(path) = &opts.out {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(f, "{r}")?;
        }
    }
    Ok(())
}

async fn run_e2e(opts: &Opts, http: &reqwest::Client, world: Arc<World>) -> Result<()> {
    let sim = e2e::start(opts, http, &world).await?;
    println!(
        "{:>7} {:>8} {:>8} {:>8} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>5} {:>8} {:>8}",
        "target",
        "sent/s",
        "send_p50",
        "send_p99",
        "busy_p50",
        "busy_p99",
        "wake_p50",
        "wake_p99",
        "lag_p50",
        "lag_p99",
        "dups",
        "hooks/s",
        "errors"
    );
    for &rate in &opts.rates {
        let r = e2e::run_step(opts, http, world.clone(), &sim, rate).await;
        println!(
            "{:>7.0} {:>8.1} {:>8} {:>8} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9} {:>5} {:>8.0} {:>8}",
            rate,
            r["sent_per_s"].as_f64().unwrap_or(0.0),
            ms(&r["send_ms"]["p50"]),
            ms(&r["send_ms"]["p99"]),
            ms(&r["to_busy_ms"]["p50"]),
            ms(&r["to_busy_ms"]["p99"]),
            ms(&r["to_wake_ms"]["p50"]),
            ms(&r["to_wake_ms"]["p99"]),
            ms(&r["event_lag_ms"]["p50"]),
            ms(&r["event_lag_ms"]["p99"]),
            r["duplicates"],
            r["hooks_per_s"].as_f64().unwrap_or(0.0),
            r["errors"],
        );
        if let Some(e) = r["first_error"].as_str() {
            eprintln!("first error at rate {rate}: {e}");
        }
        if let Some(path) = &opts.out {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(f, "{r}")?;
        }
    }
    for n in sim.nodes {
        n.shutdown();
    }
    Ok(())
}

fn ms(v: &Value) -> String {
    v.as_f64()
        .map(|x| format!("{x:.1}ms"))
        .unwrap_or_else(|| "-".into())
}

pub(crate) async fn call(
    http: &reqwest::Client,
    url: String,
    token: &str,
    method: reqwest::Method,
    body: Value,
) -> Result<Value> {
    let resp = http
        .request(method, url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("HTTP {status}: {v}");
    }
    Ok(v)
}

async fn setup(o: &Opts, http: &reqwest::Client) -> Result<World> {
    let store = amail_store::open(&o.db, &amail_store::OpenOptions { max_connections: 4 }).await?;
    let mut user_tokens = Vec::new();
    for i in 0..o.orgs {
        let token = amail_server::new_token();
        store
            .create_org(
                &format!("{}{i:03}", o.prefix),
                "owner",
                &amail_server::hash_token(&token),
            )
            .await
            .with_context(|| {
                format!("creating org {i} (use a fresh database or another --prefix)")
            })?;
        user_tokens.push(token);
    }
    drop(store);
    let user_tokens = Arc::new(user_tokens);

    let seat_names: Vec<Vec<String>> = (0..o.orgs)
        .map(|_| (0..o.seats_per_org).map(|s| format!("s{s:03}")).collect())
        .collect();
    let n_lists = o.seats_per_org / o.list_size.max(1);
    let lists: Vec<Vec<String>> = (0..o.orgs)
        .map(|_| (0..n_lists).map(|j| format!("team{j:02}")).collect())
        .collect();

    // Org settings: lift the per-seat budget so it does not cap the benchmark.
    let base = o
        .url
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    for t in user_tokens.iter() {
        call(
            http,
            format!("{base}/v1/org"),
            t,
            reqwest::Method::PATCH,
            json!({ "send_rate_per_min": 100000, "send_burst": 1000 }),
        )
        .await?;
    }

    // Seats and tokens.
    let jobs: Vec<(usize, usize)> = (0..o.orgs)
        .flat_map(|org| (0..o.seats_per_org).map(move |s| (org, s)))
        .collect();
    let seats: Vec<Result<SeatW>> = stream::iter(jobs)
        .map(|(org, idx)| {
            let (http, base, user_tokens) = (http.clone(), base.clone(), user_tokens.clone());
            let name = seat_names[org][idx].clone();
            async move {
                let t = &user_tokens[org];
                call(
                    &http,
                    format!("{base}/v1/seats"),
                    t,
                    reqwest::Method::POST,
                    json!({ "name": name }),
                )
                .await?;
                let v = call(
                    &http,
                    format!("{base}/v1/seats/{name}/tokens"),
                    t,
                    reqwest::Method::POST,
                    json!({}),
                )
                .await?;
                Ok(SeatW {
                    org,
                    idx,
                    token: v["token"].as_str().unwrap_or_default().to_string(),
                })
            }
        })
        .buffered(64)
        .collect()
        .await;
    let seats: Vec<SeatW> = seats.into_iter().collect::<Result<_>>()?;

    // Lists and members.
    let jobs: Vec<(usize, usize)> = (0..o.orgs)
        .flat_map(|org| (0..n_lists).map(move |j| (org, j)))
        .collect();
    let results: Vec<Result<()>> = stream::iter(jobs)
        .map(|(org, j)| {
            let (http, base, user_tokens) = (http.clone(), base.clone(), user_tokens.clone());
            let list = lists[org][j].clone();
            let members: Vec<String> =
                seat_names[org][j * o.list_size..(j + 1) * o.list_size].to_vec();
            async move {
                let t = &user_tokens[org];
                call(
                    &http,
                    format!("{base}/v1/lists"),
                    t,
                    reqwest::Method::POST,
                    json!({ "name": list }),
                )
                .await?;
                for m in members {
                    call(
                        &http,
                        format!("{base}/v1/lists/{list}/members/{m}"),
                        t,
                        reqwest::Method::PUT,
                        json!({}),
                    )
                    .await?;
                }
                Ok(())
            }
        })
        .buffer_unordered(64)
        .collect()
        .await;
    results.into_iter().collect::<Result<Vec<()>>>()?;
    Ok(World {
        seats,
        lists,
        seat_names,
        user_tokens,
    })
}

#[derive(Default)]
struct Stats {
    send_us: Vec<u64>,
    claim_us: Vec<u64>,
    sent: u64,
    deliveries: u64,
    /// Sends that completed inside the measurement window (any schedule time).
    done_in_window: u64,
    deliveries_in_window: u64,
    claims: u64,
    claimed: u64,
    errors: u64,
    first_error: Option<String>,
}

pub(crate) fn pct(v: &mut [u64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    Some(v[i] as f64 / 1000.0)
}

pub(crate) fn message_for(o: &Opts, w: &World, rng: &mut impl Rng) -> (usize, Value) {
    let si = rng.random_range(0..w.seats.len());
    let s = &w.seats[si];
    let names = &w.seat_names[s.org];
    let body: String = "lorem ipsum dolor sit amet "
        .chars()
        .cycle()
        .take(o.body_bytes)
        .collect();
    let n_lists = w.lists[s.org].len();
    let own_list = s.idx / o.list_size.max(1);
    let msg = if n_lists > 1 && rng.random_bool(o.list_share) {
        let mut j = rng.random_range(0..n_lists);
        if j == own_list {
            j = (j + 1) % n_lists;
        }
        json!({ "to": [w.lists[s.org][j]], "subject": "bench list", "body": body })
    } else {
        let others: Vec<&String> = names
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != s.idx)
            .map(|(_, n)| n)
            .collect();
        let picked: Vec<&String> = others
            .choose_multiple(rng, o.fanout.min(others.len()))
            .copied()
            .collect();
        json!({ "to": [picked[0]], "cc": picked[1..], "subject": "bench direct", "body": body })
    };
    (si, msg)
}

async fn run_step(o: &Opts, http: &reqwest::Client, w: Arc<World>, rate: f64) -> Value {
    let base = o
        .url
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();
    let stats = Arc::new(Mutex::new(Stats::default()));
    let start = Instant::now() + Duration::from_millis(200);
    let measure_from = start + Duration::from_secs(o.warmup);
    let end = measure_from + Duration::from_secs(o.duration);
    let send_sem = Arc::new(Semaphore::new(o.max_inflight));
    let claim_sem = Arc::new(Semaphore::new(o.max_inflight));
    let late = Arc::new(AtomicU64::new(0));

    // Sender: open loop at `rate`.
    let sender = {
        let (http, w, stats, sem, base, o) = (
            http.clone(),
            w.clone(),
            stats.clone(),
            send_sem.clone(),
            base.clone(),
            o.clone(),
        );
        tokio::spawn(async move {
            let mut rng = rand::rngs::StdRng::from_rng(&mut rand::rng());
            let mut i: u64 = 0;
            loop {
                let at = start + Duration::from_secs_f64(i as f64 / rate);
                if at >= end {
                    break;
                }
                i += 1;
                tokio::time::sleep_until(at).await;
                let permit = sem.clone().acquire_owned().await.unwrap();
                let (si, msg) = message_for(&o, &w, &mut rng);
                let (http, w, stats, base) = (http.clone(), w.clone(), stats.clone(), base.clone());
                tokio::spawn(async move {
                    let r = call(
                        &http,
                        format!("{base}/v1/messages"),
                        &w.seats[si].token,
                        reqwest::Method::POST,
                        msg,
                    )
                    .await;
                    let done = Instant::now();
                    drop(permit);
                    let mut s = stats.lock().await;
                    if let (Ok(v), true) = (&r, done >= measure_from && done < end) {
                        s.done_in_window += 1;
                        s.deliveries_in_window += v["deliveries"].as_u64().unwrap_or(0);
                    }
                    if at < measure_from {
                        return;
                    }
                    match r {
                        Ok(v) => {
                            s.sent += 1;
                            s.deliveries += v["deliveries"].as_u64().unwrap_or(0);
                            s.send_us.push((done - at).as_micros() as u64);
                        }
                        Err(e) => {
                            s.errors += 1;
                            s.first_error.get_or_insert(e.to_string());
                        }
                    }
                });
            }
        })
    };

    // Claimers: every seat once per interval, spread evenly.
    let claimer = (o.claim_interval > 0.0).then(|| {
        let (http, w, stats, sem, base, late) = (
            http.clone(),
            w.clone(),
            stats.clone(),
            claim_sem.clone(),
            base.clone(),
            late.clone(),
        );
        let claim_rate = w.seats.len() as f64 / o.claim_interval;
        tokio::spawn(async move {
            let mut i: u64 = 0;
            loop {
                let at = start + Duration::from_secs_f64(i as f64 / claim_rate);
                if at >= end {
                    break;
                }
                let si = (i as usize) % w.seats.len();
                i += 1;
                tokio::time::sleep_until(at).await;
                let permit = sem.clone().acquire_owned().await.unwrap();
                if Instant::now() > at + Duration::from_millis(100) {
                    late.fetch_add(1, Ordering::Relaxed);
                }
                let (http, w, stats, base) = (http.clone(), w.clone(), stats.clone(), base.clone());
                tokio::spawn(async move {
                    let body =
                        json!({ "via": "bench", "classes": ["to", "cc", "list"], "limit": 100 });
                    let r = call(
                        &http,
                        format!("{base}/v1/claim"),
                        &w.seats[si].token,
                        reqwest::Method::POST,
                        body,
                    )
                    .await;
                    let done = Instant::now();
                    drop(permit);
                    if at < measure_from {
                        return;
                    }
                    let mut s = stats.lock().await;
                    match r {
                        Ok(v) => {
                            s.claims += 1;
                            s.claimed +=
                                v["entries"].as_array().map(|a| a.len() as u64).unwrap_or(0);
                            s.claim_us.push((done - at).as_micros() as u64);
                        }
                        Err(e) => {
                            s.errors += 1;
                            s.first_error.get_or_insert(e.to_string());
                        }
                    }
                });
            }
        })
    });

    sender.await.unwrap();
    if let Some(c) = claimer {
        c.await.unwrap();
    }
    // Wait for in-flight requests.
    let _a = send_sem.acquire_many(o.max_inflight as u32).await.unwrap();
    let _b = claim_sem.acquire_many(o.max_inflight as u32).await.unwrap();
    let wall = (Instant::now().max(end) - measure_from).as_secs_f64();
    let window = o.duration as f64;
    let mut s = stats.lock().await;
    let r = json!({
        "target_rate": rate,
        "duration_s": o.duration,
        "wall_s": wall,
        "sent": s.sent,
        "sent_per_s": s.done_in_window as f64 / window,
        "deliveries_per_s": s.deliveries_in_window as f64 / window,
        "scheduled_sends_completed": s.sent,
        "send_ms": { "p50": pct(&mut s.send_us, 0.50), "p90": pct(&mut s.send_us, 0.90), "p99": pct(&mut s.send_us, 0.99), "max": pct(&mut s.send_us, 1.0) },
        "claims_per_s": s.claims as f64 / window,
        "claimed_per_s": s.claimed as f64 / window,
        "claim_ms": { "p50": pct(&mut s.claim_us, 0.50), "p90": pct(&mut s.claim_us, 0.90), "p99": pct(&mut s.claim_us, 0.99), "max": pct(&mut s.claim_us, 1.0) },
        "claims_started_late": late.load(Ordering::Relaxed),
        "errors": s.errors,
        "first_error": s.first_error,
    });
    if let Some(e) = &s.first_error {
        eprintln!("first error at rate {rate}: {e}");
    }
    r
}
