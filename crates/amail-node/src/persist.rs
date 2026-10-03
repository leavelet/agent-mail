//! The node's durable state: Codex session registrations, launch
//! reservations, and the seat tokens handed out at launch.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Saved {
    /// Harness session id -> registration.
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionReg>,
    /// Pending launches waiting for their first prompt, by nonce.
    #[serde(default)]
    pub reservations: BTreeMap<String, Reservation>,
    /// Seat -> the token issued at its last launch.
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
    #[serde(default)]
    pub wake_counter: u64,
    /// Seat -> wake-ups sent (shown by `amail node status`).
    #[serde(default)]
    pub wakes: BTreeMap<String, u64>,
    /// Seat -> its live session, restored on restart if still running.
    #[serde(default)]
    pub live: BTreeMap<String, LiveSession>,
    /// Seat -> claimed mail whose injection did not reach the agent.
    #[serde(default)]
    pub stash: BTreeMap<String, Vec<amail_proto::InboxEntry>>,
    /// Seats this node supervises (`amail up`).
    #[serde(default)]
    pub managed: BTreeMap<String, Managed>,
    /// Lifecycle settings of the supervised team.
    #[serde(default)]
    pub settings: crate::team::Settings,
}

/// A supervised seat: where it runs and what the supervisor knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Managed {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub harness: String,
    pub cwd: String,
    pub mux_session: String,
    pub tab: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<u32>,
    /// Wanted running (`up`); false after `down` or a clean exit.
    pub up: bool,
    /// Asleep: no process; mail for the seat starts it again.
    #[serde(default)]
    pub dormant: bool,
    /// Harness session to resume at the next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// The seat had a session that is gone: the next fresh start says so.
    #[serde(default)]
    pub context_lost: bool,
    /// Consecutive failed runs.
    #[serde(default)]
    pub failures: u32,
    /// Gave up restarting.
    #[serde(default)]
    pub failed: bool,
    /// The running harness process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<u64>,
    /// Position in the team file (parents first): start order.
    #[serde(default)]
    pub order: u32,
    /// How the last run ended, for `amail ps`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSession {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReg {
    pub seat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub seat: String,
    pub pid: u32,
    /// Unix milliseconds.
    pub expires: i64,
}

/// Write `data` to `path` through a temporary file and rename.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all().ok();
    }
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

pub fn load(path: &Path) -> Saved {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable state file {}: {e}", path.display());
            Saved::default()
        }),
        Err(_) => Saved::default(),
    }
}

pub fn save(path: &Path, s: &Saved) -> Result<()> {
    write_atomic(path, &serde_json::to_vec_pretty(s)?, 0o600)
}
