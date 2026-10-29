//! The node daemon: one per host.
//!
//! - Serves the node socket (hooks, launches, credentials, status).
//! - Mirrors, per seat, whether pending (unnotified) deliveries exist, from
//!   the server's event stream plus `GET /v1/pending`. A hook for a seat with
//!   nothing pending is answered from memory, without a server round trip.
//! - Runs each seat's session state machine (offline / busy / idle /
//!   awaiting) from hook events and process liveness, and reports changes to
//!   the server (`POST /v1/presence`).
//! - Wakes idle seats that have pending `to` or urgent mail. A wake-up only
//!   nudges the session; the hook it triggers (`UserPromptSubmit`) claims and
//!   shows the mail, so mail reaches the model through one path only, and
//!   the server's atomic claim guarantees each delivery is shown once.
//! - Keeps claimed mail whose hook answer could not be delivered and shows it
//!   with the seat's next injecting hook.
//!
//! When the server is unreachable every hook gets an empty answer; local
//! state is kept and re-synchronized on reconnect. Live sessions, Codex
//! registrations and undelivered mail are persisted in the state directory.
//!
//! What the socket serves depends on the local policy (see [`notices`]).
//! Under `restricted`, requests are checked against the process that sent
//! them (see [`caller`]): a seat's hooks and credentials are served only to
//! that seat's session, and requests for the person only to processes
//! outside the sessions the node launched. Under `open`, every process of
//! the user is served, and the requests of sessions that manage seats or
//! obtain other seats' credentials are reported to the person.

use crate::adapter::{Adapter, AdapterEnv, Event, Harness, Hook, LaunchCtx, Output};
use crate::persist::{self, LiveSession, Reservation, Saved, SessionReg};
use crate::server::{ClaimSpec, ServerClient};
use crate::wire::{Credentials, GitAccess, LaunchPlan, Request};
use amail_proto::{
    DeliveryClass, InboxEntry, MailboxEvent, PendingCounts, PermissionMode, PresenceReport,
    PresenceUpdate, Priority, SeatState,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, OnceCell};

mod adopt;
mod caller;
mod handover;
mod mirror;
mod notices;
mod requests;
mod supervise;
mod turns;

/// Node configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub url: String,
    pub token: String,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
    /// Shown in presence reports.
    pub label: Option<String>,
    pub amail_exe: PathBuf,
    pub claude_bin: String,
    pub codex_bin: String,
    /// Explicit native Codex policy; otherwise use amail's workspace/git profiles.
    pub codex_sandbox: Option<crate::adapter::codex::SandboxMode>,
    /// The `dsh` launcher (DeepSeek Harness).
    pub dsh_bin: String,
    /// Environment given only to dsh hosts (e.g. the model provider's key).
    pub dsh_env: BTreeMap<String, String>,
    pub codex_auth: PathBuf,
    /// Coalescing window for wake-ups caused by normal `to` mail.
    pub wake_window: Duration,
    /// How long to wait for the `UserPromptSubmit` that confirms a wake-up.
    pub wake_confirm: Duration,
    /// After a wake-up went unanswered (twice): first retry delay, doubling
    /// up to `wake_backoff_max`. New mail for the seat retries at once.
    pub wake_backoff: Duration,
    pub wake_backoff_max: Duration,
    /// Consecutive `Stop` blocks per turn before leaving mail to the waker.
    pub stop_block_cap: u32,
    /// Failed turns in a row after which the node stops delivering the
    /// mail of failed turns again by waking the seat (see [`turns`]).
    pub turn_fail_limit: u32,
    /// Wait before waking a seat whose turn failed; doubles per failure
    /// (up to `wake_backoff_max`).
    pub turn_retry_backoff: Duration,
    /// For harnesses whose hooks report no failed turns (Codex): how soon
    /// after a turn's end the node asks how it ended.
    pub turn_check_delay: Duration,
    /// A busy seat without hooks this long has its turn looked at (and
    /// again at this interval while it stays quiet): Codex seats by asking
    /// the app-server, Claude Code seats by reading the end of their
    /// transcript (see [`turns`]).
    pub turn_quiet: Duration,
    /// A busy supervised seat without hooks this long whose transcript says
    /// nothing has its pane looked at (Claude Code: is a turn running?).
    pub screen_check_after: Duration,
    pub heartbeat: Duration,
    pub liveness_interval: Duration,
    /// The least total time of the server calls one hook waits for
    /// (claims, inbox). The budget grows with the latency the node measures,
    /// up to `hook_budget_max` (see `Node::hook_budget`); both stay well
    /// below the hook client's wait (2 s), which is well below the
    /// harness's hook timeout (3-5 s); see [`crate::hook_client`].
    pub hook_server_timeout: Duration,
    /// The most a hook waits for the server, however slow it measures.
    pub hook_budget_max: Duration,
    /// The zellij binary, and the size of the headless client that keeps
    /// panes wide while nobody is attached (see [`crate::mux`]).
    pub zellij_bin: String,
    pub pane_cols: u16,
    pub pane_rows: u16,
    /// Most sessions starting at once (from launch to their first hook).
    pub start_limit: usize,
    /// A session without a hook this long after launch is checked for a
    /// prompt that needs a person (and stops holding a start slot).
    pub start_timeout: Duration,
    /// How often a starting session's screen is checked for prompts that
    /// need a person (less often once `start_timeout` has passed).
    pub probe_interval: Duration,
    /// SIGTERM, then SIGKILL after this long.
    pub kill_grace: Duration,
    /// First restart delay after a crash; doubles per consecutive crash.
    pub restart_backoff: Duration,
    /// Restarts after consecutive crashes before giving up.
    pub max_restarts: u32,
    /// A run that lasted this long resets the crash count.
    pub stable_after: Duration,
    /// A resumed session that dies this soon without a hook can fall back
    /// to a new session only with an explicit missing-session diagnostic.
    pub resume_grace: Duration,
    pub supervise_interval: Duration,
    /// How often a mirrored transcript file is checked for new lines.
    pub mirror_poll: Duration,
    /// Serve the supervised sessions in the browser (`zellij web`).
    pub web_terminal: Option<WebTerminal>,
    /// Use this digest interval instead of the org's (for tests).
    pub digest_every: Option<Duration>,
    /// Check who sends each socket request (see [`caller`]). Only tests
    /// that drive the socket from processes the node did not launch turn
    /// this off.
    pub check_callers: bool,
    /// Which processes may use the node socket (see
    /// [`amail_proto::LocalPolicy`]); `None`: the team file's, else
    /// [`amail_proto::LocalPolicy::DEFAULT`]. A team file can choose
    /// `restricted` but cannot loosen the node's setting.
    pub local_policy: Option<amail_proto::LocalPolicy>,
    /// Report setting changes and seat management by agent sessions to the
    /// server, which tells the person by system mail.
    pub notices: bool,
    /// At most one notice about one session's requests per this long.
    pub notice_window: Duration,
    /// How often due notices are sent.
    pub notice_tick: Duration,
    /// Claude Code's configuration directory as the node sees it
    /// (`$CLAUDE_CONFIG_DIR`, else `~/.claude`). Claude Code transcripts are
    /// followed only under its `projects` directory, or under that of the
    /// directory the session was started with.
    pub claude_config_dir: PathBuf,
    /// Ask for the handover note at the end of a turn whose input passed
    /// this percentage of the model's context window.
    pub handover_threshold: f64,
    /// Context windows by model (`--context-window`).
    pub context_windows: crate::transcript::usage::ContextWindows,
    /// How often queued worklog entries are sent.
    pub worklog_every: Duration,
    /// How often idle seats are looked at for an old handover note.
    pub handover_scan: Duration,
    /// The document index is fetched again at the start of a turn at most
    /// this often per seat.
    pub index_recheck: Duration,
}

/// The node's browser terminal: a `zellij web` server of its own (on its
/// own port, with its login tokens in the node's state directory).
#[derive(Debug, Clone)]
pub struct WebTerminal {
    pub port: u16,
    /// Base URL people open (default `http://127.0.0.1:<port>`); a seat's
    /// terminal is `<base>/<zellij session>`.
    pub base_url: String,
}

impl Config {
    /// Defaults for everything but the server URL and token.
    pub fn new(url: &str, token: &str) -> Config {
        Config {
            url: url.to_string(),
            token: token.to_string(),
            state_dir: crate::paths::state_dir(),
            socket: crate::paths::socket(),
            label: None,
            amail_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("amail")),
            claude_bin: "claude".into(),
            codex_bin: "codex".into(),
            codex_sandbox: None,
            dsh_bin: "dsh".into(),
            dsh_env: BTreeMap::new(),
            codex_auth: crate::paths::home().join(".codex").join("auth.json"),
            wake_window: Duration::from_secs(3),
            wake_confirm: Duration::from_secs(10),
            wake_backoff: Duration::from_secs(60),
            wake_backoff_max: Duration::from_secs(600),
            stop_block_cap: 3,
            turn_fail_limit: 3,
            turn_retry_backoff: Duration::from_secs(30),
            turn_check_delay: Duration::from_secs(1),
            turn_quiet: Duration::from_secs(3),
            screen_check_after: Duration::from_secs(60),
            heartbeat: Duration::from_secs(30),
            liveness_interval: Duration::from_secs(2),
            hook_server_timeout: crate::server::HOOK_SERVER_TIMEOUT,
            hook_budget_max: Duration::from_millis(1500),
            zellij_bin: "zellij".into(),
            pane_cols: 200,
            pane_rows: 60,
            start_limit: 4,
            start_timeout: Duration::from_secs(60),
            probe_interval: Duration::from_secs(3),
            kill_grace: Duration::from_secs(10),
            restart_backoff: Duration::from_secs(2),
            max_restarts: 3,
            stable_after: Duration::from_secs(600),
            resume_grace: Duration::from_secs(30),
            supervise_interval: Duration::from_millis(500),
            mirror_poll: Duration::from_millis(500),
            web_terminal: None,
            digest_every: None,
            check_callers: true,
            local_policy: None,
            notices: true,
            notice_window: Duration::from_secs(300),
            notice_tick: Duration::from_secs(2),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| crate::paths::home().join(".claude")),
            handover_threshold: 70.0,
            context_windows: Default::default(),
            worklog_every: Duration::from_secs(3),
            handover_scan: Duration::from_secs(60),
            index_recheck: Duration::from_secs(30),
        }
    }
}

const RESERVATION_TTL_MS: i64 = 15 * 60 * 1000;
const SUMMARY_LIMIT: u32 = 20;
/// Open tasks counted at session start (more show as `200+`).
const TASKS_LIMIT: u32 = 200;
/// Digest mail one hook takes at most (shown one line per thread).
const DIGEST_LIMIT: u32 = 100;
/// An answer that would reach the hook client less than this before its
/// deadline is kept for the next hook instead of being sent.
const LATE_MARGIN: Duration = Duration::from_millis(30);

/// Hooks that asked the server for mail are counted over this window, with
/// whether they got their answer in time; see `Node::hook_budget`.
const HOOK_WINDOW: Duration = Duration::from_secs(600);
/// A slow-server notice needs this many misses in the window (and at least
/// 30% of the hooks), and is repeated at most once per `SLOW_NOTICE_EVERY`.
const SLOW_NOTICE_MISSES: usize = 5;
const SLOW_NOTICE_EVERY: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Default)]
struct SessionRt {
    id: Option<String>,
    /// `(pid, start time)`; the start time guards against pid reuse.
    pid: Option<(u32, Option<u64>)>,
    target: Option<String>,
}

#[derive(Debug, Clone)]
struct SeatRt {
    harness: Option<Harness>,
    state: SeatState,
    since: i64,
    session: Option<SessionRt>,
    pending: PendingCounts,
    stop_blocks: u32,
    wake_due: Option<Instant>,
    /// `(sent at, attempt)` of an unconfirmed wake-up.
    wake_out: Option<(Instant, u8)>,
    wake_failed: bool,
    /// When a failed seat is tried again, and the current backoff.
    wake_retry_at: Option<Instant>,
    wake_backoff: Duration,
    wakes: u64,
    /// Reported to the server as present at least once.
    reported: bool,
    /// Claimed mail whose injection never reached the agent; shown by the
    /// next injecting hook.
    stash: Vec<InboxEntry>,
    /// Messages the current session was shown, as new mail or in an unread
    /// summary. The summary a turn starts with leaves them out: mail that
    /// stays unread (a long body the agent did not `amail read`) would
    /// otherwise be listed again at every turn. Cleared at session start.
    listed: HashSet<i64>,
    /// Last hook or launch (Unix ms).
    last_activity: i64,
    /// What the server was last told about the seat needing a person.
    reported_attention: Option<(String, String)>,
    /// When the oldest pending digest mail was sent (Unix ms).
    digest_since: Option<i64>,
    /// Focus mode (only urgent mail) until then (Unix ms).
    focus_until: Option<i64>,
    /// Last wake-up for digest mail alone (Unix ms).
    last_digest_wake: i64,
    /// The last turn failed: no wake-up before then (see [`turns`]).
    turn_retry_at: Option<Instant>,
    /// A turn whose end is being checked with the harness.
    turn_check: Option<turns::TurnCheck>,
    /// When a quiet busy seat's turn was last checked.
    turn_quiet_checked: Option<Instant>,
    /// What the last checks of a quiet busy seat found (see [`turns`]).
    quiet: turns::QuietRt,
    /// A surviving Codex session restored without a current turn result.
    /// It cannot receive automatic mail wakes until positively reconciled.
    restore_pending: bool,
    /// Documents and handover (see [`handover`]).
    ho: handover::SeatHo,
    /// Requests for the person (see [`requests`]).
    rq: requests::SeatRq,
}

impl SeatRt {
    fn new(name: &str) -> SeatRt {
        SeatRt {
            harness: None,
            state: SeatState::Offline,
            since: now_ms(),
            session: None,
            pending: PendingCounts {
                seat: name.to_string(),
                ..Default::default()
            },
            stop_blocks: 0,
            wake_due: None,
            wake_out: None,
            wake_failed: false,
            wake_retry_at: None,
            wake_backoff: Duration::ZERO,
            wakes: 0,
            reported: false,
            stash: Vec::new(),
            listed: HashSet::new(),
            last_activity: 0,
            reported_attention: None,
            digest_since: None,
            focus_until: None,
            last_digest_wake: 0,
            turn_retry_at: None,
            turn_check: None,
            turn_quiet_checked: None,
            quiet: Default::default(),
            restore_pending: false,
            ho: Default::default(),
            rq: Default::default(),
        }
    }

    fn focused(&self, now: i64) -> bool {
        self.focus_until.is_some_and(|t| t > now)
    }

    /// Digest mail has waited a full interval, and the seat was not woken
    /// for a digest within the last one.
    fn digest_due(&self, now: i64, every: i64) -> bool {
        !self.focused(now)
            && self.pending.digest > 0
            && self.digest_since.is_some_and(|t| now - t >= every)
            && now - self.last_digest_wake >= every
    }

    /// Whether the seat should be woken (or started): `to` or urgent mail,
    /// mail kept from a lost answer, or a due digest. In focus, only urgent
    /// mail (and kept mail) counts.
    fn wants_wake(&self, now: i64, digest_every: i64) -> bool {
        if self.restore_pending {
            return false;
        }
        if !self.stash.is_empty() || self.pending.urgent > 0 {
            return true;
        }
        if self.focused(now) {
            return false;
        }
        self.pending.to > 0 || self.digest_due(now, digest_every)
    }

