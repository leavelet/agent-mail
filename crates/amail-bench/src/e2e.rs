//! `--mode e2e`: the whole delivery chain, not just the server's endpoints.
//!
//! One real `amail_node` per org runs in this process (its event stream,
//! pending-count refreshes, claims, wake-ups and presence reports are the
//! real code), with one simulated Claude Code session per seat:
//!
//! - The session registers with a `SessionStart` hook, then alternates
//!   between idle and busy. A turn starts when the node wakes it (a line on
//!   the seat's inbox socket, which this process serves) or spontaneously
//!   (like a person typing, `--prompt-every` on average per seat); it is a
//!   `UserPromptSubmit` hook, a few `PostToolUse` hooks `--tool-gap` apart,
//!   then `Stop` (again while the node blocks it to show more mail).
//! - Hooks call the node in-process (the hook client's Unix-socket hop is
//!   left out). Every injected message is recorded with the hook that
//!   showed it.
//!
//! Load: messages at the target rate from random seats, as in the server
//! benchmark (half to a list of 10 in To, half to 10 seats: 1 To + 9 Cc).
//! Browser-like subscribers hold the event stream of some orgs; probe
//! messages to those orgs' users measure how late the event arrives.
//!
//! Reported per rate: send latency; for `to` deliveries, the time from the
//! send's (scheduled) start to the hook that showed it, split by path (`busy`:
//! PostToolUse/Stop of a running turn; `wake`: UserPromptSubmit of a turn
//! the node's wake-up started; `prompt`: a turn started for another
//! reason); duplicates (a message shown twice to a seat); event lag.

use crate::{Opts, World, call, message_for, pct};
use amail_node::NodeHandle;
use anyhow::Result;
use rand::{Rng, SeedableRng};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};
use tokio::time::Instant;

pub struct Sim {
    pub nodes: Vec<NodeHandle>,
    /// Message id -> when its send completed.
    pub sent_at: Arc<std::sync::Mutex<HashMap<i64, Instant>>>,
    pub rec: Arc<Recorder>,
    pub base: Vec<String>,
}

pub struct SeatSim {
    pub name: String,
    pub node: Arc<amail_node::Node>,
    pub env: BTreeMap<String, String>,
    pub sid: String,
}

#[derive(Default)]
pub struct Recorder {
    pub inner: std::sync::Mutex<Rec>,
    pub hooks: AtomicU64,
    pub wakes: AtomicU64,
}

#[derive(Default)]
pub struct Rec {
    pub measuring: bool,
    /// (seat, message) pairs shown so far, to find duplicates.
    pub seen: HashSet<(String, i64)>,
    pub dup: u64,
    pub busy_ms: Vec<u64>,
    pub wake_ms: Vec<u64>,
    pub prompt_ms: Vec<u64>,
    pub cc_shown: u64,
    pub hook_us: Vec<u64>,
    pub inject_hook_us: Vec<u64>,
    pub lag_ms: Vec<u64>,
    pub unknown: u64,
}

/// Header lines of the new-mail part of an injection: `(id, is_cc)`.
fn shown(text: &str) -> Vec<(i64, bool)> {
    let mut out = Vec::new();
    let mut new_mail = false;
    for l in text.lines() {
        if l.starts_with("[amail]") {
            new_mail = l.contains("new message(s)");
            continue;
        }
        if !new_mail {
            continue;
        }
        if let Some(rest) = l.strip_prefix('#')
            && rest.contains(" from ")
            && let Some(id) = rest.split(' ').next().and_then(|x| x.parse().ok())
        {
            out.push((id, l.contains(" · cc · ")));
        }
    }
    out
}

