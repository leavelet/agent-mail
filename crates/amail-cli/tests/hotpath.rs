//! Hook hot path: 1,000 seats with live sessions on one node, no mail. The
//! end-to-end latency of `amail hook` (process start to exit) must have a
//! p99 under 10 ms. The bound is enforced in optimized builds
//! (`cargo test --release`); debug builds only report the numbers.

mod common;

use common::*;
use std::time::Duration;

const SEATS: usize = 1000;
const SAMPLES: usize = 2000;

fn pct(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hook_hot_path_1000_seats() {
    let h = Harness::start(
        amail_store::testutil::Backend::Sqlite,
        "hook_hot_path",
        Opts::default(),
    )
    .await
    .unwrap();
    for i in 0..SEATS {
        h.api
            .post(
                "/seats",
                serde_json::json!({"name": format!("s{i:04}"), "harness": "claude"}),
            )
            .await;
    }
    // Every seat gets a session (SessionStart), in parallel.
    let sock = h.sock.clone();
    let start = fixture("claude", "SessionStart").to_string();
    tokio::task::spawn_blocking(move || {
        std::thread::scope(|s| {
            for t in 0..8 {
                let (sock, start) = (&sock, &start);
                s.spawn(move || {
                    for i in (t..SEATS).step_by(8) {
                        let seat = format!("s{i:04}");
                        let cc = format!("/tmp/amail-hotpath-{i}.sock");
                        let env = [
                            ("AMAIL_SEAT", seat.as_str()),
                            ("CLAUDE_CODE_MESSAGING_SOCKET", cc.as_str()),
                        ];
                        let (_, _, ok) = run_hook(sock, "claude", &env, start, Some(10_000));
                        assert!(ok);
                    }
                });
            }
        })
    })
    .await
    .unwrap();
    h.wait("1000 sessions", || async {
        h.status().await["seats"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["state"] == "idle")
            .count()
            == SEATS
    })
    .await;

    // Baseline: starting the binary at all.
    let mut base = Vec::new();
    for _ in 0..200 {
        let t0 = std::time::Instant::now();
        let ok = std::process::Command::new(BIN)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok);
        base.push(t0.elapsed());
    }
    base.sort();

    // The hot path: PostToolUse for seats with nothing pending.
    let sock = h.sock.clone();
    let input = fixture("claude", "PostToolUse").to_string();
    let mut lat = tokio::task::spawn_blocking(move || {
        let mut lat = Vec::with_capacity(SAMPLES);
        for n in 0..SAMPLES {
            let seat = format!("s{:04}", (n * 7919) % SEATS);
            let (out, dt, ok) = run_hook(&sock, "claude", &[("AMAIL_SEAT", &seat)], &input, None);
            assert!(ok && out.is_empty(), "{out}");
            lat.push(dt);
        }
        lat
    })
    .await
    .unwrap();
    lat.sort();
    let p50 = pct(&lat, 0.50);
    let p99 = pct(&lat, 0.99);
    eprintln!(
        "hook hot path, {SEATS} seats, {SAMPLES} samples ({} build): p50 {:.2} ms, p90 {:.2} ms, \
         p99 {:.2} ms, max {:.2} ms; `amail --version` p50 {:.2} ms, p99 {:.2} ms",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        p50.as_secs_f64() * 1e3,
        pct(&lat, 0.90).as_secs_f64() * 1e3,
        p99.as_secs_f64() * 1e3,
        lat.last().unwrap().as_secs_f64() * 1e3,
        pct(&base, 0.50).as_secs_f64() * 1e3,
        pct(&base, 0.99).as_secs_f64() * 1e3,
    );
    // Every seat is still in its state; nothing went to the server.
    assert_eq!(h.status().await["connected"], true);
    if !cfg!(debug_assertions) {
        assert!(p99 < Duration::from_millis(10), "p99 {p99:?}");
    }
}