    fn clear_wake_failure(&mut self) {
        self.wake_failed = false;
        self.wake_retry_at = None;
        self.wake_backoff = Duration::ZERO;
    }
}

struct State {
    connected: bool,
    seats: HashMap<String, SeatRt>,
    saved: Saved,
    dirty_pending: HashSet<String>,
    /// Seats whose pending counts the server refused to give this node (the
    /// user seat: node tokens read agent seats' mailboxes only). Their
    /// mailbox events are ignored until the node connects again.
    pending_refused: HashSet<String>,
    dirty_presence: HashSet<String>,
    /// `saved` changed and must be written.
    save_dirty: bool,
    /// Supervisor state of supervised seats (not persisted).
    sup: HashMap<String, supervise::SupRt>,
    /// Supervised seats waiting for a start slot, in order.
    start_queue: std::collections::VecDeque<String>,
    /// Transcripts being forwarded to the server, by seat.
    mirrors: HashMap<String, mirror::MirrorRt>,
    /// The org's digest interval (ms), from the server.
    digest_every: i64,
    /// Requests of agent sessions to report, by session (seat name; empty
    /// for Codex).
    notices: BTreeMap<String, notices::Pending>,
    /// The settings may differ from those last reported.
    settings_due: bool,
    /// The state file existed when the node started (an earlier version
    /// may have run, which always used the restricted policy).
    prior_state: bool,
    /// Notices about failing turns, not sent yet.
    turn_notices: Vec<amail_proto::NodeNotice>,
    /// [`Config::turn_fail_limit`].
    turn_fail_limit: u32,
    /// Hooks that asked the server for mail in the last [`HOOK_WINDOW`]:
    /// when, and whether the answer came too late for the hook.
    hooks: VecDeque<(Instant, bool)>,
    /// When the user was last told that hooks miss the server.
    slow_notice_at: Option<Instant>,
    /// Worklog entries not sent yet (see [`handover`]).
    worklog: VecDeque<amail_proto::WorklogPostEntry>,
    /// Entries dropped since the last warning.
    worklog_dropped: u64,
    /// Work off the hook path (see [`handover::Job`]).
    jobs: VecDeque<handover::Job>,
}

impl Default for State {
    fn default() -> Self {
        State {
            connected: false,
            seats: HashMap::new(),
            saved: Saved::default(),
            dirty_pending: HashSet::new(),
            pending_refused: HashSet::new(),
            dirty_presence: HashSet::new(),
            save_dirty: false,
            sup: HashMap::new(),
            start_queue: Default::default(),
            mirrors: HashMap::new(),
            digest_every: DIGEST_EVERY_DEFAULT,
            notices: BTreeMap::new(),
            settings_due: true,
            prior_state: false,
            turn_notices: Vec::new(),
            turn_fail_limit: 3,
            hooks: VecDeque::new(),
            slow_notice_at: None,
            worklog: VecDeque::new(),
            worklog_dropped: 0,
            jobs: VecDeque::new(),
        }
    }
}

impl State {
    /// A hook asked the server for mail; `missed`: its answer came too
    /// late for the hook (the mail is kept for the next one).
    fn note_hook(&mut self, now: Instant, missed: bool) {
        self.hooks.push_back((now, missed));
        while self
            .hooks
            .front()
            .is_some_and(|(t, _)| now.duration_since(*t) > HOOK_WINDOW)
        {
            self.hooks.pop_front();
        }
    }

    /// Hooks in the window that asked the server, and how many missed.
    fn hook_stats(&self, now: Instant) -> (usize, usize) {
        let recent = self
            .hooks
            .iter()
            .filter(|(t, _)| now.duration_since(*t) <= HOOK_WINDOW);
        let hooks = recent.clone().count();
        let misses = recent.filter(|(_, m)| *m).count();
        (hooks, misses)
    }
}

/// Digest interval until the server says otherwise.
const DIGEST_EVERY_DEFAULT: i64 = 30 * 60 * 1000;
/// Full pending-count resync, to pick up anything an event stream missed.
const RESYNC: Duration = Duration::from_secs(30);
/// Mailbox events within this window share one pending-count refresh.
const REFRESH_BATCH: Duration = Duration::from_millis(30);
/// Presence changes within this window go in one report.
const PRESENCE_BATCH: Duration = Duration::from_millis(300);

impl State {
    fn seat(&mut self, name: &str) -> &mut SeatRt {
        self.seats
            .entry(name.to_string())
            .or_insert_with(|| SeatRt::new(name))
    }

    /// A seat named by the server: its entry, created only for a valid name
    /// while the node keeps fewer than [`NODE_MAX_OTHER_SEATS`] seats it
    /// does not host (so a server cannot make the node grow without bound).
    ///
    /// [`NODE_MAX_OTHER_SEATS`]: amail_proto::limits::NODE_MAX_OTHER_SEATS
    fn server_seat(&mut self, name: &str) -> Option<&mut SeatRt> {
        if !self.seats.contains_key(name) {
            let others = self
                .seats
                .iter()
                .filter(|(n, s)| s.harness.is_none() && !self.saved.managed.contains_key(*n))
                .count();
            if !amail_proto::valid_name(name) || others >= amail_proto::limits::NODE_MAX_OTHER_SEATS
            {
                return None;
            }
        }
        Some(self.seat(name))
    }

    /// Change a seat's state; returns true if it changed.
    fn set_state(&mut self, name: &str, s: SeatState) -> bool {
        let seat = self.seat(name);
        if seat.state == s {
            return false;
        }
        seat.state = s;
        seat.since = now_ms();
        seat.clear_wake_failure();
        seat.wake_due = None;
        seat.wake_out = None;
        seat.quiet = Default::default();
        if s != SeatState::Busy {
            seat.stop_blocks = 0;
        }
        if s == SeatState::Offline {
            seat.session = None;
        }
        self.dirty_presence.insert(name.to_string());
        self.record_live(name);
        true
    }

    /// Keep the persisted live session of `seat` in step with memory.
    fn record_live(&mut self, seat: &str) {
        let want = self.seats.get(seat).and_then(|s| {
            if s.state == SeatState::Offline {
                return None;
            }
            let x = s.session.as_ref()?;
            let (pid, pid_start) = x.pid?;
            Some(LiveSession {
                harness: s.harness?.as_str().to_string(),
                session_id: x.id.clone(),
                pid,
                pid_start,
                wake_target: x.target.clone(),
            })
        });
        if self.saved.live.get(seat) != want.as_ref() {
            match want {
                Some(l) => self.saved.live.insert(seat.to_string(), l),
                None => self.saved.live.remove(seat),
            };
            self.save_dirty = true;
        }
    }

    /// Keep the persisted stash of `seat` in step with memory.
    fn record_stash(&mut self, seat: &str) {
        let stash = self
            .seats
            .get(seat)
            .map(|s| s.stash.clone())
            .unwrap_or_default();
        if stash.is_empty() {
            if self.saved.stash.remove(seat).is_some() {
                self.save_dirty = true;
            }
        } else {
            self.saved.stash.insert(seat.to_string(), stash);
            self.save_dirty = true;
        }
    }
}

/// What a hook produced: its stdout, and the claimed mail shown in it (kept
/// for later if the answer cannot be delivered).
#[derive(Debug, Default)]
pub struct HookOutcome {
    pub stdout: String,
    pub seat: Option<String>,
    pub shown: Vec<InboxEntry>,
    /// The answer blocks a `Stop`: if it is not delivered, the session stops.
    pub blocks_stop: bool,
}

/// After a restart, a seat's last activity (for `amail ps`) is the last
/// write to its conversation log, until its next hook or launch.
fn restore_activity(st: &mut State) {
    for (seat, t) in &st.saved.transcripts {
        let Some(s) = st.seats.get_mut(seat).filter(|s| s.last_activity == 0) else {
            continue;
        };
        let modified = std::fs::metadata(&t.path).and_then(|m| m.modified());
        if let Ok(ms) = modified.map(|m| {
            m.duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
        }) {
            s.last_activity = ms;
        }
    }
}
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn ts(ms: i64) -> String {
    // RFC 3339 in UTC without pulling in a date library.
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (h, m, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{:03}Z",
        ms.rem_euclid(1000)
    )
}

/// The start time of process `pid`, if it exists and is not a zombie.
fn proc_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields after the parenthesized command name: state is field 3, the
    // start time field 22.
    let rest = &stat[stat.rfind(')')? + 2..];
    let mut f = rest.split(' ');
    if matches!(f.next(), Some("Z") | Some("X")) {
        return None;
    }
    f.nth(18)?.parse().ok()
}

fn alive(pid: u32, start: Option<u64>) -> bool {
    match proc_start(pid) {
        None => false,
        Some(s) => start.is_none_or(|want| want == s),
    }
}

/// What a hook needs from the server.
#[derive(Debug, Default)]
struct Plan {
    /// Claim `to`/`cc`/`list` (all immediate classes).
    claim_all: bool,
    /// Claim class `to`.
    claim_to: bool,
    /// Claim urgent `cc`/`list`.
    claim_urgent: bool,
    /// Claim urgent mail of every class (in focus).
    claim_urgent_all: bool,
    /// Claim digest mail and show it as a digest.
    digest: bool,
    /// Show unread headers (nothing claimed).
    summary: bool,
    /// Show the count of the seat's open tasks (session start).
    tasks: bool,
    /// Messages the session was shown already; left out of the summary.
    listed: HashSet<i64>,
    /// Mail claimed earlier whose injection was lost.
    stash: Vec<InboxEntry>,
    /// The prompt is a wake-up nudge and the node is connected: when the
    /// hook then shows nothing, it says so, and the turn ends at once.
    wake_prompt: bool,
    /// Show the cached document index: `Some(true)` show it (session
    /// start), `Some(false)` show it only when it changed since the seat
    /// saw it.
    index: Option<bool>,
    /// Refresh the cached index in the background.
    index_fetch: bool,
    /// Wait for that refresh, within a part of the deadline (session start
    /// with nothing cached).
    index_wait: bool,
    /// Show the seat's handover note, when it has been fetched.
    note: bool,
    /// Fetch the note in the background.
    note_fetch: bool,
    /// Wait for that fetch, within a part of the deadline (session start).
    note_wait: bool,
    /// `Stop`: see whether to ask for the handover note before the turn
    /// ends.
    handover: bool,
}

impl Plan {
    fn none(&self) -> bool {
        !(self.claim_all
            || self.claim_to
            || self.claim_urgent
            || self.claim_urgent_all
            || self.digest
            || self.summary
            || self.tasks
            || self.index.is_some()
            || self.note
            || self.handover)
            && self.stash.is_empty()
    }
}

/// A new idempotency key for one claim.
fn claim_key() -> String {
    hex::encode(rand::random::<[u8; 12]>())
}

/// A model name the node passes to a harness: it cannot be read as an
/// option, and has no spaces or control characters.
fn valid_model(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= 200
        && !m.starts_with('-')
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:/@[]+".contains(c))
}

/// After a refresh of pending counts: the seats to ask again, and the
/// seats the server refused for good (a permanent error of a per-seat
/// request: asking again cannot help). A failed request for the whole org
/// is asked again for every seat.
fn after_refresh(
    whole: bool,
    dirty: &[String],
    results: &[Result<amail_proto::PendingResponse>],
) -> (Vec<String>, Vec<String>) {
    let (mut retry, mut refused) = (Vec::new(), Vec::new());
    if whole {
        if results.iter().any(|r| r.is_err()) {
            retry.extend(dirty.iter().cloned());
        }
        return (retry, refused);
    }
    for (seat, r) in dirty.iter().zip(results) {
        match r {
            Ok(_) => {}
            Err(e) if crate::server::is_permanent(e) => refused.push(seat.clone()),
            Err(_) => retry.push(seat.clone()),
        }
    }
    (retry, refused)
}