impl SeatSim {
    async fn hook(
        &self,
        rec: &Recorder,
        sent: &std::sync::Mutex<HashMap<i64, Instant>>,
        input: Value,
        path: &str,
    ) -> bool {
        let t0 = Instant::now();
        let out = self
            .node
            .hook_outcome("claude", &self.env, &input.to_string(), None)
            .await;
        let dt = t0.elapsed().as_micros() as u64;
        rec.hooks.fetch_add(1, Ordering::Relaxed);
        if out.stdout.is_empty() {
            let mut r = rec.inner.lock().unwrap();
            if r.measuring {
                r.hook_us.push(dt);
            }
            return false;
        }
        let v: Value = serde_json::from_str(&out.stdout).unwrap_or(Value::Null);
        let text = v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .or_else(|| v["reason"].as_str())
            .unwrap_or_default();
        let now = Instant::now();
        let ids = shown(text);
        let sent = sent.lock().unwrap();
        let mut r = rec.inner.lock().unwrap();
        if r.measuring {
            r.hook_us.push(dt);
            r.inject_hook_us.push(dt);
        }
        for (id, cc) in ids {
            if !r.seen.insert((self.name.clone(), id)) {
                r.dup += 1;
                continue;
            }
            if !r.measuring {
                continue;
            }
            if cc {
                r.cc_shown += 1;
                continue;
            }
            let Some(at) = sent.get(&id) else {
                r.unknown += 1;
                continue;
            };
            let ms = now.saturating_duration_since(*at).as_millis() as u64;
            match path {
                "busy" => r.busy_ms.push(ms),
                "wake" => r.wake_ms.push(ms),
                _ => r.prompt_ms.push(ms),
            }
        }
        v["decision"] == "block"
    }

    /// The session: idle until woken or prompted, then a turn.
    async fn run(
        self: Arc<Self>,
        mut wake: mpsc::UnboundedReceiver<()>,
        rec: Arc<Recorder>,
        sent: Arc<std::sync::Mutex<HashMap<i64, Instant>>>,
        prompt_every: f64,
        tool_gap: (f64, f64),
    ) {
        let mut rng = rand::rngs::StdRng::from_rng(&mut rand::rng());
        let sid = self.sid.clone();
        self.hook(
            &rec,
            &sent,
            json!({"hook_event_name": "SessionStart", "session_id": sid, "source": "startup"}),
            "-",
        )
        .await;
        loop {
            let gap = -prompt_every * (1.0 - rng.random::<f64>()).ln();
            let cause = tokio::select! {
                w = wake.recv() => {
                    if w.is_none() { return; }
                    rec.wakes.fetch_add(1, Ordering::Relaxed);
                    "wake"
                }
                _ = tokio::time::sleep(Duration::from_secs_f64(gap)) => "prompt",
            };
            // Coalesce extra wake-ups that came with this one.
            while wake.try_recv().is_ok() {}
            self.hook(
                &rec,
                &sent,
                json!({"hook_event_name": "UserPromptSubmit", "session_id": sid,
                       "prompt": "[amail] You have new mail."}),
                cause,
            )
            .await;
            let mut tools = rng.random_range(1..=4);
            let mut blocks = 0;
            loop {
                for _ in 0..tools {
                    let d = rng.random_range(tool_gap.0..tool_gap.1);
                    tokio::time::sleep(Duration::from_secs_f64(d)).await;
                    self.hook(
                        &rec,
                        &sent,
                        json!({"hook_event_name": "PostToolUse", "session_id": sid, "tool_name": "Bash"}),
                        "busy",
                    )
                    .await;
                }
                let blocked = self
                    .hook(
                        &rec,
                        &sent,
                        json!({"hook_event_name": "Stop", "session_id": sid, "stop_hook_active": blocks > 0}),
                        "busy",
                    )
                    .await;
                if !blocked || blocks >= 5 {
                    break;
                }
                blocks += 1;
                tools = rng.random_range(0..=2);
            }
        }
    }
}

/// Serve a seat's inbox socket: each line is a wake-up.
fn serve_inbox(path: &std::path::Path, tx: mpsc::UnboundedSender<()>) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)?;
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        loop {
            let Ok((s, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut r = tokio::io::BufReader::new(s).lines();
                while let Ok(Some(_)) = r.next_line().await {
                    let _ = tx.send(());
                }
            });
        }
    });
    Ok(())
}

