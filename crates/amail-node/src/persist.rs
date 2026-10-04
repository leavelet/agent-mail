//! The node's durable state: Codex session registrations, launch
//! reservations, the seat tokens handed out at launch, and the session
//! processes the node launched.

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
    /// Seat -> the transcript file of its most recent session (for the
    /// conversation mirror; kept while the seat sleeps).
    #[serde(default)]
    pub transcripts: BTreeMap<String, TranscriptRef>,
    /// Claims whose answer did not arrive, to be retried with the same key
    /// (the server then returns what they took), by key.
    #[serde(default)]
    pub claim_retries: BTreeMap<String, ClaimRetry>,
    /// Seat -> the process of its most recently launched session (the
    /// `amail run` process, which becomes the harness). Requests on the node
    /// socket are tied to a seat through it.
    #[serde(default)]
    pub roots: BTreeMap<String, SessionRoot>,
    /// The value of `AMAIL_CODEX_PASS` that the node's Codex sessions (their
    /// shared app-server, its hooks and tool commands) hold; see
    /// [`crate::wire::CODEX_PASS_ENV`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_pass: Option<String>,
    /// The settings the server last accepted a report of (see
    /// [`crate::node`]'s settings notices).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported: Option<Reported>,
}

/// Settings of a node as reported to the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reported {
    pub local_policy: String,
    /// Supervised seat -> its settings.
    #[serde(default)]
    pub seats: BTreeMap<String, ReportedSeat>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedSeat {
    pub permission_mode: String,
    /// Codex seats only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_access: Option<String>,
}

/// A session process the node launched (see [`Saved::roots`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRoot {
    pub harness: String,
    pub pid: u32,
    /// The process start time, which guards against pid reuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid_start: Option<u64>,
}

/// A claim to retry (see [`Saved::claim_retries`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRetry {
    pub seat: String,
    pub spec: crate::server::ClaimSpec,
    /// Unix ms of the first attempt.
    pub since: i64,
}

/// Where a seat's conversation is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptRef {
    pub harness: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The harness's transcript directory the path was accepted under
    /// (checked again when the file is opened).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
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
    /// Type the wake-up into the pane when native wake-ups fail (guarded).
    #[serde(default)]
    pub paste_fallback: bool,
    /// The permission mode from the team file (see `SeatPlacement`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<amail_proto::PermissionMode>,
    /// The git access from the team file (see `SeatPlacement`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_access: Option<crate::wire::GitAccess>,
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
    // Unique per call: concurrent writers must not share a temporary file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}.{n}", std::process::id()));
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