/// RFC 3339 to Unix ms.
fn parse_ts(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

pub struct Node {
    /// This node, for tasks spawned from `&self` methods.
    me: std::sync::Weak<Node>,
    cfg: Config,
    server: ServerClient,
    mux: Arc<dyn crate::mux::Mux>,
    sup_kick: Notify,
    save_lock: Mutex<()>,
    claude: Arc<dyn Adapter>,
    codex: Arc<dyn Adapter>,
    dsh: Arc<dyn Adapter>,
    prepared: HashMap<Harness, Arc<OnceCell<Result<(), String>>>>,
    st: Mutex<State>,
    state_file: PathBuf,
    wake_kick: Notify,
    presence_kick: Notify,
    refresh_kick: Notify,
    retry_kick: Notify,
    release_kick: Notify,
    check_kick: Notify,
    jobs_kick: Notify,
    /// A compaction draft was filed (or will not be): see
    /// [`handover::SeatHo::draft_wait`].
    draft_done: Notify,
    started: Instant,
}

/// A running node. Dropping it does not stop it; call [`NodeHandle::shutdown`].
pub struct NodeHandle {
    pub node: Arc<Node>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl NodeHandle {
    /// Stop at once, as if the process had been killed: nothing is saved
    /// beyond what the save loop already wrote, and the socket file stays.
    pub fn crash(self) {
        for t in &self.tasks {
            t.abort();
        }
    }

    pub fn shutdown(self) {
        for t in &self.tasks {
            t.abort();
        }
        self.node.save_now();
        let _ = std::fs::remove_file(&self.node.cfg.socket);
    }
}

/// Start a node: bind the socket and spawn its tasks. Supervised sessions
/// run in zellij.
pub async fn start(cfg: Config) -> Result<NodeHandle> {
    let mux = Arc::new(
        crate::mux::Zellij::new(
            &cfg.zellij_bin,
            &cfg.state_dir,
            cfg.pane_cols,
            cfg.pane_rows,
        )
        .with_web_sharing(cfg.web_terminal.is_some()),
    );
    let web = cfg.web_terminal.clone();
    let (zellij, state_dir) = (cfg.zellij_bin.clone(), cfg.state_dir.clone());
    let mut h = start_with_mux(cfg, mux).await?;
    if let Some(w) = web {
        h.tasks
            .push(tokio::spawn(web_terminal_loop(zellij, state_dir, w.port)));
    }
    Ok(h)
}

/// [`start`] with another multiplexer (tests use [`crate::mux::ProcessMux`]).
pub async fn start_with_mux(cfg: Config, mux: Arc<dyn crate::mux::Mux>) -> Result<NodeHandle> {
    std::fs::create_dir_all(&cfg.state_dir)
        .with_context(|| format!("creating {}", cfg.state_dir.display()))?;
    let sock_dir = cfg.socket.parent().context("socket path has no parent")?;
    std::fs::create_dir_all(sock_dir)?;
    std::fs::set_permissions(
        sock_dir,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .with_context(|| format!("securing {}", sock_dir.display()))?;
    crate::paths::check_socket_dir(sock_dir)?;
    if cfg.socket.exists() {
        if tokio::net::UnixStream::connect(&cfg.socket).await.is_ok() {
            bail!("a node is already listening on {}", cfg.socket.display());
        }
        std::fs::remove_file(&cfg.socket)?;
    }
    let listener = tokio::net::UnixListener::bind(&cfg.socket)
        .with_context(|| format!("binding {}", cfg.socket.display()))?;

    let state_file = cfg.state_dir.join("node-state.json");
    let mut st = State {
        prior_state: state_file.exists(),
        turn_fail_limit: cfg.turn_fail_limit,
        saved: persist::load(&state_file),
        ..Default::default()
    };
    let codex_pass = match &st.saved.codex_pass {
        Some(p) => p.clone(),
        None => {
            let p = hex::encode(rand::random::<[u8; 32]>());
            st.saved.codex_pass = Some(p.clone());
            st.save_dirty = true;
            p
        }
    };
    // Transcripts recorded before paths were checked: keep those in the
    // default transcript directories.
    let defaults = [
        cfg.claude_config_dir.join("projects"),
        crate::adapter::codex::home_in(&cfg.state_dir).join("sessions"),
    ];
    st.saved.transcripts.retain(|_, t| {
        if t.base.is_some() {
            return true;
        }
        t.base = defaults
            .iter()
            .find(|b| crate::transcript::path_under(std::path::Path::new(&t.path), b))
            .map(|b| b.to_string_lossy().into_owned());
        st.save_dirty = true;
        t.base.is_some()
    });
    // Sessions that ended while the node was down.
    let before = st.saved.roots.len();
    st.saved.roots.retain(|_, r| alive(r.pid, r.pid_start));
    st.save_dirty |= st.saved.roots.len() != before;
    let env = AdapterEnv {
        state_dir: cfg.state_dir.clone(),
        amail_exe: cfg.amail_exe.clone(),
        node_socket: cfg.socket.clone(),
        server_url: cfg.url.clone(),
        claude_bin: cfg.claude_bin.clone(),
        codex_bin: cfg.codex_bin.clone(),
        codex_sandbox: cfg.codex_sandbox,
        dsh_bin: cfg.dsh_bin.clone(),
        dsh_env: cfg.dsh_env.clone(),
        codex_auth: cfg.codex_auth.clone(),
        codex_pass,
    };
    let claude: Arc<dyn Adapter> = Arc::new(crate::adapter::claude::Claude::new(env.clone()));
    let codex: Arc<dyn Adapter> = Arc::new(crate::adapter::codex::Codex::new(env.clone()));
    let dsh: Arc<dyn Adapter> = Arc::new(crate::adapter::dsh::Dsh::new(env));
    // A surviving process is not evidence of a normal last turn. Codex can
    // retain a live CLI after a failed request; hold automatic wakes until
    // positive reconciliation or a real new in-progress hook.
    for (seat, l) in st.saved.live.clone() {
        let adapter = match Harness::parse(&l.harness) {
            Some(Harness::Claude) => &claude,
            Some(Harness::Codex) => &codex,
            Some(Harness::Dsh) => &dsh,
            None => continue,
        };
        let ok = alive(l.pid, l.pid_start)
            && l.wake_target
                .as_deref()
                .is_none_or(|t| adapter.target_alive(t));
        if !ok {
            st.saved.live.remove(&seat);
            st.save_dirty = true;
            continue;
        }
        let s = st.seat(&seat);
        s.harness = Some(adapter.harness());
        s.restore_pending = adapter.harness() == Harness::Codex;
        s.state = if s.restore_pending {
            SeatState::Busy
        } else {
            SeatState::Idle
        };
        s.session = Some(SessionRt {
            id: l.session_id.clone(),
            pid: Some((l.pid, l.pid_start)),
            target: l.wake_target.clone(),
        });
        tracing::info!("restored the {} session of seat {seat}", l.harness);
    }
    for (seat, entries) in st.saved.stash.clone() {
        st.seat(&seat).stash = entries;
    }
    for (seat, n) in st.saved.wakes.clone() {
        st.seat(&seat).wakes = n;
    }
    supervise::restore(&mut st, |h| match Harness::parse(h) {
        Some(Harness::Claude) => Some(claude.harness()),
        Some(Harness::Codex) => Some(codex.harness()),
        Some(Harness::Dsh) => Some(dsh.harness()),
        None => None,
    });
    restore_activity(&mut st);
    let server = ServerClient::new(&cfg.url, &cfg.token, cfg.hook_server_timeout)?;
    let node = Arc::new_cyclic(|me| Node {
        me: me.clone(),
        server,
        claude,
        codex,
        dsh,
        prepared: [Harness::Claude, Harness::Codex, Harness::Dsh]
            .into_iter()
            .map(|h| (h, Arc::new(OnceCell::new())))
            .collect(),
        cfg,
        mux,
        sup_kick: Notify::new(),
        save_lock: Mutex::new(()),
        st: Mutex::new(st),
        state_file,
        wake_kick: Notify::new(),
        presence_kick: Notify::new(),
        refresh_kick: Notify::new(),
        retry_kick: Notify::new(),
        release_kick: Notify::new(),
        check_kick: Notify::new(),
        jobs_kick: Notify::new(),
        draft_done: Notify::new(),
        started: Instant::now(),
    });

    let mut tasks = Vec::new();
    for h in [Harness::Claude, Harness::Codex, Harness::Dsh] {
        let n = node.clone();
        tasks.push(tokio::spawn(async move {
            n.ensure_prepared(h).await.ok();
        }));
    }
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.accept_loop(listener).await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.events_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.refresh_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.wake_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.presence_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.liveness_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.save_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.supervise_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.retry_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.release_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.turn_check_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.worklog_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.jobs_loop().await }));
    let n = node.clone();
    tasks.push(tokio::spawn(async move { n.handover_idle_loop().await }));
    if node.cfg.notices {
        let n = node.clone();
        tasks.push(tokio::spawn(async move { n.notice_loop().await }));
    }
    Ok(NodeHandle { node, tasks })
}

impl Node {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn adapter(&self, h: Harness) -> &Arc<dyn Adapter> {
        match h {
            Harness::Claude => &self.claude,
            Harness::Codex => &self.codex,
            Harness::Dsh => &self.dsh,
        }
    }