pub async fn start(o: &Opts, http: &reqwest::Client, w: &World) -> Result<Sim> {
    let bases: Vec<String> = o
        .url
        .split(',')
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .collect();
    let run_dir = PathBuf::from(
        o.run_dir
            .clone()
            .unwrap_or_else(|| "amail-bench-run".into()),
    );
    std::fs::create_dir_all(&run_dir)?;
    let rec = Arc::new(Recorder::default());
    let sent_at = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let mut nodes = Vec::new();
    let mut seats = 0;
    let t0 = std::time::Instant::now();
    for org in 0..o.orgs {
        let base = &bases[org % bases.len()];
        let token = call(
            http,
            format!("{base}/v1/tokens"),
            &w.user_tokens[org],
            reqwest::Method::POST,
            json!({"scope": "node", "label": format!("bench-{org}")}),
        )
        .await?["token"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let dir = run_dir.join(format!("n{org:03}"));
        std::fs::create_dir_all(dir.join("s"))?;
        let mut cfg = amail_node::Config::new(base, &token);
        cfg.state_dir = dir.join("state");
        cfg.socket = dir.join("node.sock");
        cfg.label = Some(format!("bench-node-{org}"));
        cfg.codex_bin = "/bin/false".into();
        cfg.claude_bin = "/bin/false".into();
        cfg.codex_auth = dir.join("no-auth.json");
        let h = amail_node::start_with_mux(cfg, Arc::new(amail_node::mux::ProcessMux::new(vec![])))
            .await?;
        let node = h.node.clone();
        nodes.push(h);
        for name in &w.seat_names[org] {
            let sock = dir.join("s").join(format!("{name}.sock"));
            let (tx, rx) = mpsc::unbounded_channel();
            serve_inbox(&sock, tx)?;
            let env = BTreeMap::from([
                ("AMAIL_SEAT".to_string(), name.clone()),
                (
                    "CLAUDE_CODE_MESSAGING_SOCKET".to_string(),
                    sock.to_string_lossy().into_owned(),
                ),
                ("CLAUDE_PID".to_string(), std::process::id().to_string()),
            ]);
            let s = Arc::new(SeatSim {
                name: name.clone(),
                node: node.clone(),
                env,
                sid: format!("bench-{org}-{name}"),
            });
            tokio::spawn(s.clone().run(
                rx,
                rec.clone(),
                sent_at.clone(),
                o.prompt_every,
                (o.tool_gap_min, o.tool_gap_max),
            ));
            seats += 1;
        }
    }
    eprintln!(
        "e2e: {} nodes, {} simulated sessions in {:.1}s",
        nodes.len(),
        seats,
        t0.elapsed().as_secs_f64()
    );
    // Let the nodes connect and report.
    tokio::time::sleep(Duration::from_secs(5)).await;
    Ok(Sim {
        nodes,
        sent_at,
        rec,
        base: bases,
    })
}

/// Browser-like subscribers: the org's event stream; on an event for the
/// user seat, read the user's new mail and note how late each probe came.
fn browsers(
    o: &Opts,
    w: &Arc<World>,
    sim: &Sim,
    probes: Arc<std::sync::Mutex<HashMap<i64, Instant>>>,
    stop: Arc<tokio::sync::Notify>,
) {
    for org in 0..o.browsers.min(o.orgs) {
        let base = sim.base[(org + 1) % sim.base.len()].clone();
        let token = w.user_tokens[org].clone();
        let rec = sim.rec.clone();
        let probes = probes.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            let mut after = 0i64;
            loop {
                let resp = http
                    .get(format!("{base}/v1/events"))
                    .bearer_auth(&token)
                    .send()
                    .await;
                let Ok(resp) = resp else {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                };
                use futures::StreamExt;
                let mut s = resp.bytes_stream();
                let mut buf = String::new();
                loop {
                    let chunk = tokio::select! {
                        c = s.next() => c,
                        _ = stop.notified() => return,
                    };
                    let Some(Ok(chunk)) = chunk else { break };
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    let mut owner_changed = false;
                    while let Some(i) = buf.find("\n\n") {
                        let ev: String = buf.drain(..i + 2).collect();
                        if ev.contains("event: mailbox") && ev.contains("\"seat\":\"owner\"") {
                            owner_changed = true;
                        }
                    }
                    if !owner_changed {
                        continue;
                    }
                    let arrived = Instant::now();
                    let Ok(v) = call(
                        &http,
                        format!("{base}/v1/inbox?after_seq={after}&limit=200"),
                        &token,
                        reqwest::Method::GET,
                        Value::Null,
                    )
                    .await
                    else {
                        continue;
                    };
                    let probes = probes.lock().unwrap();
                    let mut r = rec.inner.lock().unwrap();
                    for e in v["entries"].as_array().into_iter().flatten() {
                        after = after.max(e["delivery"]["seq"].as_i64().unwrap_or(0));
                        if let Some(at) = e["message"]["id"].as_i64().and_then(|id| probes.get(&id))
                            && r.measuring
                        {
                            r.lag_ms
                                .push(arrived.saturating_duration_since(*at).as_millis() as u64);
                        }
                    }
                }
            }
        });
    }
}

