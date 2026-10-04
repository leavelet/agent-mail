//! Who sent a request on the node socket.
//!
//! The node takes the process id of the connecting process from the socket
//! (`SO_PEERCRED`) and walks its parent processes in `/proc`:
//!
//! - A process the node launched for a seat (`amail run`, which becomes the
//!   harness; see [`crate::persist::Saved::roots`]) ties the caller to that
//!   seat. Claude Code runs its hooks and tool commands as descendants of
//!   that process.
//! - Codex runs hooks and tool commands in its shared app-server, not under
//!   the session's process. Those callers are recognized by the node's
//!   Codex home in the environment of the caller or a parent, and their
//!   requests must carry the node's Codex pass
//!   ([`crate::wire::CODEX_PASS_ENV`]).
//!
//! Requests that tie a caller to a seat (hooks, credentials) are served only
//! to that seat's session; requests for the person (launch, status,
//! supervision) are refused to sessions.

use super::*;

/// Parent processes followed at most.
const MAX_DEPTH: usize = 64;

/// What the node knows about a connecting process.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub pid: u32,
    /// The nearest session process the node launched among the caller and
    /// its parents: `(seat, harness)`.
    pub session: Option<(String, Harness)>,
    /// The caller or a parent runs with the node's Codex home: a Codex
    /// session, its app-server, or a command run by it. (Only looked up for
    /// requests meant for the person.)
    pub codex: bool,
}

impl Caller {
    /// Part of a session the node launched (agents run here).
    pub fn is_session(&self) -> bool {
        self.session.is_some() || self.codex
    }

    /// Part of a Claude Code session of `seat`.
    pub fn is_claude_seat(&self, seat: &str) -> bool {
        matches!(&self.session, Some((s, Harness::Claude)) if s == seat)
    }

    /// Part of a Claude Code session (of any seat).
    pub fn is_claude(&self) -> bool {
        matches!(&self.session, Some((_, Harness::Claude)))
    }
}

/// `(parent pid, start time)` of `pid` from `/proc/<pid>/stat`.
fn stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesized command name: state (3), ppid (4), ...,
    // start time (22).
    let rest = stat.get(stat.rfind(')')? + 2..)?;
    let f: Vec<&str> = rest.split(' ').collect();
    let ppid = f.get(1)?.parse().ok()?;
    let start = f.get(19)?.parse().ok()?;
    Some((ppid, start))
}

/// `pid` and its parents, nearest first, with their start times.
pub fn ancestry(pid: u32) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    let mut p = pid;
    while p > 1 && out.len() < MAX_DEPTH {
        let Some((ppid, start)) = stat(p) else { break };
        out.push((p, start));
        p = ppid;
    }
    out
}

/// Whether the environment `pid` started with contains `entry` exactly.
fn environ_has(pid: u32, entry: &[u8]) -> bool {
    std::fs::read(format!("/proc/{pid}/environ"))
        .map(|env| env.split(|b| *b == 0).any(|kv| kv == entry))
        .unwrap_or(false)
}

/// The value of `name` in the environment `pid` started with.
pub fn environ_var(pid: u32, name: &str) -> Option<String> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    let key = format!("{name}=");
    env.split(|b| *b == 0)
        .find_map(|kv| kv.strip_prefix(key.as_bytes()))
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

/// Compare two secrets in time that depends only on their lengths.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

impl Node {
    /// Identify the process `pid` (from `SO_PEERCRED`). The environments of
    /// its parents are read only with `codex` (hooks, on the hot path, do
    /// not need them).
    pub(super) fn caller(&self, pid: u32, codex: bool) -> Caller {
        let chain = ancestry(pid);
        let session = {
            let st = self.lock();
            chain.iter().find_map(|(p, start)| {
                st.saved.roots.iter().find_map(|(seat, r)| {
                    (r.pid == *p && r.pid_start.is_none_or(|s| s == *start))
                        .then(|| Some((seat.clone(), Harness::parse(&r.harness)?)))
                        .flatten()
                })
            })
        };
        let entry = format!(
            "CODEX_HOME={}",
            crate::adapter::codex::home_in(&self.cfg.state_dir).display()
        );
        let codex = codex && chain.iter().any(|(p, _)| environ_has(*p, entry.as_bytes()));
        Caller {
            pid,
            session,
            codex,
        }
    }

    /// Whether `pass` is the node's Codex pass.
    pub(super) fn codex_pass_ok(&self, pass: Option<&str>) -> bool {
        let st = self.lock();
        match (pass, st.saved.codex_pass.as_deref()) {
            (Some(a), Some(b)) => same_secret(a, b),
            _ => false,
        }
    }

    /// Whether a hook request from `caller` may act for its seat. Claude
    /// Code hooks must come from the session of the seat they name; Codex
    /// hooks must carry the Codex pass (and not come from a Claude Code
    /// session).
    pub(super) fn hook_allowed(
        &self,
        caller: &Caller,
        harness: &str,
        env: &BTreeMap<String, String>,
    ) -> bool {
        match Harness::parse(harness) {
            Some(Harness::Claude) => env
                .get("AMAIL_SEAT")
                .is_some_and(|seat| caller.is_claude_seat(seat)),
            Some(Harness::Codex) => {
                !caller.is_claude()
                    && self.codex_pass_ok(env.get(crate::wire::CODEX_PASS_ENV).map(String::as_str))
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_ancestry() {
        let me = std::process::id();
        let chain = super::ancestry(me);
        assert_eq!(chain.first().map(|c| c.0), Some(me));
        assert_eq!(
            chain.get(1).map(|c| c.0),
            Some(std::os::unix::process::parent_id())
        );
        assert_eq!(chain[0].1, super::super::proc_start(me).unwrap());
    }

    #[test]
    fn secrets() {
        assert!(super::same_secret("abc", "abc"));
        assert!(!super::same_secret("abc", "abd"));
        assert!(!super::same_secret("abc", "ab"));
    }
}