    async fn ensure_prepared(&self, h: Harness) -> Result<()> {
        let cell = self.prepared[&h].clone();
        let r = cell
            .get_or_init(|| async {
                self.adapter(h)
                    .prepare()
                    .await
                    .map_err(|e| format!("{e:#}"))
            })
            .await;
        match r {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("preparing {}: {e}", h.as_str());
                bail!("preparing {}: {e}", h.as_str())
            }
        }
    }

    /// Write the state file if it changed.
    fn save_now(&self) {
        // One writer at a time, so an older snapshot never replaces a newer.
        let _w = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let saved = {
            let mut st = self.lock();
            if !st.save_dirty {
                return;
            }
            st.save_dirty = false;
            st.saved.clone()
        };
        if let Err(e) = persist::save(&self.state_file, &saved) {
            tracing::warn!("saving node state: {e:#}");
            self.lock().save_dirty = true;
        }
    }

    /// State changes are written in batches, off the hook path.
    async fn save_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        loop {
            tick.tick().await;
            let n = self.clone();
            let _ = tokio::task::spawn_blocking(move || n.save_now()).await;
        }
    }

    /// Keep claimed mail whose answer was not delivered. An undelivered
    /// `Stop` block means the session did stop, so the seat is idle and the
    /// waker can deliver the kept mail.
    fn stash(&self, seat: &str, entries: Vec<InboxEntry>, blocks_stop: bool) {
        if blocks_stop {
            let mut st = self.lock();
            st.set_state(seat, SeatState::Idle);
            drop(st);
            self.presence_kick.notify_one();
            self.wake_kick.notify_one();
        }
        if entries.is_empty() {
            return;
        }
        tracing::info!(
            "seat {seat}: hook answer not delivered; keeping {} message(s) for the next hook",
            entries.len()
        );
        let mut st = self.lock();
        st.unshow(seat, &entries);
        let s = st.seat(seat);
        for e in &entries {
            s.listed.remove(&e.message.id);
        }
        let mut all = entries;
        all.append(&mut s.stash);
        all.sort_by_key(|e| e.delivery.seq);
        all.dedup_by_key(|e| e.message.id);
        s.stash = all;
        st.record_stash(seat);
        drop(st);
        self.wake_kick.notify_one();
    }

    /// Keep claimed mail that did not fit in a hook's answer for the next
    /// hook of the seat.
    fn keep(&self, seat: &str, entries: Vec<InboxEntry>) {
        let mut st = self.lock();
        let s = st.seat(seat);
        let mut all = entries;
        all.append(&mut s.stash);
        all.sort_by_key(|e| e.delivery.seq);
        all.dedup_by_key(|e| e.message.id);
        s.stash = all;
        st.record_stash(seat);
        drop(st);
        self.wake_kick.notify_one();
    }

    // --- socket ---------------------------------------------------------------

    async fn accept_loop(self: Arc<Self>, listener: tokio::net::UnixListener) {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let n = self.clone();
                    tokio::spawn(async move { n.serve_conn(stream).await });
                }
                Err(e) => {
                    tracing::warn!("accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn serve_conn(self: Arc<Self>, stream: tokio::net::UnixStream) {
        // The connecting process, as the kernel reports it.
        let peer = stream
            .peer_cred()
            .ok()
            .map(|c| (c.pid().unwrap_or(0).max(0) as u32, c.uid()));
        let (r, mut w) = stream.into_split();
        let mut line = String::new();
        let max = amail_proto::limits::NODE_MAX_REQUEST_BYTES;
        let mut reader = BufReader::new(r).take(max as u64);
        let read = reader.read_line(&mut line);
        match tokio::time::timeout(Duration::from_secs(10), read).await {
            Ok(Ok(n)) if n > 0 => {}
            _ => return,
        }
        if line.len() >= max && !line.ends_with('\n') {
            let msg = format!("{{\"error\":\"request larger than {max} bytes\"}}\n");
            let _ = w.write_all(msg.as_bytes()).await;
            return;
        }
        let started = Instant::now();
        let req = serde_json::from_str::<Request>(&line);
        // Under the open policy every process of the person's OS user is
        // served; the callers of seat management and credential requests
        // are still identified, to tell the person about agent sessions
        // that make them (see `notices`).
        let restricted = self.local_policy() == amail_proto::LocalPolicy::Restricted;
        let caller = match (&req, self.cfg.check_callers) {
            (Ok(r), true) => match peer {
                Some((pid, uid)) if pid > 0 && uid == crate::paths::uid() => {
                    let for_person =
                        !matches!(r, Request::Hook { .. } | Request::Credentials { .. });
                    if restricted {
                        Some(self.caller(pid, for_person))
                    } else {
                        Some(self.open_caller(pid, r))
                    }
                }
                _ => {
                    let _ = w
                        .write_all(b"{\"error\":\"cannot identify the calling process\"}\n")
                        .await;
                    return;
                }
            },
            _ => None,
        };
        let resp = match req {
            Ok(Request::Hook {
                harness,
                env,
                input,
                budget_ms,
            }) => {
                if let Some(c) = &caller
                    && restricted
                    && !self.hook_allowed(c, &harness, &env)
                {
                    tracing::warn!(
                        "refused a {harness} hook from process {}: not a session this node \
                         launched for the seat it names",
                        c.pid
                    );
                    let _ = w.write_all(b"{\"stdout\":\"\"}\n").await;
                    return;
                }
                // Open policy: a Claude Code or dsh session sending a hook
                // in another seat's name reads and claims that seat's mail.
                // (dsh sessions are identified from inside their jail too.)
                if let Some(c) = &caller
                    && !restricted
                    && let Some(seat) = env.get("AMAIL_SEAT")
                    && let Some((own, h)) = &c.session
                    && matches!(h, Harness::Claude | Harness::Dsh)
                    && own != seat
                {
                    self.note_request(c, amail_proto::NoticeOp::Hook, vec![seat.clone()]);
                }
                let deadline = budget_ms
                    .map(|b| started + Duration::from_millis(b).saturating_sub(LATE_MARGIN));
                let out = self.hook_outcome(&harness, &env, &input, deadline).await;
                // Too late: the client is gone or about to give up. Keep the
                // mail for the next hook rather than risk losing it.
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    if let Some(seat) = &out.seat {
                        self.stash(seat, out.shown, out.blocks_stop);
                    }
                    let _ = w.write_all(b"{\"stdout\":\"\"}\n").await;
                    return;
                }
                let mut line = json!({"stdout": out.stdout}).to_string();
                line.push('\n');
                if let Err(e) = w.write_all(line.as_bytes()).await {
                    tracing::debug!("hook answer not delivered: {e}");
                    if let Some(seat) = &out.seat {
                        self.stash(seat, out.shown, out.blocks_stop);
                    }
                }
                return;
            }
            Ok(req) => {
                let up = matches!(req, Request::Up { .. } | Request::Ps | Request::Status);
                let mut v = self.handle_from(&caller, restricted, req).await;
                // Best effort (see `caller`): say which agent session asks
                // (`amail up` asks `ps` first), so that what it changes on
                // the server can name it.
                if up
                    && let Some(c) = &caller
                    && c.is_session()
                    && let Some(o) = v.as_object_mut()
                {
                    let who = match &c.session {
                        Some((seat, _)) => format!("seat {seat}"),
                        None => "a Codex session".to_string(),
                    };
                    o.insert("caller_session".into(), json!(who));
                }
                v
            }
            Err(e) => json!({"error": format!("bad request: {e}")}),
        };
        let mut out = resp.to_string();
        out.push('\n');
        let _ = w.write_all(out.as_bytes()).await;
    }

    async fn handle_from(
        &self,
        caller: &Option<caller::Caller>,
        restricted: bool,
        req: Request,
    ) -> Value {
        match caller {
            None => self.handle(req).await,
            Some(c) if !restricted => self.handle(self.authorize_open(c, req)).await,
            Some(c) => match self.authorize(c, req) {
                Ok(req) => self.handle(req).await,
                Err(e) => {
                    tracing::warn!("refused a request from process {}: {e}", c.pid);
                    json!({"error": e})
                }
            },
        }
    }

    pub async fn handle(&self, req: Request) -> Value {
        match req {
            Request::Hook {
                harness,
                env,
                input,
                ..
            } => json!({"stdout": self.hook(&harness, &env, &input).await}),
            Request::Launch {
                seat,
                cwd,
                pid,
                harness,
                extra,
                pane,
                mux_session,
                permission_mode,
                git_access,
                dsh_jail,
                session,
            } => match self
                .launch(
                    &seat,
                    &cwd,
                    pid,
                    harness.as_deref(),
                    &extra,
                    pane,
                    mux_session,
                    permission_mode,
                    git_access,
                    dsh_jail,
                    session.as_deref(),
                )
                .await
            {
                Ok(plan) => serde_json::to_value(plan).unwrap_or_default(),
                Err(e) => json!({"error": format!("{e:#}")}),
            },
            Request::Credentials { codex_thread, pass } => {
                // Under both local policies: a thread id alone (Codex shows
                // them in file names and session lists) gets nothing.
                if !self.codex_pass_ok(pass.as_deref()) {
                    json!({"error": "credentials are given only to the seat's own Codex session \
                                     (the node's Codex pass is missing or wrong)"})
                } else {
                    match self.credentials(&codex_thread) {
                        Some(c) => serde_json::to_value(c).unwrap_or_default(),
                        None => json!({"error": "no seat is registered for this thread"}),
                    }
                }
            }
            Request::Status => self.status(),
            Request::Up {
                seats,
                settings,
                start,
                only,
            } => self.sup_up(seats, settings, start, only).await,
            Request::Down { seats } => self.sup_down(seats).await,
            Request::Forget { seats } => self.sup_forget(seats).await,
            Request::Sleep { seat } => self.sup_sleep(&seat).await,
            Request::Wake { seat } => self.sup_wake(&seat).await,
            Request::Ps => self.sup_ps(),
            Request::Locate { target } => self.sup_locate(&target),
            Request::CheckSessions { claims } => self.sup_check_sessions(claims).await,
            Request::Adopt {
                seat,
                session,
                restart,
            } => self.sup_adopt(&seat, &session, restart).await,
        }
    }

    /// Check a request (other than a hook) against its sender; returns the
    /// request to serve, with a launch's pid set to the sender's.
    fn authorize(&self, caller: &caller::Caller, req: Request) -> Result<Request, String> {
        match req {
            Request::Hook { .. } => Ok(req),
            Request::Credentials { codex_thread, pass } => {
                if caller.is_claude() || !self.codex_pass_ok(pass.as_deref()) {
                    return Err("credentials are given only to the seat's own Codex session".into());
                }
                Ok(Request::Credentials { codex_thread, pass })
            }
            _ if caller.is_session() => Err(
                "this request is for the person, not for agent sessions: run it outside \
                 the sessions this node launched"
                    .into(),
            ),
            Request::Launch {
                seat,
                cwd,
                harness,
                extra,
                pane,
                mux_session,
                permission_mode,
                git_access,
                dsh_jail,
                session,
                ..
            } => Ok(Request::Launch {
                seat,
                cwd,
                // `amail run` becomes the harness: its process is the session.
                pid: caller.pid,
                harness,
                extra,
                pane,
                mux_session,
                permission_mode,
                git_access,
                dsh_jail,
                session,
            }),
            req => Ok(req),
        }
    }

    /// The caller of `req` under the open policy: identified (by its
    /// parent processes) only for requests that are reported when an agent
    /// session makes them.
    fn open_caller(&self, pid: u32, req: &Request) -> caller::Caller {
        let watched = match req {
            Request::Hook { env, .. } => env.contains_key("AMAIL_SEAT"),
            Request::Credentials { .. } => true,
            r => notices::request_op(r).is_some(),
        };
        if !self.cfg.notices || !watched {
            return caller::Caller {
                pid,
                ..Default::default()
            };
        }
        let hook = matches!(req, Request::Hook { .. });
        self.caller(pid, !hook)
    }

    /// Serve `req` from `caller` under the open policy: a launch's pid is
    /// the caller's; requests of agent sessions that manage seats or obtain
    /// another seat's credentials are recorded for a notice.
    fn authorize_open(&self, caller: &caller::Caller, req: Request) -> Request {
        if let Some((op, targets)) = notices::request_op(&req) {
            self.note_request(caller, op, targets);
        }
        match req {
            Request::Credentials { codex_thread, pass } => {
                // Codex sessions use this for their own seat; a Claude Code
                // session has no reason to. (The pass is checked when the
                // request is served, under both policies.)
                if caller.is_claude() {
                    let seat = self
                        .lock()
                        .saved
                        .sessions
                        .get(&codex_thread)
                        .map(|r| r.seat.clone());
                    self.note_request(
                        caller,
                        amail_proto::NoticeOp::Credentials,
                        seat.into_iter().collect(),
                    );
                }
                Request::Credentials { codex_thread, pass }
            }
            Request::Launch {
                seat,
                cwd,
                harness,
                extra,
                pane,
                mux_session,
                permission_mode,
                git_access,
                dsh_jail,
                session,
                ..
            } => Request::Launch {
                seat,
                cwd,
                // `amail run` becomes the harness: its process is the session.
                pid: caller.pid,
                harness,
                extra,
                pane,
                mux_session,
                permission_mode,
                git_access,
                dsh_jail,
                session,
            },
            req => req,
        }
    }

    // --- hooks ----------------------------------------------------------------

    /// Handle one hook; returns its stdout.
    /// How long one hook's server calls may take in total: twice the
    /// slowest answer measured in the last [`crate::server::LATENCY_WINDOW`]
    /// plus a little, at least `hook_server_timeout`, at most
    /// `hook_budget_max`. A hook that waits longer than this would slow the
    /// agent down; one that waits less than the server takes shows mail one
    /// hook late (the claim is retried and kept). Only hooks with mail to
    /// fetch pay this: without pending mail no server call is made.
    fn hook_budget(&self) -> Duration {
        let floor = self.cfg.hook_server_timeout;
        let cap = self.cfg.hook_budget_max.max(floor);
        match self.server.latency().max {
            Some(slowest) => (slowest * 2 + Duration::from_millis(50)).clamp(floor, cap),
            None => floor,
        }
    }

    pub async fn hook(&self, harness: &str, env: &BTreeMap<String, String>, input: &str) -> String {
        self.hook_outcome(harness, env, input, None).await.stdout
    }

    /// Handle one hook. Server calls stop at the configured hook timeout or
    /// at `deadline`, whichever comes first.
    pub async fn hook_outcome(
        &self,
        harness: &str,
        env: &BTreeMap<String, String>,
        input: &str,
        deadline: Option<Instant>,
    ) -> HookOutcome {
        let Some(h) = Harness::parse(harness) else {
            return HookOutcome::default();
        };
        let adapter = self.adapter(h).clone();
        let Ok(v) = serde_json::from_str::<Value>(input) else {
            return HookOutcome::default();
        };
        let Some(hook) = adapter.parse(&v) else {
            return HookOutcome::default();
        };
        if !adapter.accepts(env, &hook) {
            return HookOutcome::default();
        }
        let Some((seat, registered)) = self.identify(&*adapter, env, &hook) else {
            return HookOutcome::default();
        };
        tracing::debug!("seat {seat}: {} hook {}", h.as_str(), hook.name);
        if registered && let Some(sid) = hook.session_id.clone() {
            let (a, s) = (adapter.clone(), seat.clone());
            tokio::spawn(async move { a.registered(&sid, &s).await });
        }
        let own = Instant::now() + self.hook_budget();
        let deadline = deadline.map_or(own, |d| d.min(own));
        let (out, shown) = self
            .run_hook(&*adapter, env, &hook, &seat, registered, deadline)
            .await;
        HookOutcome {
            stdout: adapter.render(&hook, &out),
            seat: Some(seat),
            shown,
            blocks_stop: matches!(out, Output::Block(_)),
        }
    }

    /// The seat a hook belongs to, and whether this hook just registered
    /// its session.
    fn identify(
        &self,
        adapter: &dyn Adapter,
        env: &BTreeMap<String, String>,
        hook: &Hook,
    ) -> Option<(String, bool)> {
        if let Some(seat) = adapter.seat_from_env(env) {
            return Some((seat, false));
        }
        if !adapter.registers_sessions() {
            return None;
        }
        let sid = hook.session_id.as_deref()?;
        let mut st = self.lock();
        if let Some(reg) = st.saved.sessions.get(sid) {
            return Some((reg.seat.clone(), false));
        }
        let Event::UserPromptSubmit { prompt } = &hook.event else {
            return None;
        };
        let (seat, nonce) = adapter.registration_marker(prompt)?;
        let now = now_ms();
        st.saved.reservations.retain(|_, r| r.expires > now);
        match st.saved.reservations.get(&nonce) {
            Some(r) if r.seat == seat => {}
            _ => return None,
        }
        let r = st.saved.reservations.remove(&nonce)?;
        st.saved.sessions.insert(
            sid.to_string(),
            SessionReg {
                seat: seat.clone(),
                pid: Some(r.pid),
            },
        );
        let s = st.seat(&seat);
        s.harness = Some(adapter.harness());
        s.session = Some(SessionRt {
            id: Some(sid.to_string()),
            pid: Some((r.pid, proc_start(r.pid))),
            target: None,
        });
        st.save_dirty = true;
        tracing::info!(
            "registered {} session {sid} as seat {seat}",
            adapter.harness().as_str()
        );
        Some((seat, true))
    }

    /// Directories where the harness writes `seat`'s transcripts: for
    /// Codex the dedicated home's `sessions`; for dsh the seat's dsh home's
    /// `sessions`; for Claude Code `projects` of
    /// the configuration directory the seat's session was started with
    /// (from its environment) and of the node's.
    fn transcript_bases(&self, h: Harness, seat: &str) -> Vec<PathBuf> {
        match h {
            Harness::Codex => {
                vec![crate::adapter::codex::home_in(&self.cfg.state_dir).join("sessions")]
            }
            Harness::Dsh => {
                vec![crate::adapter::dsh::seat_home_in(&self.cfg.state_dir, seat).join("sessions")]
            }
            Harness::Claude => {
                let mut v = vec![self.cfg.claude_config_dir.join("projects")];
                let root = self.lock().saved.roots.get(seat).cloned();
                if let Some(r) = root.filter(|r| alive(r.pid, r.pid_start)) {
                    let dir = caller::environ_var(r.pid, "CLAUDE_CONFIG_DIR")
                        .filter(|d| !d.is_empty())
                        .map(PathBuf::from)
                        .or_else(|| {
                            caller::environ_var(r.pid, "HOME")
                                .map(|h| PathBuf::from(h).join(".claude"))
                        });
                    if let Some(d) = dir {
                        v.push(d.join("projects"));
                    }
                }
                v
            }
        }
    }

    /// `(path, base)` if a hook's transcript path lies in a directory where
    /// the harness writes `seat`'s transcripts.
    fn accept_transcript(&self, h: Harness, seat: &str, path: &str) -> Option<(String, String)> {
        let known = self
            .lock()
            .saved
            .transcripts
            .get(seat)
            .filter(|t| t.path == path && t.harness == h.as_str())
            .and_then(|t| t.base.clone());
        if let Some(base) = known {
            return Some((path.to_string(), base));
        }
        let p = std::path::Path::new(path);
        match self
            .transcript_bases(h, seat)
            .into_iter()
            .find(|b| crate::transcript::path_under(p, b))
        {
            Some(b) => Some((path.to_string(), b.to_string_lossy().into_owned())),
            None => {
                tracing::warn!(
                    "seat {seat}: ignoring transcript path {path:?}, which is outside the \
                     directories where {} writes transcripts",
                    h.as_str()
                );
                None
            }
        }
    }

    /// Run the state machine for one hook; returns the answer and the
    /// claimed mail it shows.
    async fn run_hook(
        &self,
        adapter: &dyn Adapter,
        env: &BTreeMap<String, String>,
        hook: &Hook,
        seat: &str,
        registered: bool,
        deadline: Instant,
    ) -> (Output, Vec<InboxEntry>) {
        let hint = adapter.session_hint(env, hook);
        let cap = self.cfg.stop_block_cap;
        let transcript = adapter
            .transcript(hook, seat)
            .and_then(|p| self.accept_transcript(adapter.harness(), seat, &p));
        // The context's size, from the transcript (read before the lock).
        let over = hook.event == Event::Stop
            && self.lock().connected
            && self.context_over(adapter.harness(), seat);
        // 1. State transition and plan, under the lock.
        let plan = {
            let mut st = self.lock();
            let connected = st.connected;
            if hook.event == Event::SessionEnd {
                // Logged with the hook's own session: a late SessionEnd
                // may belong to a session that is gone already.
                let reason = hook.facts.reason.as_deref().unwrap_or("not given");
                st.log_session(
                    seat,
                    hook.session_id.clone(),
                    "session_end",
                    format!("reason: {reason}"),
                );
            }
            if st.saved.managed.contains_key(seat) {
                // Supervised: the process is watched directly. SessionEnd
                // may come late (Codex: about a minute after the TUI ended,
                // possibly after the seat was restarted), so it is ignored.
                if hook.event == Event::SessionEnd {
                    return (Output::None, Vec::new());
                }
                // A session is worth resuming once it has a turn (Claude Code
                // cannot resume a session that never had one).
                let resumable = match hook.event {
                    Event::SessionStart { .. } => None,
                    _ => hook.session_id.as_deref(),
                };
                if supervise::on_hook(&mut st, seat, resumable) {
                    self.sup_kick.notify_one();
                }
            }
            st.seat(seat).last_activity = now_ms();
            if connected && st.want_note_state(seat, false) {
                self.jobs_kick.notify_one();
            }
            {
                let s = st.seat(seat);
                s.harness = Some(adapter.harness());
                // A different session id means a new session took the seat,
                // except that a late SessionEnd of an old session is ignored.
                let current = s.session.as_ref().and_then(|x| x.id.clone());
                if hook.event == Event::SessionEnd
                    && hook.session_id.is_some()
                    && current.is_some()
                    && current != hook.session_id
                {
                    return (Output::None, Vec::new());
                }
                let sess = s.session.get_or_insert_with(SessionRt::default);
                if hook.session_id.is_some() {
                    sess.id = hook.session_id.clone();
                }
                if let Some(pid) = hint.pid
                    && sess.pid.map(|p| p.0) != Some(pid)
                {
                    sess.pid = Some((pid, proc_start(pid)));
                }
                if hint.wake_target.is_some() {
                    sess.target = hint.wake_target.clone();
                }
            }
            if let Some((path, base)) = transcript {
                let r = crate::persist::TranscriptRef {
                    harness: adapter.harness().as_str().to_string(),
                    path,
                    session_id: hook.session_id.clone(),
                    base: Some(base),
                };
                if st.saved.transcripts.get(seat) != Some(&r) {
                    st.saved.transcripts.insert(seat.to_string(), r);
                    st.save_dirty = true;
                }
            }
            {
                let ho = &mut st.seat(seat).ho;
                if let Some(c) = &hook.facts.cwd {
                    ho.cwd = Some(c.clone());
                }
                if let Some(m) = &hook.facts.model {
                    ho.model = Some(m.clone());
                }
            }
            let mut plan = Plan::default();
            let now = now_ms();
            let index_due = {
                let every = self.cfg.index_recheck;
                let ho = &st.seat(seat).ho;
                ho.index_checked.is_none_or(|t| t.elapsed() >= every)
            };
            let cwd = st.seat(seat).ho.cwd.clone();
            let head_job = |log: bool| {
                cwd.clone().map(|cwd| handover::Job::Head {
                    seat: seat.to_string(),
                    cwd,
                    session: hook.session_id.clone(),
                    log,
                })
            };
            // When a session starts or a turn ends with nothing to show, the
            // seat's pending count is fetched again: a mailbox event lost on
            // the way (a dropped stream, another server instance that died
            // before telling the others) would otherwise leave the seat's
            // mail unnoticed until the next full resync, 30 s later. The
            // seat is woken when the answer shows mail.
            let mut recheck = false;
            match &hook.event {
                Event::SessionStart { source } => {
                    if let Some(s) = adapter.session_start_state(source) {
                        st.set_state(seat, s);
                    }
                    // A new (or resumed) session has seen nothing yet.
                    st.seat(seat).listed.clear();
                    plan.summary = connected;
                    plan.tasks = connected;
                    plan.index = connected.then_some(true);
                    let ho = &mut st.seat(seat).ho;
                    ho.index_hash = None;
                    ho.asked = false;
                    // The note is due until shown (or found empty); a fetch
                    // of an earlier session start no longer counts.
                    ho.note_pending =
                        matches!(source.as_str(), "startup" | "resume" | "clear" | "compact");
                    ho.note_fetched = None;
                    ho.note_fetching = false;
                    ho.note_gen += 1;
                    // Claude Code runs `SessionStart` (compact) before
                    // `PostCompact`: the note is fetched once the
                    // compaction's draft is filed, so that the session is
                    // shown the draft and not the note before it.
                    ho.draft_wait = (source == "compact" && ho.draft_expected())
                        .then(|| Instant::now() + handover::DRAFT_WAIT);
                    ho.compacted = None;
                    plan.index_wait = ho.index.is_none();
                    plan.note_wait = ho.note_pending;
                    st.log(seat, "session_start", format!("source: {source}"));
                    st.jobs.extend(head_job(false));
                    self.jobs_kick.notify_one();
                    // Ask the server again (see `recheck_pending`).
                    recheck = true;
                }
                Event::UserPromptSubmit { prompt } => {
                    if registered {
                        // The session's own `SessionStart` came before its
                        // registration and could not be tied to the seat
                        // (Codex): this prompt starts the session.
                        st.log(seat, "session_start", "source: startup");
                        let ho = &mut st.seat(seat).ho;
                        ho.note_pending = true;
                        ho.note_fetched = None;
                        ho.note_fetching = false;
                        ho.note_gen += 1;
                        plan.note_wait = connected;
                    }
                    st.seat(seat).restore_pending = false;
                    st.set_state(seat, SeatState::Busy);
                    self.turn_started(&mut st, seat);
                    let s = st.seat(seat);
                    s.stop_blocks = 0;
                    s.wake_out = None;
                    s.clear_wake_failure();
                    plan.stash = std::mem::take(&mut s.stash);
                    let wake = crate::inject::is_wake_text(prompt);
                    plan.wake_prompt = connected && wake;
                    s.ho.turn_start = Some(now);
                    s.ho.asked = false;
                    s.ho.task_tool = false;
                    if s.rq.turn_started() {
                        st.dirty_presence.insert(seat.to_string());
                    }
                    let s = st.seat(seat);
                    // A turn whose end is still being checked stays open.
                    s.ho.turns_open = s.ho.turns_open.min(u32::from(s.turn_check.is_some())) + 1;
                    // A wake-up whose mail is gone should end at once.
                    plan.index = (connected && !wake).then_some(false);
                    if s.focused(now) {
                        plan.claim_urgent_all = connected && s.pending.urgent > 0;
                    } else {
                        plan.claim_all = connected && s.pending.total() > 0;
                        plan.digest = connected && s.pending.digest > 0;
                    }
                    plan.summary = connected && registered;
                    st.log(
                        seat,
                        "turn_start",
                        if wake {
                            "turn started by a wake-up"
                        } else {
                            "turn started"
                        },
                    );
                }
                Event::PostToolUse => {
                    st.seat(seat).restore_pending = false;
                    st.set_state(seat, SeatState::Busy);
                    if let Some(tool) = &hook.facts.tool {
                        let input = hook.facts.tool_input.as_deref().unwrap_or_default();
                        if hook.facts.task_tool {
                            let ho = &mut st.seat(seat).ho;
                            ho.task_tool = true;
                            // The task it holds may have changed: from now
                            // on the server fills it in.
                            ho.doing = None;
                        }
                        if hook.facts.ask_tool {
                            st.seat(seat).rq.asked = true;
                        }
                        if hook.facts.note_tool && connected && st.want_note_state(seat, true) {
                            self.jobs_kick.notify_one();
                        }
                        let text = if input.is_empty() {
                            tool.clone()
                        } else {
                            format!("{tool}: {input}")
                        };
                        st.log(seat, "tool", text);
                    }
                    let s = st.seat(seat);
                    plan.stash = std::mem::take(&mut s.stash);
                    if s.focused(now) {
                        plan.claim_urgent_all = connected && s.pending.urgent > 0;
                    } else {
                        plan.claim_to = connected && s.pending.to > 0;
                        plan.claim_urgent = connected && s.pending.urgent > 0;
                    }
                }
                Event::Stop | Event::Interrupt if st.seat(seat).restore_pending => {
                    // A late terminal hook alone cannot prove that the
                    // restored turn succeeded. Do not inject pending mail.
                    st.set_state(seat, SeatState::Busy);
                    self.check_kick.notify_one();
                }
                Event::Stop => {
                    st.jobs.extend(head_job(true));
                    self.jobs_kick.notify_one();
                    let s = st.seat(seat);
                    let focused = s.focused(now);
                    let pending = connected
                        && if focused {
                            s.pending.urgent > 0
                        } else {
                            s.pending.total() > 0 || s.pending.digest > 0
                        };
                    let blocks = (pending || !s.stash.is_empty()) && s.stop_blocks < cap;
                    // The seat asked the person in this turn, and the turn
                    // ends: the node looks for the open request (see
                    // `requests`).
                    if !blocks && connected && std::mem::take(&mut s.rq.asked) {
                        let job = handover::Job::Waiting {
                            seat: seat.to_string(),
                            turn_start: s.ho.turn_start,
                        };
                        st.jobs.push_back(job);
                    }
                    let s = st.seat(seat);
                    if blocks {
                        plan.stash = std::mem::take(&mut s.stash);
                        if focused {
                            plan.claim_urgent_all = pending;
                        } else {
                            plan.claim_all = pending && s.pending.total() > 0;
                            plan.digest = pending && s.pending.digest > 0;
                        }
                        plan.index = connected.then_some(false);
                    } else if connected && !s.ho.asked && (over || s.ho.task_tool || s.ho.requested)
                    {
                        // Only then is the server asked: a turn's end with
                        // nothing to show is otherwise answered from
                        // memory. The turn ends after the check (step 3).
                        plan.handover = true;
                        plan.index = Some(false);
                        recheck = true;
                    } else {
                        st.set_state(seat, SeatState::Idle);
                        self.turn_ended(&mut st, seat, turns::TurnEnd::Normal);
                        recheck = true;
                    }
                }
                Event::Interrupt => {
                    st.set_state(seat, SeatState::Idle);
                    self.turn_ended(&mut st, seat, turns::TurnEnd::Person);
                }
                Event::TurnFailed { reason } => {
                    st.set_state(seat, SeatState::Idle);
                    self.turn_ended(&mut st, seat, turns::TurnEnd::Failed(reason.clone()));
                }
                Event::Awaiting => {
                    st.set_state(seat, SeatState::Awaiting);
                }
                Event::SessionEnd => {
                    st.set_state(seat, SeatState::Offline);
                    st.seat(seat).session = None;
                    self.turn_ended(&mut st, seat, turns::TurnEnd::Person);
                }
                Event::PreCompact { trigger } => {
                    let t = if trigger.is_empty() {
                        "not given"
                    } else {
                        trigger
                    };
                    st.log(seat, "precompact", format!("trigger: {t}"));
                }
                Event::PostCompact { summary } => {
                    st.log(
                        seat,
                        "compact",
                        summary
                            .clone()
                            .unwrap_or_else(|| "context compacted (no summary given)".into()),
                    );
                    // The next injecting hook shows the whole index again.
                    let ho = &mut st.seat(seat).ho;
                    ho.index_hash = None;
                    ho.index_checked = None;
                    // Kept for the session start that reports this
                    // compaction, unless one is waiting for it already.
                    ho.compacted = ho.draft_wait.is_none().then(Instant::now);
                    let turn_start = ho.turn_start;
                    let draft = summary.clone().filter(|_| connected);
                    ho.draft_running = draft.is_some();
                    if draft.is_none() {
                        // No draft comes: a session start waiting for one
                        // fetches the note now.
                        ho.draft_wait = None;
                        self.draft_done.notify_waiters();
                    }
                    if let Some(summary) = draft {
                        st.jobs.push_back(handover::Job::Draft {
                            seat: seat.to_string(),
                            summary,
                            session: hook.session_id.clone(),
                            turn_start,
                        });
                        self.jobs_kick.notify_one();
                    }
                }
            }
            // The index and the note are fetched off the hook path; a hook
            // shows what has arrived (see `handover`).
            if connected {
                let ho = &mut st.seat(seat).ho;
                // A tool call refreshes the cache without showing it.
                let refresh = plan.index.is_some() || hook.event == Event::PostToolUse;
                if refresh && !ho.index_fetching && (index_due || ho.index.is_none()) {
                    ho.index_fetching = true;
                    plan.index_fetch = true;
                }
                let shows = matches!(
                    hook.event,
                    Event::SessionStart { .. }
                        | Event::UserPromptSubmit { .. }
                        | Event::PostToolUse
                        | Event::Stop
                );
                if shows && ho.note_pending {
                    plan.note = true;
                    if ho.note_fetched.is_none() && !ho.note_fetching {
                        ho.note_fetching = true;
                        plan.note_fetch = true;
                    }
                }
            } else {
                plan.index_wait = false;
                plan.note_wait = false;
            }
            if !plan.stash.is_empty() {
                st.record_stash(seat);
            }
            if plan.summary {
                plan.listed = st.seat(seat).listed.clone();
            }
            st.record_live(seat);
            if recheck && connected {
                st.dirty_pending.insert(seat.to_string());
                self.refresh_kick.notify_one();
            }
            plan
        };
        self.presence_kick.notify_one();
        self.wake_kick.notify_one();
        // The index and the note are fetched in tasks of their own, so that
        // they never hold up the mail's answer: started here when the hook
        // has nothing else to do, else once the mail calls are done.
        let spawn_jobs = || {
            let index = plan.index_fetch.then(|| {
                let max = handover::share(adapter.inject_budget(), handover::INDEX_SHARE);
                self.spawn_index(seat, max)
            });
            let note = plan.note_fetch.then(|| self.spawn_note(seat));
            (index.flatten(), note.flatten())
        };
        if plan.none() {
            spawn_jobs();
            let out = if plan.wake_prompt {
                Output::Context(crate::inject::NOTHING_PENDING.to_string())
            } else {
                Output::None
            };
            return (out, Vec::new());
        }

        // 2. Server calls, without the lock, all within `deadline`.
        let stashed = plan.stash.len();
        // Kept mail counts against the claim limit: what one hook shows is
        // bounded, and kept mail goes first.
        let limit = adapter.claim_limit().saturating_sub(stashed as u32);
        let via = format!("hook:{}", hook.name);
        let mut claimed: Vec<InboxEntry> = plan.stash;
        let mut digest: Vec<InboxEntry> = Vec::new();
        let mut failed = false;
        let mut more = false;
        use DeliveryClass::*;
        let spec = |classes: &[DeliveryClass], urgent_only: bool, digest: bool| ClaimSpec {
            classes: classes.to_vec(),
            urgent_only,
            digest,
            via: via.clone(),
            limit: if digest { DIGEST_LIMIT } else { limit },
        };
        let mut calls: Vec<ClaimSpec> = Vec::new();
        if limit == 0 {
            more = plan.claim_all || plan.claim_urgent_all || plan.claim_to || plan.claim_urgent;
        } else if plan.claim_all {
            calls.push(spec(&[To, Cc, List], false, false));
        } else if plan.claim_urgent_all {
            calls.push(spec(&[To, Cc, List], true, false));
        } else {
            if plan.claim_to {
                calls.push(spec(&[To], false, false));
            }
            if plan.claim_urgent {
                calls.push(spec(&[Cc, List], true, false));
            }
        }
        if plan.digest {
            calls.push(spec(&[To, Cc, List], false, true));
        }
        let made_calls = !calls.is_empty() || plan.summary || plan.tasks;
        // A server answer that came after the deadline: the mail is shown
        // one hook late.
        let mut timed_out = false;
        let mut keyed: Vec<(String, Vec<InboxEntry>)> = Vec::new();
        for spec in calls {
            let key = claim_key();
            let left = deadline.saturating_duration_since(Instant::now());
            let call = self.server.claim(seat, &spec, &key, Some(left));
            let err = match tokio::time::timeout_at(deadline.into(), call).await {
                Ok(Ok(r)) => {
                    keyed.push((key.clone(), r.entries.clone()));
                    if spec.digest {
                        digest.extend(r.entries);
                    } else {
                        more |= r.entries.len() as u32 >= limit;
                        claimed.extend(r.entries);
                    }
                    continue;
                }
                Ok(Err(e)) => {
                    timed_out |= Instant::now() >= deadline;
                    e
                }
                Err(_) => {
                    timed_out = true;
                    anyhow::anyhow!("hook deadline passed")
                }
            };
            tracing::debug!("claim for {seat}: {err:#}");
            failed = true;
            // The server may have made the claim: ask again with the same key
            // (it then answers with what it took) and keep the result.
            if !crate::server::is_permanent(&err) {
                self.retry_claim(seat, spec, key);
            }
        }
        // The unread summary and the seat's open tasks (one line), at the
        // same time; each shows nothing on an error or after the deadline.
        // The second value says whether the deadline passed.
        let summary_call = async {
            if !(plan.summary && !failed) {
                return (Vec::new(), false);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let call = self.server.unread(seat, SUMMARY_LIMIT, left);
            match tokio::time::timeout_at(deadline.into(), call).await {
                Ok(Ok(r)) => {
                    let seen: HashSet<i64> = claimed.iter().map(|e| e.message.id).collect();
                    // Mail still waiting to be notified is injected in full by
                    // the next claim; listing it here too would show it twice.
                    // Mail this session was shown before is not listed again.
                    let v = r
                        .entries
                        .into_iter()
                        .filter(|e| !seen.contains(&e.message.id))
                        .filter(|e| !plan.listed.contains(&e.message.id))
                        .filter(|e| {
                            e.delivery.notified_at.is_some()
                                || e.delivery.mode != amail_proto::DeliveryMode::Immediate
                        })
                        .collect();
                    (v, false)
                }
                _ => (Vec::new(), Instant::now() >= deadline),
            }
        };
        let tasks_call = async {
            if !(plan.tasks && !failed) {
                return (None, None, false);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let call = self.server.tasks_mine(seat, TASKS_LIMIT, left);
            match tokio::time::timeout_at(deadline.into(), call).await {
                Ok(Ok(r)) => {
                    // The task the seat holds, when it is doing just one.
                    let doing: Vec<i64> = r
                        .tasks
                        .iter()
                        .filter(|t| t.state == amail_proto::TaskState::Doing)
                        .map(|t| t.num)
                        .collect();
                    let doing = match doing.as_slice() {
                        [one] => Some(Some(*one)),
                        _ => Some(None),
                    };
                    (
                        crate::inject::tasks_summary(&r.tasks, r.truncated),
                        doing,
                        false,
                    )
                }
                Ok(Err(e)) => {
                    tracing::debug!("open tasks of {seat}: {e:#}");
                    (None, None, Instant::now() >= deadline)
                }
                Err(_) => (None, None, true),
            }
        };
        let ((summary, late1), (tasks, doing, late2)) = tokio::join!(summary_call, tasks_call);
        let (index_job, note_job) = spawn_jobs();
        // After the mail: at session start, wait a little for the index (when
        // nothing is cached) and the note; the rest of the deadline stays
        // the mail's. What misses comes with the seat's next hook.
        let index_wait = async {
            if let (true, Some(h)) = (plan.index_wait, index_job) {
                handover::wait_part(h, deadline, handover::INDEX_WAIT).await;
            }
        };
        let note_wait = async {
            if let (true, Some(h)) = (plan.note_wait, note_job) {
                handover::wait_part(h, deadline, handover::NOTE_WAIT).await;
            }
        };
        let check_call = async {
            if !plan.handover {
                return handover::StopCheck::default();
            }
            let now = Instant::now();
            let left = deadline.saturating_duration_since(now);
            if left < crate::server::HOOK_SERVER_TIMEOUT * 2 {
                return handover::StopCheck::default();
            }
            let turn_start = self.lock().seats.get(seat).and_then(|s| s.ho.turn_start);
            self.stop_check(seat, turn_start, over, now + left / 2)
                .await
        };
        let ((), (), check) = tokio::join!(index_wait, note_wait, check_call);
        timed_out |= late1 || late2;

        // 3. Apply the outcome.
        {
            let mut st = self.lock();
            for (key, entries) in &keyed {
                st.note_claimed(seat, key, entries);
            }
            if made_calls {
                st.note_hook(Instant::now(), timed_out);
            }
            if claimed.len() > stashed || !digest.is_empty() {
                let s = st.seat(seat);
                for e in claimed[stashed..].iter().chain(&digest) {
                    let p = &mut s.pending;
                    if e.delivery.mode == amail_proto::DeliveryMode::Digest {
                        p.digest = (p.digest - 1).max(0);
                    } else {
                        match e.delivery.class {
                            To => p.to = (p.to - 1).max(0),
                            Cc => p.cc = (p.cc - 1).max(0),
                            List => p.list = (p.list - 1).max(0),
                        }
                        if e.message.priority == Priority::Urgent {
                            p.urgent = (p.urgent - 1).max(0);
                        }
                    }
                    p.seq = p.seq.max(e.delivery.mod_seq);
                }
                st.dirty_pending.insert(seat.to_string());
                self.refresh_kick.notify_one();
            }
            {
                let ho = &mut st.seat(seat).ho;
                if let Some(d) = doing {
                    ho.doing = d;
                }
                if let Some(n) = check.note_at {
                    ho.note_at = Some(n);
                    ho.requested = check.requested;
                }
                // A request for the note arrives as system mail.
                if claimed.iter().any(handover::is_request) {
                    ho.requested = true;
                }
            }
            if hook.event == Event::Stop {
                if !(claimed.is_empty() && digest.is_empty()) {
                    st.seat(seat).stop_blocks += 1;
                } else if check.ask.is_some() {
                    // Once per turn; the next `Stop` ends the turn.
                    st.seat(seat).ho.asked = true;
                } else {
                    st.set_state(seat, SeatState::Idle);
                    self.turn_ended(&mut st, seat, turns::TurnEnd::Normal);
                }
                self.presence_kick.notify_one();
                self.wake_kick.notify_one();
            }
        }

        // Claimed mail is shown whole, as much as fits in the harness's limit
        // (leaving room for the digest and the summary); the rest is kept and
        // shown by the next hook. An entry is never cut, since mail with an
        // inlined body is marked read by the claim.
        let mut text = String::new();
        let mut kept = Vec::new();
        if !claimed.is_empty() {
            let reserve =
                if digest.is_empty() { 0 } else { 1000 } + if summary.is_empty() { 0 } else { 500 };
            let budget = adapter.inject_budget().saturating_sub(reserve);
            let (t, shown) = crate::inject::new_mail(seat, &claimed, more, budget);
            text = t;
            kept = claimed.split_off(shown);
        }
        if !digest.is_empty() {
            let rest = adapter.inject_budget().saturating_sub(text.len()).max(1000);
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&crate::inject::digest(seat, &digest, rest));
        }
        if !summary.is_empty() {
            let rest = adapter.inject_budget().saturating_sub(text.len()).max(500);
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&crate::inject::unread_summary(seat, &summary, rest));
        }
        if let Some(t) = tasks {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&t);
        }
        if check.ask.is_some() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(crate::inject::HANDOVER_ASK);
        }
        // What has arrived of the index and the note.
        let (index, note) = {
            let mut st = self.lock();
            let ho = &mut st.seat(seat).ho;
            let index = plan.index.and(ho.index.clone());
            let note = if plan.note {
                ho.note_fetched.clone()
            } else {
                None
            };
            (index, note)
        };
        // A `Stop` shows the index and the note only in an answer that
        // blocks anyway.
        let blocks = hook.event == Event::Stop && !text.is_empty();
        // The handover note goes first, within its share and what the mail
        // left; then the index, which is cut first.
        let budget = adapter.inject_budget();
        let mut note_text = None;
        let mut note_shown = None;
        if let Some(r) = note
            .as_ref()
            .filter(|_| hook.event != Event::Stop || blocks)
        {
            match &r.note {
                None => {
                    // No note to show.
                    let mut st = self.lock();
                    let ho = &mut st.seat(seat).ho;
                    ho.note_pending = false;
                    ho.note_fetched = None;
                }
                Some(n) => {
                    let room = handover::share(budget, handover::NOTE_SHARE)
                        .min(budget.saturating_sub(text.len() + 2));
                    match crate::inject::handover_note(n, now_ms(), room) {
                        Some((t, _)) => {
                            note_text = Some(t);
                            note_shown = Some(n.rev);
                            let mut st = self.lock();
                            let ho = &mut st.seat(seat).ho;
                            ho.note_pending = false;
                            ho.note_fetched = None;
                        }
                        None => tracing::debug!("seat {seat}: no room for the handover note"),
                    }
                }
            }
        }
        if let Some(ix) = &index
            && (hook.event != Event::Stop || blocks)
        {
            let hash = handover::text_hash(&ix.text);
            let seen = self.lock().seat(seat).ho.index_hash;
            if plan.index == Some(true) || seen != Some(hash) {
                let used = text.len() + note_text.as_ref().map_or(0, |t| t.len() + 2);
                let room = handover::share(budget, handover::INDEX_SHARE)
                    .min(budget.saturating_sub(used + 2));
                let shown = if ix.lines.is_empty() {
                    true
                } else if let Some(t) = crate::inject::docs_index(ix, room) {
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&t);
                    true
                } else {
                    false
                };
                if shown {
                    self.lock().seat(seat).ho.index_hash = Some(hash);
                }
            }
        }
        if let Some(n) = note_text {
            text = if text.is_empty() {
                n
            } else {
                format!("{n}\n\n{text}")
            };
        }
        if let (Some(rev), Some(session)) = (note_shown, hook.session_id.clone()) {
            let mut st = self.lock();
            st.jobs.push_back(handover::Job::Read {
                seat: seat.to_string(),
                session,
                rev,
            });
            drop(st);
            self.jobs_kick.notify_one();
        }
        let out = if text.is_empty() {
            // A wake-up whose mail is gone (shown by an earlier turn, or
            // read since); not when a claim failed or was cut short.
            if plan.wake_prompt && !failed && !more {
                Output::Context(crate::inject::NOTHING_PENDING.to_string())
            } else {
                Output::None
            }
        } else if hook.event == Event::Stop {
            Output::Block(text)
        } else {
            Output::Context(text)
        };
        if !kept.is_empty() {
            self.keep(seat, kept);
        }
        claimed.extend(digest);
        if out != Output::None {
            let mut st = self.lock();
            st.note_shown(seat, &claimed);
            let s = st.seat(seat);
            s.listed
                .extend(claimed.iter().chain(&summary).map(|e| e.message.id));
        }
        (out, claimed)
    }

    /// Retry a claim whose answer did not arrive, with the same key, in the
    /// background; what it took is shown with the seat's next hook.
    fn retry_claim(&self, seat: &str, spec: ClaimSpec, key: String) {
        let mut st = self.lock();
        st.saved.claim_retries.insert(
            key,
            crate::persist::ClaimRetry {
                seat: seat.to_string(),
                spec,
                since: now_ms(),
            },
        );
        st.save_dirty = true;
        drop(st);
        self.retry_kick.notify_one();
    }

    async fn retry_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_millis(100);
        loop {
            let todo: Vec<(String, crate::persist::ClaimRetry)> = self
                .lock()
                .saved
                .claim_retries
                .iter()
                .map(|(k, r)| (k.clone(), r.clone()))
                .collect();
            if todo.is_empty() {
                backoff = Duration::from_millis(100);
                self.retry_kick.notified().await;
                continue;
            }
            tokio::time::sleep(backoff).await;
            let mut failed = false;
            for (key, r) in todo {
                let res = self
                    .server
                    .claim(&r.seat, &r.spec, &key, Some(Duration::from_secs(10)))
                    .await;
                let done = match res {
                    Ok(resp) => {
                        if !resp.entries.is_empty() {
                            tracing::info!(
                                "seat {}: a claim whose answer was lost took {} message(s); \
                                 keeping them for the next hook",
                                r.seat,
                                resp.entries.len()
                            );
                            self.lock().note_claimed(&r.seat, &key, &resp.entries);
                            self.stash(&r.seat, resp.entries, false);
                            self.lock().dirty_pending.insert(r.seat.clone());
                            self.refresh_kick.notify_one();
                        }
                        true
                    }
                    Err(e) if crate::server::is_permanent(&e) => {
                        tracing::warn!("seat {}: giving up a claim retry: {e:#}", r.seat);
                        true
                    }
                    Err(e) => {
                        tracing::debug!("seat {}: claim retry: {e:#}", r.seat);
                        failed = true;
                        false
                    }
                };
                if done {
                    let mut st = self.lock();
                    st.saved.claim_retries.remove(&key);
                    st.save_dirty = true;
                }
            }
            backoff = if failed {
                (backoff * 2).min(Duration::from_secs(10))
            } else {
                Duration::from_millis(100)
            };
        }
    }

    // --- launch and credentials ---------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn launch(
        &self,
        seat: &str,
        cwd: &str,
        pid: u32,
        harness: Option<&str>,
        extra: &[String],
        pane: Option<u32>,
        mux_session: Option<String>,
        permission_mode: Option<PermissionMode>,
        git_access: Option<GitAccess>,
        dsh_jail: Option<bool>,
        session: Option<&str>,
    ) -> Result<LaunchPlan> {
        let managed = self.lock().saved.managed.get(seat).cloned();
        let harness = harness.or(managed.as_ref().map(|m| m.harness.as_str()));
        // `amail run --session`: the seat adopts an existing session. It is
        // checked before the server issues a new token for the seat when
        // the harness is known here.
        let claim = |h: Harness, id: &str| crate::wire::SessionClaim {
            seat: seat.to_string(),
            harness: h.as_str().to_string(),
            session: id.to_string(),
            cwd: cwd.to_string(),
        };
        let mut checked = None;
        if let (Some(id), Some(h)) = (session, harness.and_then(Harness::parse)) {
            let found = self
                .check_claim(&claim(h, id))
                .await
                .map_err(anyhow::Error::msg)?;
            checked = Some((h, found));
        }
        let mut info = self.server.launch(seat).await?;
        if info.seat != seat {
            bail!("the server answered for seat {:?}, not {seat:?}", info.seat);
        }
        // The permission mode is decided on this machine: the `amail run`
        // flag, else the team file of a supervised seat, else the harness's
        // normal prompting. The server's value is only reported.
        let mode = permission_mode
            .or(managed.as_ref().and_then(|m| m.permission_mode))
            .unwrap_or(PermissionMode::Default);
        let notice = (info.permission_mode == Some(PermissionMode::Unattended)
            && mode != PermissionMode::Unattended)
            .then(|| {
                format!(
                    "seat {seat}: the server's seat settings say permission_mode \
                     \"unattended\", but this machine did not choose it, so the \
                     session keeps the harness's permission prompts. To run it \
                     without prompts, set permission_mode in the team file and run \
                     amail up, or use amail run --permission-mode unattended."
                )
            });
        if let Some(n) = &notice {
            tracing::warn!("{n}");
        }
        info.permission_mode = Some(mode);
        // Git access, like the permission mode, is decided on this machine.
        let git_access = git_access
            .or(managed.as_ref().and_then(|m| m.git_access))
            .unwrap_or_default();
        // So is the jail of unattended dsh seats (on unless turned off).
        let dsh_jail = dsh_jail
            .or(managed.as_ref().and_then(|m| m.dsh_jail))
            .unwrap_or(true);
        if let Some(m) = info.model.take_if(|m| !valid_model(m)) {
            tracing::warn!("seat {seat}: ignoring the server's model {m:?}");
        }
        let name = harness
            .map(str::to_string)
            .or_else(|| info.harness.clone())
            .with_context(|| format!("seat {seat} has no harness; set one or pass --harness"))?;
        let h = Harness::parse(&name).with_context(|| format!("unknown harness {name:?}"))?;
        let mut adopt_notice = None;
        if let Some(id) = session {
            let found = match checked {
                Some((ch, found)) if ch == h => found,
                _ => self
                    .check_claim(&claim(h, id))
                    .await
                    .map_err(anyhow::Error::msg)?,
            };
            adopt_notice = found.warning;
        }
        self.ensure_prepared(h).await?;
        let nonce = hex::encode(rand::random::<[u8; 8]>());
        {
            let mut st = self.lock();
            let now = now_ms();
            st.saved.reservations.retain(|_, r| r.expires > now);
            if self.adapter(h).registers_sessions() {
                st.saved.reservations.insert(
                    nonce.clone(),
                    Reservation {
                        seat: seat.to_string(),
                        pid,
                        expires: now + RESERVATION_TTL_MS,
                    },
                );
            }
            st.saved.tokens.insert(seat.to_string(), info.token.clone());
            st.seat(seat).harness = Some(h);
            st.save_dirty = true;
            // Its pending counts, in case the node did not keep them.
            st.dirty_pending.insert(seat.to_string());
            // And its note's state, for `amail ps`.
            if st.want_note_state(seat, false) {
                self.jobs_kick.notify_one();
            }
        }
        self.refresh_kick.notify_one();
        // A supervised seat resumes its last session when it has one;
        // Claude Code sessions get their id up front. A session given with
        // `amail run --session` is adopted instead.
        let registers = self.adapter(h).registers_sessions();
        let mut managed = managed;
        if let Some(id) = session {
            let mut st = self.lock();
            if managed.is_some() {
                adopt::adopt_into(&mut st, seat, id, registers);
                managed = st.saved.managed.get(seat).cloned();
            } else if registers {
                adopt::register(&mut st, seat, id);
            }
        }
        let same_harness = managed
            .as_ref()
            .filter(|m| Harness::parse(&m.harness) == Some(h));
        let resume = session
            .map(str::to_string)
            .or_else(|| same_harness.and_then(|m| m.resume.clone()));
        // A session that existed before it became this seat, on its first
        // start as the seat.
        let adopted = session.is_some() || same_harness.is_some_and(|m| m.adopt_intro);
        let context_lost = managed.as_ref().is_some_and(|m| m.context_lost);
        let new_id = (resume.is_none() && h == Harness::Claude).then(supervise::uuid_v4);
        if registers && let Some(id) = &resume {
            // A resumed session sends no registration marker.
            let mut st = self.lock();
            if !st.saved.sessions.contains_key(id) {
                adopt::register(&mut st, seat, id);
            }
        }
        self.save_now();
        let plan = self
            .adapter(h)
            .launch(&LaunchCtx {
                info: &info,
                permission_mode: mode,
                git_access,
                dsh_jail,
                cwd,
                extra,
                nonce: &nonce,
                resume: resume.as_deref(),
                codex_effort: managed.as_ref().and_then(|m| m.codex_effort.as_deref()),
                new_session_id: new_id.as_deref(),
                context_lost,
                adopted: adopted && resume.is_some(),
            })
            .await?;
        {
            // The process that asked becomes the session: requests from it
            // and its child processes are tied to this seat.
            let mut st = self.lock();
            st.saved.roots.insert(
                seat.to_string(),
                persist::SessionRoot {
                    harness: h.as_str().to_string(),
                    pid,
                    pid_start: proc_start(pid),
                },
            );
            st.save_dirty = true;
        }
        // The adapter's notice (if any) follows the node's.
        let notice = [notice, adopt_notice, plan.notice.clone()]
            .into_iter()
            .flatten()
            .reduce(|a, b| format!("{a}\n{b}"));
        let plan = LaunchPlan { notice, ..plan };
        if managed.is_some() {
            let lazy = self.adapter(h).resume_is_lazy();
            let mut st = self.lock();
            supervise::launched(
                &mut st,
                seat,
                h,
                pid,
                proc_start(pid),
                pane.filter(|_| {
                    managed.as_ref().map(|m| m.mux_session.as_str()) == mux_session.as_deref()
                }),
                resume.as_deref(),
                new_id.as_deref(),
                lazy,
            );
            drop(st);
            self.presence_kick.notify_one();
            self.wake_kick.notify_one();
            self.sup_kick.notify_one();
        }
        Ok(plan)
    }

    fn credentials(&self, thread: &str) -> Option<Credentials> {
        let st = self.lock();
        let seat = st.saved.sessions.get(thread)?.seat.clone();
        let token = st.saved.tokens.get(&seat)?.clone();
        Some(Credentials {
            url: self.cfg.url.clone(),
            token,
            seat,
        })
    }

    fn status(&self) -> Value {
        let (policy, source) = (self.local_policy(), self.policy_source());
        // Looked up before taking the lock (this reads the file system).
        let harnesses: Vec<Value> = [
            ("claude", &self.cfg.claude_bin),
            ("codex", &self.cfg.codex_bin),
            ("dsh", &self.cfg.dsh_bin),
            ("zellij", &self.cfg.zellij_bin),
        ]
        .iter()
        .map(|(h, bin)| {
            json!({
                "harness": h,
                "bin": bin,
                "path": crate::paths::find_program(bin),
            })
        })
        .collect();
        let latency = self.server.latency();
        let budget = self.hook_budget();
        let st = self.lock();
        let now = Instant::now();
        let (hooks, misses) = st.hook_stats(now);
        let ms = |d: Duration| d.as_millis() as u64;
        let mut seats: Vec<Value> = st
            .seats
            .iter()
            .filter(|(_, s)| s.harness.is_some() || s.pending.total() > 0)
            .map(|(name, s)| {
                json!({
                    "seat": name,
                    "harness": s.harness.map(Harness::as_str),
                    "state": s.state,
                    "since": ts(s.since),
                    // Without session ids or wake targets: they name a
                    // session to the harness's own interfaces.
                    "session": s.session.as_ref().map(|x| json!({
                        "pid": x.pid.map(|p| p.0),
                    })),
                    "pending": {"to": s.pending.to, "cc": s.pending.cc, "list": s.pending.list, "urgent": s.pending.urgent},
                    "stashed": s.stash.len(),
                    "stop_blocks": s.stop_blocks,
                    "wake_due_ms": s.wake_due.map(|d| d.saturating_duration_since(now).as_millis() as u64),
                    "wake_unconfirmed": s.wake_out.map(|(_, n)| n),
                    "wake_failed": s.wake_failed,
                    "wake_retry_ms": s.wake_retry_at.map(|d| d.saturating_duration_since(now).as_millis() as u64),
                    "wakes": s.wakes,
                    "turn_failed": st.turn_failure(name, self.cfg.turn_fail_limit),
                    "recovery_unverified": s.restore_pending,
                    "quiet_recovery": turns::quiet_recovery_diagnostics(&st, name),
                    "handover_note": s.ho.note_word(),
                    // Lines of the cached document index (none: not fetched).
                    "index_lines": s.ho.index.as_ref().map(|i| i.lines.len()),
                    "note_pending": s.ho.note_pending,
                    "waiting_for": s.rq.waiting_for,
                })
            })
            .collect();
        seats.sort_by(|a, b| a["seat"].as_str().cmp(&b["seat"].as_str()));
        let mut mirrors: Vec<&String> = st.mirrors.keys().collect();
        mirrors.sort();
        json!({
            "connected": st.connected,
            "server": self.cfg.url,
            "uptime_s": self.started.elapsed().as_secs(),
            // Round trips to the server lately, the time a hook now waits
            // for it, and the hooks of the last 10 minutes that asked the
            // server for mail, with how many got no answer in time.
            "latency": {
                "rtt_ms": latency.last.map(ms),
                "rtt_max_ms": latency.max.map(ms),
                "samples": latency.samples,
                "hook_budget_ms": ms(budget),
                "hooks_10m": hooks,
                "hook_misses_10m": misses,
            },
            // Registrations of sessions that are running now (others are
            // kept only so a sleeping or stopped seat can resume).
            "registered_sessions": st
                .saved
                .sessions
                .values()
                .filter(|r| {
                    st.seats
                        .get(&r.seat)
                        .is_some_and(|s| !matches!(s.state, SeatState::Offline | SeatState::Dormant))
                })
                .count(),
            "mirrors": mirrors,
            "seats": seats,
            "local_policy": policy,
            "local_policy_source": source,
            // The programs this node starts for each harness, and where it
            // finds them (null: not found), for `amail node status` and
            // `amail doctor`.
            "harnesses": harnesses,
            // Names only (the values are secrets): what dsh hosts get from
            // `--dsh-env-file`.
            "dsh_env": self.cfg.dsh_env.keys().collect::<Vec<_>>(),
        })
    }

    // --- server sync ----------------------------------------------------------

    fn apply_pending(&self, st: &mut State, resp: amail_proto::PendingResponse) {
        if let Some(d) = self.cfg.digest_every {
            st.digest_every = d.as_millis() as i64;
        } else if let Some(ms) = resp.digest_interval_ms {
            st.digest_every = ms as i64;
        }
        for c in resp.seats {
            let name = c.seat.clone();
            let Some(seat) = st.server_seat(&name) else {
                continue;
            };
            if c.seq < seat.pending.seq {
                continue;
            }
            // New mail that could wake the seat ends a wake-up failure.
            let arrived = c.to > seat.pending.to || c.urgent > seat.pending.urgent;
            let prev_seq = seat.pending.seq;
            let new_seq = c.seq;
            seat.digest_since = c.digest_since.as_deref().and_then(parse_ts);
            seat.focus_until = c.focus_until.as_deref().and_then(parse_ts);
            seat.pending = c;
            if arrived && seat.wake_failed {
                seat.clear_wake_failure();
                st.dirty_presence.insert(name.clone());
            }
            // New mail ends a sleep the person asked for. The counts it is
            // compared with must be from after the sleep (not the empty ones
            // of a node that just started).
            if arrived
                && let Some(m) = st.saved.managed.get_mut(&name)
                && m.held
                    .is_some_and(|held| prev_seq >= held && new_seq > held)
            {
                tracing::info!("seat {name}: new mail; it may be started again");
                m.held = None;
                st.save_dirty = true;
                self.sup_kick.notify_one();
            }
        }
    }

    async fn events_loop(self: Arc<Self>) {
        let mut last_id: Option<String> = None;
        let mut backoff = Duration::from_millis(250);
        loop {
            match self.server.events(last_id.as_deref()).await {
                Ok(stream) => {
                    futures::pin_mut!(stream);
                    // Subscribed first, so nothing after this snapshot is missed.
                    match self.server.pending(None).await {
                        Ok(p) => {
                            let mut st = self.lock();
                            self.apply_pending(&mut st, p);
                            st.connected = true;
                            st.pending_refused.clear();
                            // The server (or another instance) may know
                            // nothing about the seats: report them all.
                            let all: Vec<String> = st
                                .seats
                                .iter()
                                .filter(|(_, s)| s.harness.is_some())
                                .map(|(n, _)| n.clone())
                                .collect();
                            // `amail ps` shows whether each note is current
                            // from the start, not after the next check.
                            for seat in &all {
                                st.want_note_state(seat, false);
                            }
                            self.jobs_kick.notify_one();
                            st.dirty_presence.extend(all);
                            backoff = Duration::from_millis(250);
                            tracing::info!("connected to {}", self.cfg.url);
                        }
                        Err(e) => {
                            tracing::warn!("fetching pending counts: {e:#}");
                            tokio::time::sleep(backoff).await;
                            continue;
                        }
                    }
                    self.wake_kick.notify_one();
                    self.presence_kick.notify_one();
                    use futures::StreamExt;
                    while let Some(ev) = stream.next().await {
                        let Ok(ev) = ev else { break };
                        if ev.event == "command" {
                            match serde_json::from_str::<amail_proto::NodeCommand>(&ev.data) {
                                Ok(cmd) => self.on_command(cmd),
                                Err(e) => tracing::debug!("ignoring a command: {e}"),
                            }
                            continue;
                        }
                        if ev.event != "mailbox" {
                            continue;
                        }
                        if let Some(id) = &ev.id {
                            last_id = Some(id.clone());
                        }
                        let Ok(m) = serde_json::from_str::<MailboxEvent>(&ev.data) else {
                            continue;
                        };
                        let mut st = self.lock();
                        if st.pending_refused.contains(&m.seat) {
                            continue;
                        }
                        if st
                            .server_seat(&m.seat)
                            .is_some_and(|s| m.seq > s.pending.seq)
                        {
                            st.dirty_pending.insert(m.seat);
                            self.refresh_kick.notify_one();
                        }
                    }
                    tracing::warn!("event stream from {} ended", self.cfg.url);
                }
                Err(e) => tracing::warn!("connecting to {}: {e:#}", self.cfg.url),
            }
            self.lock().connected = false;
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }

    /// Refresh the pending counts of seats whose mailbox changed, and every
    /// [`RESYNC`] all of them (an event stream can miss events: a server
    /// instance that died before telling the others, a dropped connection).
    /// Seats whose refresh failed are tried again.
    async fn refresh_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_millis(200);
        loop {
            let full = tokio::select! {
                _ = self.refresh_kick.notified() => {
                    // Mailbox events come in bursts (a message to a list,
                    // a claim): refresh them together.
                    tokio::time::sleep(REFRESH_BATCH).await;
                    false
                }
                _ = tokio::time::sleep(RESYNC) => true,
            };
            let connected = self.lock().connected;
            if full && !connected {
                continue;
            }
            let mut first = true;
            loop {
                let dirty: Vec<String> = std::mem::take(&mut self.lock().dirty_pending)
                    .into_iter()
                    .collect();
                if dirty.is_empty() && !(full && first) {
                    break;
                }
                // One query answers for the whole org about as fast as for
                // one seat.
                let whole = (full && first) || dirty.len() > 4;
                first = false;
                let results: Vec<Result<amail_proto::PendingResponse>> = if whole {
                    vec![self.server.pending(None).await]
                } else {
                    futures::future::join_all(dirty.iter().map(|s| self.server.pending(Some(s))))
                        .await
                };
                let failed = {
                    let mut st = self.lock();
                    let (retry, refused) = after_refresh(whole, &dirty, &results);
                    for r in results {
                        match r {
                            Ok(p) => self.apply_pending(&mut st, p),
                            Err(e) => {
                                tracing::debug!("refreshing pending counts: {e:#}");
                            }
                        }
                    }
                    for seat in refused {
                        tracing::debug!(
                            "seat {seat}: the server does not give this node its pending \
                             counts; not asked again until the node reconnects"
                        );
                        st.pending_refused.insert(seat);
                    }
                    let failed = !retry.is_empty();
                    st.dirty_pending.extend(retry);
                    failed
                };
                self.wake_kick.notify_one();
                self.presence_kick.notify_one();
                if failed {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                } else {
                    backoff = Duration::from_millis(200);
                }
            }
        }
    }

    // --- wake-ups -------------------------------------------------------------

    async fn wake_loop(self: Arc<Self>) {
        loop {
            let next = self.wake_scan();
            let sleep = match next {
                Some(t) => tokio::time::sleep_until(t.into()),
                None => tokio::time::sleep(Duration::from_secs(3600)),
            };
            tokio::select! {
                _ = self.wake_kick.notified() => {}
                _ = sleep => {}
            }
        }
    }

    /// Fire due wake-ups; return the next deadline.
    ///
    /// A wake-up not confirmed by a `UserPromptSubmit` within `wake_confirm`
    /// is sent once more; if that is not confirmed either, the seat is marked
    /// `wake_failed` and tried again on a backoff, or at once when new mail
    /// arrives for it.
    fn wake_scan(self: &Arc<Self>) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut soonest = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        let mut fire: Vec<(Harness, String, String, u64)> = Vec::new();
        let mut st = self.lock();
        let connected = st.connected;
        let digest_every = st.digest_every;
        let now_wall = now_ms();
        let mut counter = st.saved.wake_counter;
        let mut changed = Vec::new();
        let (confirm, window) = (self.cfg.wake_confirm, self.cfg.wake_window);
        let (backoff0, backoff_max) = (self.cfg.wake_backoff, self.cfg.wake_backoff_max);
        // Seats that may be woken by typing into their pane.
        let paste_ok: HashMap<String, (String, u32)> = st
            .saved
            .managed
            .iter()
            .filter(|(_, m)| m.paste_fallback)
            .filter_map(|(n, m)| Some((n.clone(), (m.mux_session.clone(), m.pane?))))
            .collect();
        let mut pastes: Vec<(String, String, u32, u64)> = Vec::new();
        // Seats whose turns failed too often are not woken for mail.
        let exhausted: HashSet<String> = st
            .saved
            .turns
            .keys()
            .filter(|n| st.turns_exhausted(n, self.cfg.turn_fail_limit))
            .cloned()
            .collect();
        for (name, s) in st.seats.iter_mut() {
            let target = s.session.as_ref().and_then(|x| x.target.clone());
            // Mail held by focus is due when the focus ends; digest mail
            // when it has waited an interval.
            if connected && s.state == SeatState::Idle {
                if let Some(t) = s.focus_until.filter(|t| *t > now_wall)
                    && (s.pending.to > 0 || s.pending.digest > 0)
                {
                    soonest(now + Duration::from_millis((t - now_wall) as u64 + 50));
                }
                if s.pending.digest > 0
                    && let Some(since) = s.digest_since
                {
                    let at = (since + digest_every).max(s.last_digest_wake + digest_every);
                    if at > now_wall {
                        soonest(now + Duration::from_millis((at - now_wall) as u64 + 50));
                    }
                }
            }
            // After a failed turn, wait for its backoff.
            let held = match s.turn_retry_at {
                Some(t) if t > now => {
                    if s.state == SeatState::Idle && s.wants_wake(now_wall, digest_every) {
                        soonest(t);
                    }
                    true
                }
                _ => false,
            };
            let eligible = connected
                && s.state == SeatState::Idle
                && target.is_some()
                && !held
                && !exhausted.contains(name)
                && s.wants_wake(now_wall, digest_every);
            let (Some(h), Some(target)) = (s.harness, target.filter(|_| eligible)) else {
                s.wake_out = None;
                s.wake_due = None;
                continue;
            };
            if let Some((sent, attempt)) = s.wake_out {
                if now < sent + confirm {
                    soonest(sent + confirm);
                    continue;
                }
                if attempt < 2 {
                    counter += 1;
                    fire.push((h, name.clone(), target, counter));
                    s.wake_out = Some((now, attempt + 1));
                    s.wakes += 1;
                    soonest(now + confirm);
                } else if attempt == 2
                    && let Some((session, pane)) = paste_ok.get(name)
                {
                    // Native wake-ups failed: try typing the nudge (it is
                    // checked against the screen first).
                    counter += 1;
                    pastes.push((name.clone(), session.clone(), *pane, counter));
                    s.wake_out = Some((now, 3));
                    s.wakes += 1;
                    soonest(now + confirm);
                } else {
                    s.wake_out = None;
                    s.wake_backoff = if s.wake_backoff.is_zero() {
                        backoff0
                    } else {
                        (s.wake_backoff * 2).min(backoff_max)
                    };
                    s.wake_retry_at = Some(now + s.wake_backoff);
                    soonest(now + s.wake_backoff);
                    if !s.wake_failed {
                        tracing::warn!(
                            "seat {name}: wake-up not confirmed after {attempt} attempts"
                        );
                        s.wake_failed = true;
                        changed.push(name.clone());
                    }
                }
                continue;
            }
            if s.wake_failed {
                match s.wake_retry_at {
                    Some(t) if now < t => soonest(t),
                    _ => {
                        // One attempt per backoff step.
                        counter += 1;
                        fire.push((h, name.clone(), target, counter));
                        s.wake_out = Some((now, 2));
                        s.wakes += 1;
                        soonest(now + confirm);
                    }
                }
                continue;
            }
            let urgent = s.pending.urgent > 0 || !s.stash.is_empty();
            let due = s
                .wake_due
                .get_or_insert(if urgent { now } else { now + window });
            if urgent && *due > now {
                *due = now;
            }
            if now >= *due {
                counter += 1;
                fire.push((h, name.clone(), target, counter));
                if s.digest_due(now_wall, digest_every) {
                    s.last_digest_wake = now_wall;
                }
                s.wake_due = None;
                s.wake_out = Some((now, 1));
                s.wakes += 1;
                soonest(now + confirm);
            } else {
                soonest(*due);
            }
        }
        if !changed.is_empty() {
            st.dirty_presence.extend(changed);
            self.presence_kick.notify_one();
        }
        if counter != st.saved.wake_counter {
            st.saved.wake_counter = counter;
            st.save_dirty = true;
        }
        // Per-seat wake counts survive restarts with the sequence.
        for (_, name, _, _) in &fire {
            let n = st.seats.get(name).map_or(0, |s| s.wakes);
            st.saved.wakes.insert(name.clone(), n);
        }
        drop(st);
        for (seat, session, pane, n) in pastes {
            let me = self.clone();
            tokio::spawn(async move { me.paste_wake(&seat, &session, pane, n).await });
        }
        for (h, seat, target, n) in fire {
            let adapter = self.adapter(h).clone();
            tracing::info!("waking seat {seat} (#{n})");
            tokio::spawn(async move {
                if let Err(e) = adapter.wake(&target, &crate::inject::wake_text(n)).await {
                    tracing::warn!("waking seat {seat}: {e:#}");
                }
            });
        }
        next
    }

    /// Type the wake-up into the seat's pane, if the screen shows an idle
    /// harness at an empty input box (see [`crate::paste`]); otherwise, or
    /// if anything is off, the seat is marked `wake_failed` at once.
    async fn paste_wake(self: Arc<Self>, seat: &str, session: &str, pane: u32, n: u64) {
        let text = crate::inject::wake_text(n);
        let r = async {
            let idle = self.lock().seats.get(seat).is_some_and(|s| {
                s.state == SeatState::Idle && s.wake_out.is_some_and(|w| w.1 == 3)
            });
            if !idle {
                bail!("the seat is no longer idle");
            }
            let screen = self.mux.screen(session, pane).await?;
            crate::paste::check(&screen).map_err(anyhow::Error::msg)?;
            self.mux.paste(session, pane, &text).await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            let screen = self.mux.screen(session, pane).await?;
            crate::paste::check_typed(&screen, &text).map_err(anyhow::Error::msg)?;
            self.mux.enter(session, pane).await
        }
        .await;
        match r {
            Ok(()) => tracing::info!("seat {seat}: typed the wake-up (#{n}) into its pane"),
            Err(e) => {
                tracing::warn!("seat {seat}: not typing a wake-up into its pane: {e:#}");
                let mut st = self.lock();
                let (b0, bmax) = (self.cfg.wake_backoff, self.cfg.wake_backoff_max);
                let s = st.seat(seat);
                if s.wake_out.is_some_and(|w| w.1 == 3) {
                    s.wake_out = None;
                    s.wake_backoff = if s.wake_backoff.is_zero() {
                        b0
                    } else {
                        (s.wake_backoff * 2).min(bmax)
                    };
                    s.wake_retry_at = Some(Instant::now() + s.wake_backoff);
                    if !s.wake_failed {
                        s.wake_failed = true;
                        st.dirty_presence.insert(seat.to_string());
                    }
                }
                drop(st);
                self.presence_kick.notify_one();
                self.wake_kick.notify_one();
            }
        }
    }

    // --- presence and liveness -------------------------------------------------

    async fn presence_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(self.cfg.heartbeat);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let full = tokio::select! {
                _ = self.presence_kick.notified() => {
                    // Batch changes that arrive together (a turn of a seat
                    // is busy and idle again within a fraction of a second).
                    tokio::time::sleep(PRESENCE_BATCH).await;
                    false
                }
                _ = tick.tick() => true,
            };
            let (report, sent): (Vec<PresenceUpdate>, Vec<String>) = {
                let mut st = self.lock();
                if !st.connected {
                    continue;
                }
                let mut dirty = std::mem::take(&mut st.dirty_presence);
                // Needing a person comes from several places (screens,
                // crashes, permission prompts); report its changes.
                let names_all: Vec<String> = st.seats.keys().cloned().collect();
                let mut attention = HashMap::new();
                for name in names_all {
                    let a = supervise::attention_of(&st, &name);
                    if st
                        .seats
                        .get(&name)
                        .is_some_and(|s| s.reported_attention != a)
                    {
                        dirty.insert(name.clone());
                    }
                    attention.insert(name, a);
                }
                let web = self.cfg.web_terminal.clone();
                let failures: HashMap<String, amail_proto::TurnFailure> = st
                    .saved
                    .turns
                    .keys()
                    .filter_map(|n| {
                        st.turn_failure(n, self.cfg.turn_fail_limit)
                            .map(|f| (n.clone(), f))
                    })
                    .collect();
                let sessions: HashMap<String, String> = st
                    .saved
                    .managed
                    .iter()
                    .map(|(n, m)| (n.clone(), m.mux_session.clone()))
                    .collect();
                let mut out = Vec::new();
                let mut names = Vec::new();
                for (name, s) in st.seats.iter_mut() {
                    let include = dirty.contains(name)
                        || (full && (s.state != SeatState::Offline || !s.reported));
                    if !include || s.harness.is_none() {
                        continue;
                    }
                    s.reported = s.state != SeatState::Offline;
                    let a = attention.remove(name).flatten();
                    s.reported_attention = a.clone();
                    let (tag, hint) = presence_attention(a);
                    names.push(name.clone());
                    out.push(PresenceUpdate {
                        seat: name.clone(),
                        state: s.state,
                        wake_failed: s.wake_failed,
                        harness: s.harness.map(|h| h.as_str().to_string()),
                        since: Some(ts(s.since)),
                        attention: tag,
                        attention_hint: hint,
                        terminal_url: match (&web, sessions.get(name)) {
                            (Some(w), Some(session)) => {
                                Some(format!("{}/{}", w.base_url.trim_end_matches('/'), session))
                            }
                            _ => None,
                        },
                        turn_failed: failures.get(name).cloned(),
                        waiting_for: s.rq.waiting_for,
                    });
                }
                (out, names)
            };
            if report.is_empty() {
                continue;
            }
            for chunk in report.chunks(1000) {
                let r = PresenceReport {
                    node: self.cfg.label.clone(),
                    seats: chunk.to_vec(),
                };
                if let Err(e) = self.server.presence(&r).await {
                    tracing::debug!("reporting presence: {e:#}");
                    self.lock().dirty_presence.extend(sent.iter().cloned());
                    break;
                }
            }
        }
    }

    async fn liveness_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(self.cfg.liveness_interval);
        loop {
            tick.tick().await;
            let mut st = self.lock();
            let managed: HashSet<String> = st.saved.managed.keys().cloned().collect();
            let gone: Vec<String> = st
                .seats
                .iter()
                .filter(|(n, _)| !managed.contains(*n))
                .filter(|(_, s)| s.state != SeatState::Offline)
                .filter(|(_, s)| {
                    s.session
                        .as_ref()
                        .and_then(|x| x.pid)
                        .is_some_and(|(pid, start)| !alive(pid, start))
                })
                .map(|(n, _)| n.clone())
                .collect();
            for name in gone {
                tracing::info!("seat {name}: session process exited");
                let busy = st
                    .seats
                    .get(&name)
                    .is_some_and(|s| s.state == SeatState::Busy);
                st.set_state(&name, SeatState::Offline);
                let end = if busy {
                    turns::TurnEnd::Failed("the session process ended during the turn".into())
                } else {
                    turns::TurnEnd::Person
                };
                self.turn_ended(&mut st, &name, end);
            }
            if !st.dirty_presence.is_empty() {
                self.presence_kick.notify_one();
            }
        }
    }
}

