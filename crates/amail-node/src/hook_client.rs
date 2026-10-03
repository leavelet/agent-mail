//! `amail hook <harness>`: the thin client a harness runs for every hook.
//!
//! It forwards stdin and a few environment variables to the node and prints
//! the node's answer. It never fails: if the node is not running, is slow
//! (default budget 300 ms, `AMAIL_HOOK_TIMEOUT_MS`), or answers something
//! unexpected, it prints nothing and exits 0, so a hook can never block or
//! break the agent. All decisions are made in the node.
//!
//! Plain blocking std I/O, no async runtime: this runs on the hook hot path.
//!
//! Timeouts, from inner to outer, each clearly below the next:
//! - the node's server calls for one hook: 150 ms in total
//!   ([`crate::server::HOOK_SERVER_TIMEOUT`], also capped by our budget);
//! - this client's wait for the node: 300 ms ([`DEFAULT_TIMEOUT`]); it sends
//!   the node its remaining budget, and the node keeps (rather than sends) an
//!   answer it cannot deliver in time;
//! - the harness's own hook timeout: 5 s (3 s for Codex `SessionEnd` and
//!   `Interrupt`), set in the generated hook configuration.

use crate::wire::{HOOK_ENV, HookResponse, Request};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const TIMEOUT_ENV: &str = "AMAIL_HOOK_TIMEOUT_MS";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(300);
/// Hook inputs larger than this are not forwarded.
const MAX_INPUT: u64 = 32 << 20;

/// Run the hook client; returns what to print (possibly empty). Errors are
/// reported only when `AMAIL_HOOK_DEBUG` is set, on stderr.
pub fn run(harness: &str) -> String {
    let start = Instant::now();
    let timeout = std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let deadline = start + timeout;
    match exchange(harness, &crate::paths::socket(), deadline) {
        Ok(out) => out,
        Err(e) => {
            if std::env::var_os("AMAIL_HOOK_DEBUG").is_some() {
                eprintln!("amail hook: {e}");
            }
            String::new()
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration, String> {
    let now = Instant::now();
    if now >= deadline {
        return Err("timed out".into());
    }
    Ok(deadline - now)
}

fn exchange(harness: &str, socket: &Path, deadline: Instant) -> Result<String, String> {
    // Read stdin on a helper thread so a harness that never closes it cannot
    // hold us past the deadline.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let r = std::io::stdin()
            .lock()
            .take(MAX_INPUT)
            .read_to_end(&mut buf)
            .map(|_| buf);
        let _ = tx.send(r);
    });
    let input = rx
        .recv_timeout(remaining(deadline)?)
        .map_err(|_| "timed out reading stdin".to_string())?
        .map_err(|e| format!("reading stdin: {e}"))?;

    let env: BTreeMap<String, String> = HOOK_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    let req = Request::Hook {
        harness: harness.to_string(),
        env,
        input: String::from_utf8_lossy(&input).into_owned(),
        budget_ms: Some(remaining(deadline)?.as_millis() as u64),
    };
    let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    line.push(b'\n');

    let mut sock = UnixStream::connect(socket).map_err(|e| format!("connect: {e}"))?;
    sock.set_write_timeout(Some(remaining(deadline)?))
        .map_err(|e| e.to_string())?;
    sock.write_all(&line).map_err(|e| format!("write: {e}"))?;
    sock.set_read_timeout(Some(remaining(deadline)?))
        .map_err(|e| e.to_string())?;
    let mut resp = String::new();
    BufReader::new(sock)
        .read_line(&mut resp)
        .map_err(|e| format!("read: {e}"))?;
    remaining(deadline)?;
    let r: HookResponse = serde_json::from_str(&resp).map_err(|e| format!("bad response: {e}"))?;
    Ok(r.stdout)
}
