//! `amail hook <harness>`: the thin client a harness runs for every hook.
//!
//! It forwards stdin and a few environment variables to the node and prints
//! the node's answer. It never fails: if the node is not running, is slow
//! (default wait 2 s, `AMAIL_HOOK_TIMEOUT_MS`), or answers something
//! unexpected, it prints nothing and exits 0, so a hook can never block or
//! break the agent. All decisions are made in the node.
//!
//! Plain blocking std I/O, no async runtime: this runs on the hook hot path.
//!
//! Timeouts, from inner to outer, each clearly below the next:
//! - the node's server calls for one hook: the node's hook budget, from
//!   150 ms ([`crate::server::HOOK_SERVER_TIMEOUT`]) up to 1.5 s by default,
//!   following the latency the node measures (`Node::hook_budget`); also
//!   capped by our remaining budget;
//! - this client's wait for the node: 2 s ([`DEFAULT_TIMEOUT`]); it sends
//!   the node its remaining budget, and the node keeps (rather than sends) an
//!   answer it cannot deliver in time;
//! - the harness's own hook timeout: 5 s (3 s for Codex `SessionEnd` and
//!   `Interrupt`), set in the generated hook configuration.

use crate::wire::{HOOK_ENV, HookResponse, Request};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const TIMEOUT_ENV: &str = "AMAIL_HOOK_TIMEOUT_MS";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2000);
/// Hook inputs larger than this are not forwarded.
const MAX_INPUT: u64 = 32 << 20;

mod codex_context;

/// Non-secret locations supplied by the node's managed Codex hook command.
#[derive(Clone)]
pub struct NodeContext {
    pub state_dir: PathBuf,
    pub socket: PathBuf,
}

/// Run the hook client; returns what to print (possibly empty). Errors are
/// reported only when `AMAIL_HOOK_DEBUG` is set, on stderr.
pub fn run(harness: &str) -> String {
    run_with_context(harness, None)
}

/// The same hook protocol with a guarded context source for Codex backends
/// that were started before the node supplied its environment.
pub fn run_with_context(harness: &str, context: Option<&NodeContext>) -> String {
    let start = Instant::now();
    let timeout = std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT);
    let deadline = start + timeout;
    match exchange(harness, context, deadline) {
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

fn exchange(
    harness: &str,
    context: Option<&NodeContext>,
    deadline: Instant,
) -> Result<String, String> {
    if context.is_some() && harness != "codex" {
        return Err("node hook context is only supported for codex".into());
    }
    let context = context.cloned();
    let mut env: BTreeMap<String, String> = HOOK_ENV
        .iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
        .collect();
    let socket = crate::paths::socket();
    let parent = std::os::unix::process::parent_id();
    // Read stdin on a helper thread so a harness that never closes it cannot
    // hold us past the deadline. Context validation and private-file I/O share
    // that same deadline; no process environment is mutated.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| -> Result<_, String> {
            let mut buf = Vec::new();
            std::io::stdin()
                .lock()
                .take(MAX_INPUT)
                .read_to_end(&mut buf)
                .map_err(|e| format!("reading stdin: {e}"))?;
            let socket = if let Some(context) = context {
                codex_context::restore(&context, &mut env, parent)?;
                context.socket
            } else {
                socket
            };
            Ok((buf, socket, env))
        })();
        let _ = tx.send(r);
    });
    let (input, socket, env) = rx
        .recv_timeout(remaining(deadline)?)
        .map_err(|_| "timed out preparing hook input".to_string())??;
    let req = Request::Hook {
        harness: harness.to_string(),
        env,
        input: String::from_utf8_lossy(&input).into_owned(),
        budget_ms: Some(remaining(deadline)?.as_millis() as u64),
    };
    let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    line.push(b'\n');

    let mut sock = crate::paths::connect_node(&socket).map_err(|e| format!("connect: {e}"))?;
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