/// Where the node's `zellij web` server keeps its login tokens
/// (`$XDG_DATA_HOME/zellij/tokens.db` under this directory), apart from the
/// user's own.
pub fn web_terminal_data_dir(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("zellij-web")
}

/// A `zellij web` command with the node's token store.
pub fn web_terminal_command(zellij: &str, state_dir: &std::path::Path) -> std::process::Command {
    let mut c = std::process::Command::new(zellij);
    crate::mux::scrub_env(&mut c);
    c.env("XDG_DATA_HOME", web_terminal_data_dir(state_dir));
    c
}

/// Run the node's `zellij web` server on 127.0.0.1:`port` while the node
/// runs, restarting it if it stops. It is the node's own process (never
/// `zellij web --stop`, which would stop every server of the user).
async fn web_terminal_loop(zellij: String, state_dir: PathBuf, port: u16) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let _ = std::fs::create_dir_all(web_terminal_data_dir(&state_dir));
        let mut c = tokio::process::Command::from(web_terminal_command(&zellij, &state_dir));
        c.args(["web", "--start", "--port", &port.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // Stopped with the node: `kill_on_drop` when the task ends, and the
        // parent-death signal if the node process dies.
        // SAFETY: prctl is async-signal-safe and touches no memory.
        #[cfg(target_os = "linux")]
        unsafe {
            c.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let started = Instant::now();
        match c.spawn() {
            Ok(child) => {
                tracing::info!("serving terminals with zellij web on 127.0.0.1:{port}");
                match child.wait_with_output().await {
                    Ok(out) => tracing::warn!(
                        "zellij web exited ({}): {}",
                        out.status,
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                    Err(e) => tracing::warn!("zellij web: {e}"),
                }
            }
            Err(e) => tracing::warn!("starting zellij web: {e}"),
        }
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// Ask a running node (as `amail run` and the CLI do).
pub fn request(socket: &std::path::Path, req: &Request, timeout: Duration) -> Result<Value> {
    use std::io::{BufRead, Write};
    let mut s = crate::paths::connect_node(socket).with_context(|| {
        format!(
            "connecting to the amail node at {} (is `amail node` running?)",
            socket.display()
        )
    })?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(req)?;
    line.push(b'\n');
    s.write_all(&line)?;
    let mut resp = String::new();
    std::io::BufReader::new(s).read_line(&mut resp)?;
    let v: Value = serde_json::from_str(&resp).context("bad response from node")?;
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        bail!("{e}");
    }
    Ok(v)
}

/// Bound the complete wire hint, including text added around a saved failure.
/// Keep the saved reason and the local diagnostic view unchanged.
fn presence_attention(attention: Option<(String, String)>) -> (Option<String>, Option<String>) {
    let (tag, hint) = attention.unzip();
    (
        tag,
        hint.map(|value| {
            value
                .chars()
                .take(amail_proto::limits::MAX_HINT_CHARS)
                .collect()
        }),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn restored_session_holds_all_automatic_wake_classes() {
        let mut s = super::SeatRt::new("cx");
        s.restore_pending = true;
        s.pending.to = 1;
        s.pending.urgent = 1;
        s.pending.digest = 1;
        s.digest_since = Some(0);
        assert!(!s.wants_wake(1000, 10));
        s.restore_pending = false;
        assert!(s.wants_wake(1000, 10));
    }

    #[test]
    fn presence_attention_bounds_formatted_failures_in_a_mixed_batch() {
        use amail_proto::limits::{MAX_HINT_CHARS, check_chars};

        let mut st = super::State {
            turn_fail_limit: 3,
            ..Default::default()
        };
        st.seat("reference").state = amail_proto::SeatState::Offline;
        st.seat("healthy").state = amail_proto::SeatState::Busy;
        let reason = super::turns::clean_reason(&"error ".repeat(150));
        let failure = st.saved.turns.entry("reference".into()).or_default();
        failure.failures = 3;
        failure.reason = Some(reason.clone());

        let raw = super::supervise::attention_of(&st, "reference").unwrap();
        assert!(check_chars("attention_hint", &raw.1, MAX_HINT_CHARS).is_err());
        let batch: Vec<_> = ["reference", "healthy"]
            .into_iter()
            .map(|name| {
                (
                    name,
                    super::presence_attention(super::supervise::attention_of(&st, name)),
                )
            })
            .collect();
        for (_, (_, hint)) in &batch {
            if let Some(hint) = hint {
                check_chars("attention_hint", hint, MAX_HINT_CHARS).unwrap();
            }
        }
        assert_eq!(batch[0].1.0.as_deref(), Some("turn-failed"));
        assert_eq!(batch[1].1, (None, None));
        assert_eq!(st.seats["healthy"].state, amail_proto::SeatState::Busy);
        assert_eq!(
            st.saved.turns["reference"].reason.as_deref(),
            Some(reason.as_str())
        );
        assert_eq!(st.saved.turns["reference"].failures, 3);
        assert_eq!(super::supervise::attention_of(&st, "reference"), Some(raw));
    }

    #[test]
    fn presence_attention_limits_characters_without_damaging_utf8() {
        use amail_proto::limits::{MAX_HINT_CHARS, check_chars};

        assert_eq!(super::presence_attention(None), (None, None));
        for text in ["", "a", "界", "🧪"] {
            for count in [MAX_HINT_CHARS - 1, MAX_HINT_CHARS, MAX_HINT_CHARS + 1] {
                let input = text.repeat(count);
                let (tag, hint) = super::presence_attention(Some(("login".into(), input.clone())));
                assert_eq!(tag.as_deref(), Some("login"));
                let hint = hint.unwrap();
                check_chars("attention_hint", &hint, MAX_HINT_CHARS).unwrap();
                assert_eq!(
                    hint.chars().count(),
                    input.chars().count().min(MAX_HINT_CHARS)
                );
                assert!(input.starts_with(&hint));
                if input.chars().count() <= MAX_HINT_CHARS {
                    assert_eq!(hint, input);
                }
            }
        }
    }

    #[test]
    fn activity_is_restored_from_the_conversation_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("session.jsonl");
        std::fs::write(&log, "{}\n").unwrap();
        let mut st = super::State::default();
        st.seat("w1");
        st.seat("w2");
        st.seat("busy").last_activity = 42;
        for seat in ["w1", "busy", "gone"] {
            st.saved.transcripts.insert(
                seat.into(),
                crate::persist::TranscriptRef {
                    harness: "claude".into(),
                    path: log.to_string_lossy().into_owned(),
                    session_id: None,
                    base: None,
                },
            );
        }
        super::restore_activity(&mut st);
        let now = super::now_ms();
        let w1 = st.seats["w1"].last_activity;
        assert!(w1 > now - 60_000 && w1 <= now, "{w1} vs {now}");
        // No log: unknown; a seat with activity keeps it; no new seats.
        assert_eq!(st.seats["w2"].last_activity, 0);
        assert_eq!(st.seats["busy"].last_activity, 42);
        assert!(!st.seats.contains_key("gone"));
    }

    #[test]
    fn ts() {
        assert_eq!(super::ts(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::ts(1_791_050_365_210), "2026-10-03T17:59:25.210Z");
    }
}