pub async fn run_step(
    o: &Opts,
    http: &reqwest::Client,
    w: Arc<World>,
    sim: &Sim,
    rate: f64,
) -> Value {
    let start = Instant::now() + Duration::from_millis(200);
    let measure_from = start + Duration::from_secs(o.warmup);
    let end = measure_from + Duration::from_secs(o.duration);
    let wall0 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
        + 200;
    let window_unix_ms = [
        wall0 + o.warmup * 1000,
        wall0 + (o.warmup + o.duration) * 1000,
    ];
    let sem = Arc::new(tokio::sync::Semaphore::new(o.max_inflight));
    let send_us = Arc::new(Mutex::new(Vec::<u64>::new()));
    let errors = Arc::new(AtomicU64::new(0));
    let first_error = Arc::new(std::sync::Mutex::new(None::<String>));
    let sent = Arc::new(AtomicU64::new(0));
    let probes = Arc::new(std::sync::Mutex::new(HashMap::new()));
    let stop = Arc::new(tokio::sync::Notify::new());
    browsers(o, &w, sim, probes.clone(), stop.clone());
    {
        let mut r = sim.rec.inner.lock().unwrap();
        let seen = std::mem::take(&mut r.seen);
        let dup = r.dup;
        *r = Rec {
            seen,
            dup,
            ..Default::default()
        };
    }
    let dup_before = sim.rec.inner.lock().unwrap().dup;
    let (hooks0, wakes0) = (
        sim.rec.hooks.load(Ordering::Relaxed),
        sim.rec.wakes.load(Ordering::Relaxed),
    );
    let measuring = {
        let rec = sim.rec.clone();
        tokio::spawn(async move {
            tokio::time::sleep_until(measure_from).await;
            rec.inner.lock().unwrap().measuring = true;
            tokio::time::sleep_until(end).await;
            rec.inner.lock().unwrap().measuring = false;
        })
    };
    // Probes: messages to the users of the orgs with browsers.
    let prober = {
        let (http, w, probes, base) = (http.clone(), w.clone(), probes.clone(), sim.base.clone());
        let n = o.browsers.min(o.orgs);
        tokio::spawn(async move {
            let mut i = 0usize;
            while n > 0 && Instant::now() < end {
                let org = i % n;
                i += 1;
                let seat = w
                    .seats
                    .iter()
                    .find(|s| s.org == org)
                    .map(|s| s.token.clone());
                let t0 = Instant::now();
                if let Some(t) = seat
                    && let Ok(v) = call(
                        &http,
                        format!("{}/v1/messages", base[i % base.len()]),
                        &t,
                        reqwest::Method::POST,
                        json!({"to": ["owner"], "subject": "probe", "body": "p"}),
                    )
                    .await
                    && let Some(id) = v["id"].as_i64()
                {
                    probes.lock().unwrap().insert(id, t0);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
    };
    let mut rng = rand::rngs::StdRng::from_rng(&mut rand::rng());
    let mut i: u64 = 0;
    let mut overloaded = false;
    loop {
        let at = start + Duration::from_secs_f64(i as f64 / rate);
        if at >= end {
            break;
        }
        i += 1;
        tokio::time::sleep_until(at).await;
        let permit = sem.clone().acquire_owned().await.unwrap();
        // Far behind schedule: the server cannot keep up; end the step.
        if Instant::now() > at + Duration::from_secs(30) {
            overloaded = true;
            break;
        }
        let (si, msg) = message_for(o, &w, &mut rng);
        let url = format!("{}/v1/messages", sim.base[i as usize % sim.base.len()]);
        let (http, w, send_us, errors, first_error, sent, sent_at) = (
            http.clone(),
            w.clone(),
            send_us.clone(),
            errors.clone(),
            first_error.clone(),
            sent.clone(),
            sim.sent_at.clone(),
        );
        tokio::spawn(async move {
            let r = call(&http, url, &w.seats[si].token, reqwest::Method::POST, msg).await;
            let done = Instant::now();
            drop(permit);
            match r {
                Ok(v) => {
                    // Delivery latencies count from the send's (scheduled) start.
                    if let Some(id) = v["id"].as_i64() {
                        sent_at.lock().unwrap().insert(id, at);
                    }
                    if at >= measure_from {
                        sent.fetch_add(1, Ordering::Relaxed);
                        send_us.lock().await.push((done - at).as_micros() as u64);
                    }
                }
                Err(e) => {
                    if at >= measure_from {
                        errors.fetch_add(1, Ordering::Relaxed);
                        first_error.lock().unwrap().get_or_insert(e.to_string());
                    }
                }
            }
        });
    }
    // Wait for sends in flight, at most a minute.
    let unfinished = match tokio::time::timeout(
        Duration::from_secs(60),
        sem.acquire_many(o.max_inflight as u32),
    )
    .await
    {
        Ok(_) => 0,
        Err(_) => o.max_inflight - sem.available_permits(),
    };
    // Let deliveries sent near the end of the window arrive.
    tokio::time::sleep(Duration::from_secs(o.drain)).await;
    measuring.await.ok();
    prober.await.ok();
    stop.notify_waiters();
    let window = o.duration as f64;
    let mut s = send_us.lock().await;
    let mut r = sim.rec.inner.lock().unwrap();
    let stats = |v: &mut Vec<u64>| json!({"n": v.len(), "p50": pct(v, 0.5), "p90": pct(v, 0.9), "p99": pct(v, 0.99), "max": pct(v, 1.0)});
    let ms = |v: &mut Vec<u64>| -> Vec<u64> { v.iter().map(|x| x * 1000).collect() };
    let mut busy = ms(&mut r.busy_ms);
    let mut wake = ms(&mut r.wake_ms);
    let mut prompt = ms(&mut r.prompt_ms);
    let mut lag = ms(&mut r.lag_ms);
    json!({
        "mode": "e2e",
        "target_rate": rate,
        "window_unix_ms": window_unix_ms,
        "duration_s": o.duration,
        "sent_per_s": sent.load(Ordering::Relaxed) as f64 / window,
        "send_ms": stats(&mut s),
        "errors": errors.load(Ordering::Relaxed),
        "overloaded": overloaded,
        "unfinished_sends": unfinished,
        "scheduled_sends": i,
        "first_error": first_error.lock().unwrap().clone(),
        "to_busy_ms": stats(&mut busy),
        "to_wake_ms": stats(&mut wake),
        "to_prompt_ms": stats(&mut prompt),
        "cc_shown": r.cc_shown,
        "duplicates": r.dup - dup_before,
        "unknown_ids": r.unknown,
        "hook_us": stats(&mut r.hook_us),
        "injecting_hook_us": stats(&mut r.inject_hook_us),
        "hooks_per_s": (sim.rec.hooks.load(Ordering::Relaxed) - hooks0) as f64 / (window + o.warmup as f64 + o.drain as f64),
        "wakes_per_s": (sim.rec.wakes.load(Ordering::Relaxed) - wakes0) as f64 / (window + o.warmup as f64 + o.drain as f64),
        "event_lag_ms": stats(&mut lag),
    })
}
