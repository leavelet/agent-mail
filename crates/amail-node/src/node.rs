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

use crate::adapter::{Adapter, AdapterEnv, Event, Harness, Hook, LaunchCtx, Output};
use crate::persist::{self, LiveSession, Reservation, Saved, SessionReg};
use crate::server::ServerClient;
use crate::wire::{Credentials, LaunchPlan, Request};
use amail_proto::{
    DeliveryClass, InboxEntry, MailboxEvent, PendingCounts, PresenceReport, PresenceUpdate,
    Priority, SeatState,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, OnceCell};

mod supervise;

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
    pub heartbeat: Duration,
    pub liveness_interval: Duration,
    /// Total time of the server calls one hook waits for (claims, inbox).
    /// Must stay well below the hook client's wait (300 ms), which is well
    /// below the harness's hook timeout (3-5 s); see [`crate::hook_client`].
    pub hook_server_timeout: Duration,
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
    /// SIGTERM, then SIGKILL after this long.
    pub kill_grace: Duration,
    /// First restart delay after a crash; doubles per consecutive crash.
    pub restart_backoff: Duration,
    /// Restarts after consecutive crashes before giving up.
    pub max_restarts: u32,
    /// A run that lasted this long resets the crash count.
    pub stable_after: Duration,
    /// A resumed session that dies this soon without a hook means the
    /// resume failed: the next start is a new session.
    pub resume_grace: Duration,
    pub supervise_interval: Duration,
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
            codex_auth: crate::paths::home().join(".codex").join("auth.json"),
            wake_window: Duration::from_secs(3),
            wake_confirm: Duration::from_secs(10),
            wake_backoff: Duration::from_secs(60),
            wake_backoff_max: Duration::from_secs(600),
            stop_block_cap: 3,
            heartbeat: Duration::from_secs(30),
            liveness_interval: Duration::from_secs(2),
            hook_server_timeout: crate::server::HOOK_SERVER_TIMEOUT,
            zellij_bin: "zellij".into(),
            pane_cols: 200,
            pane_rows: 60,
            start_limit: 4,
            start_timeout: Duration::from_secs(60),
            kill_grace: Duration::from_secs(10),
            restart_backoff: Duration::from_secs(2),
            max_restarts: 3,
            stable_after: Duration::from_secs(600),
            resume_grace: Duration::from_secs(30),
            supervise_interval: Duration::from_millis(500),
        }
    }
}

const RESERVATION_TTL_MS: i64 = 15 * 60 * 1000;
const SUMMARY_LIMIT: u32 = 20;
/// An answer that would reach the hook client less than this before its
/// deadline is kept for the next hook instead of being sent.
const LATE_MARGIN: Duration = Duration::from_millis(30);

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
    /// Last hook or launch (Unix ms).
    last_activity: i64,
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
            last_activity: 0,
        }
    }

    fn wants_wake(&self) -> bool {
        self.pending.to > 0 || self.pending.urgent > 0 || !self.stash.is_empty()
    }

    fn clear_wake_failure(&mut self) {
        self.wake_failed = false;
        self.wake_retry_at = None;
        self.wake_backoff = Duration::ZERO;
    }
}

#[derive(Default)]
struct State {
    connected: bool,
    seats: HashMap<String, SeatRt>,
    saved: Saved,
    dirty_pending: HashSet<String>,
    dirty_presence: HashSet<String>,
    /// `saved` changed and must be written.
    save_dirty: bool,
    /// Supervisor state of supervised seats (not persisted).
    sup: HashMap<String, supervise::SupRt>,
    /// Supervised seats waiting for a start slot, in order.
    start_queue: std::collections::VecDeque<String>,
}

impl State {
    fn seat(&mut self, name: &str) -> &mut SeatRt {
        self.seats
            .entry(name.to_string())
            .or_insert_with(|| SeatRt::new(name))
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
    /// Show unread headers (nothing claimed).
    summary: bool,
    /// Mail claimed earlier whose injection was lost.
    stash: Vec<InboxEntry>,
}

impl Plan {
    fn none(&self) -> bool {
        !(self.claim_all || self.claim_to || self.claim_urgent || self.summary)
            && self.stash.is_empty()
    }
}

pub struct Node {
    cfg: Config,
    server: ServerClient,
    mux: Arc<dyn crate::mux::Mux>,
    sup_kick: Notify,
    claude: Arc<dyn Adapter>,
    codex: Arc<dyn Adapter>,
    prepared: HashMap<Harness, Arc<OnceCell<Result<(), String>>>>,
    st: Mutex<State>,
    state_file: PathBuf,
    wake_kick: Notify,
    presence_kick: Notify,
    refresh_kick: Notify,
    started: Instant,
}

/// A running node. Dropping it does not stop it; call [`NodeHandle::shutdown`].
pub struct NodeHandle {
    pub node: Arc<Node>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl NodeHandle {
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
    let mux = Arc::new(crate::mux::Zellij::new(
        &cfg.zellij_bin,
        &cfg.state_dir,
        cfg.pane_cols,
        cfg.pane_rows,
    ));
    start_with_mux(cfg, mux).await
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
    if cfg.socket.exists() {
        if tokio::net::UnixStream::connect(&cfg.socket).await.is_ok() {
            bail!("a node is already listening on {}", cfg.socket.display());
        }
        std::fs::remove_file(&cfg.socket)?;
    }
    let listener = tokio::net::UnixListener::bind(&cfg.socket)
        .with_context(|| format!("binding {}", cfg.socket.display()))?;

    let env = AdapterEnv {
        state_dir: cfg.state_dir.clone(),
        amail_exe: cfg.amail_exe.clone(),
        node_socket: cfg.socket.clone(),
        server_url: cfg.url.clone(),
        claude_bin: cfg.claude_bin.clone(),
        codex_bin: cfg.codex_bin.clone(),
        codex_auth: cfg.codex_auth.clone(),
    };
    let claude: Arc<dyn Adapter> = Arc::new(crate::adapter::claude::Claude::new(env.clone()));
    let codex: Arc<dyn Adapter> = Arc::new(crate::adapter::codex::Codex::new(env));
    let state_file = cfg.state_dir.join("node-state.json");
    let mut st = State {
        saved: persist::load(&state_file),
        ..Default::default()
    };
    // Live sessions survive restarts. One whose process (and wake target) is
    // still there is assumed idle: a wake-up that reaches a busy session is
    // harmless (it is handled after the current turn).
    for (seat, l) in st.saved.live.clone() {
        let adapter = match Harness::parse(&l.harness) {
            Some(Harness::Claude) => &claude,
            Some(Harness::Codex) => &codex,
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
        s.state = SeatState::Idle;
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
    supervise::restore(&mut st, |h| match Harness::parse(h) {
        Some(Harness::Claude) => Some(claude.harness()),
        Some(Harness::Codex) => Some(codex.harness()),
        None => None,
    });
    let node = Arc::new(Node {
        server: ServerClient::new(&cfg.url, &cfg.token, cfg.hook_server_timeout)?,
        claude,
        codex,
        prepared: [Harness::Claude, Harness::Codex]
            .into_iter()
            .map(|h| (h, Arc::new(OnceCell::new())))
            .collect(),
        cfg,
        mux,
        sup_kick: Notify::new(),
        st: Mutex::new(st),
        state_file,
        wake_kick: Notify::new(),
        presence_kick: Notify::new(),
        refresh_kick: Notify::new(),
        started: Instant::now(),
    });

    let mut tasks = Vec::new();
    for h in [Harness::Claude, Harness::Codex] {
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
        let (r, mut w) = stream.into_split();
        let mut line = String::new();
        let mut reader = BufReader::new(r);
        let read = reader.read_line(&mut line);
        match tokio::time::timeout(Duration::from_secs(10), read).await {
            Ok(Ok(n)) if n > 0 => {}
            _ => return,
        }
        let started = Instant::now();
        let resp = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Hook {
                harness,
                env,
                input,
                budget_ms,
            }) => {
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
            Ok(req) => self.handle(req).await,
            Err(e) => json!({"error": format!("bad request: {e}")}),
        };
        let mut out = resp.to_string();
        out.push('\n');
        let _ = w.write_all(out.as_bytes()).await;
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
            } => match self
                .launch(
                    &seat,
                    &cwd,
                    pid,
                    harness.as_deref(),
                    &extra,
                    pane,
                    mux_session,
                )
                .await
            {
                Ok(plan) => serde_json::to_value(plan).unwrap_or_default(),
                Err(e) => json!({"error": format!("{e:#}")}),
            },
            Request::Credentials { codex_thread } => match self.credentials(&codex_thread) {
                Some(c) => serde_json::to_value(c).unwrap_or_default(),
                None => json!({"error": "no seat is registered for this thread"}),
            },
            Request::Status => self.status(),
            Request::Up {
                seats,
                settings,
                start,
                only,
            } => self.sup_up(seats, settings, start, only),
            Request::Down { seats } => self.sup_down(seats).await,
            Request::Forget { seats } => self.sup_forget(seats).await,
            Request::Sleep { seat } => self.sup_sleep(&seat).await,
            Request::Wake { seat } => self.sup_wake(&seat),
            Request::Ps => self.sup_ps(),
            Request::Locate { target } => self.sup_locate(&target),
        }
    }

    // --- hooks ----------------------------------------------------------------

    /// Handle one hook; returns its stdout.
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
        let Some((seat, registered)) = self.identify(&*adapter, env, &hook) else {
            return HookOutcome::default();
        };
        if registered && let Some(sid) = hook.session_id.clone() {
            let (a, s) = (adapter.clone(), seat.clone());
            tokio::spawn(async move { a.registered(&sid, &s).await });
        }
        let own = Instant::now() + self.cfg.hook_server_timeout;
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
        // 1. State transition and plan, under the lock.
        let plan = {
            let mut st = self.lock();
            let connected = st.connected;
            if st.saved.managed.contains_key(seat) {
                // Supervised: the process is watched directly. SessionEnd
                // may come late (Codex: about a minute after the TUI ended,
                // possibly after the seat was restarted), so it is ignored.
                if hook.event == Event::SessionEnd {
                    return (Output::None, Vec::new());
                }
                if supervise::on_hook(&mut st, seat, hook.session_id.as_deref()) {
                    self.sup_kick.notify_one();
                }
            }
            st.seat(seat).last_activity = now_ms();
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
            let mut plan = Plan::default();
            match &hook.event {
                Event::SessionStart { source } => {
                    if let Some(s) = adapter.session_start_state(source) {
                        st.set_state(seat, s);
                    }
                    plan.summary = connected;
                }
                Event::UserPromptSubmit { .. } => {
                    st.set_state(seat, SeatState::Busy);
                    let s = st.seat(seat);
                    s.stop_blocks = 0;
                    s.wake_out = None;
                    s.clear_wake_failure();
                    plan.stash = std::mem::take(&mut s.stash);
                    plan.claim_all = connected && s.pending.total() > 0;
                    plan.summary = connected && registered;
                }
                Event::PostToolUse => {
                    st.set_state(seat, SeatState::Busy);
                    let s = st.seat(seat);
                    plan.stash = std::mem::take(&mut s.stash);
                    plan.claim_to = connected && s.pending.to > 0;
                    plan.claim_urgent = connected && s.pending.urgent > 0;
                }
                Event::Stop => {
                    let s = st.seat(seat);
                    let pending = connected && s.pending.total() > 0;
                    if (pending || !s.stash.is_empty()) && s.stop_blocks < cap {
                        plan.stash = std::mem::take(&mut s.stash);
                        plan.claim_all = pending;
                    } else {
                        st.set_state(seat, SeatState::Idle);
                    }
                }
                Event::Interrupt => {
                    st.set_state(seat, SeatState::Idle);
                }
                Event::Awaiting => {
                    st.set_state(seat, SeatState::Awaiting);
                }
                Event::SessionEnd => {
                    st.set_state(seat, SeatState::Offline);
                    st.seat(seat).session = None;
                }
            }
            if !plan.stash.is_empty() {
                st.record_stash(seat);
            }
            st.record_live(seat);
            plan
        };
        self.presence_kick.notify_one();
        self.wake_kick.notify_one();
        if plan.none() {
            return (Output::None, Vec::new());
        }

        // 2. Server calls, without the lock, all within `deadline`.
        let limit = adapter.claim_limit();
        let via = format!("hook:{}", hook.name);
        let stashed = plan.stash.len();
        let mut claimed: Vec<InboxEntry> = plan.stash;
        let mut failed = false;
        let mut more = false;
        use DeliveryClass::*;
        let mut calls: Vec<(&'static [DeliveryClass], bool)> = Vec::new();
        if plan.claim_all {
            calls.push((&[To, Cc, List], false));
        } else {
            if plan.claim_to {
                calls.push((&[To], false));
            }
            if plan.claim_urgent {
                calls.push((&[Cc, List], true));
            }
        }
        for (classes, urgent) in calls {
            let call = self.server.claim(seat, classes, urgent, &via, limit);
            match tokio::time::timeout_at(deadline.into(), call).await {
                Ok(Ok(r)) => {
                    more |= r.entries.len() as u32 >= limit;
                    claimed.extend(r.entries);
                }
                Ok(Err(e)) => {
                    tracing::debug!("claim for {seat}: {e:#}");
                    failed = true;
                }
                Err(_) => {
                    tracing::debug!("claim for {seat}: hook deadline passed");
                    failed = true;
                }
            }
        }
        let summary = if plan.summary && !failed {
            let call = self.server.unread(seat, SUMMARY_LIMIT);
            match tokio::time::timeout_at(deadline.into(), call).await {
                Ok(Ok(r)) => {
                    let seen: HashSet<i64> = claimed.iter().map(|e| e.message.id).collect();
                    r.entries
                        .into_iter()
                        .filter(|e| !seen.contains(&e.message.id))
                        .collect()
                }
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        // 3. Apply the outcome.
        {
            let mut st = self.lock();
            if claimed.len() > stashed {
                let s = st.seat(seat);
                for e in &claimed[stashed..] {
                    let p = &mut s.pending;
                    match e.delivery.class {
                        To => p.to = (p.to - 1).max(0),
                        Cc => p.cc = (p.cc - 1).max(0),
                        List => p.list = (p.list - 1).max(0),
                    }
                    if e.message.priority == Priority::Urgent {
                        p.urgent = (p.urgent - 1).max(0);
                    }
                    p.seq = p.seq.max(e.delivery.mod_seq);
                }
                st.dirty_pending.insert(seat.to_string());
                self.refresh_kick.notify_one();
            }
            if hook.event == Event::Stop {
                if claimed.is_empty() {
                    st.set_state(seat, SeatState::Idle);
                } else {
                    st.seat(seat).stop_blocks += 1;
                }
                self.presence_kick.notify_one();
                self.wake_kick.notify_one();
            }
        }

        let mut text = String::new();
        if !claimed.is_empty() {
            text = crate::inject::new_mail(seat, &claimed, more);
        }
        if !summary.is_empty() {
            let rest = adapter.inject_budget().saturating_sub(text.len()).max(500);
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&crate::inject::unread_summary(seat, &summary, rest));
        }
        let out = if text.is_empty() {
            Output::None
        } else if hook.event == Event::Stop {
            Output::Block(text)
        } else {
            Output::Context(text)
        };
        (out, claimed)
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
    ) -> Result<LaunchPlan> {
        let managed = self.lock().saved.managed.get(seat).cloned();
        let harness = harness.or(managed.as_ref().map(|m| m.harness.as_str()));
        let info = self.server.launch(seat).await?;
        let name = harness
            .map(str::to_string)
            .or_else(|| info.harness.clone())
            .with_context(|| format!("seat {seat} has no harness; set one or pass --harness"))?;
        let h = Harness::parse(&name).with_context(|| format!("unknown harness {name:?}"))?;
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
        }
        // A supervised seat resumes its last session when it has one;
        // Claude Code sessions get their id up front.
        let resume = managed
            .as_ref()
            .filter(|m| Harness::parse(&m.harness) == Some(h))
            .and_then(|m| m.resume.clone());
        let context_lost = managed.as_ref().is_some_and(|m| m.context_lost);
        let new_id = (resume.is_none() && h == Harness::Claude).then(supervise::uuid_v4);
        self.save_now();
        let plan = self
            .adapter(h)
            .launch(&LaunchCtx {
                info: &info,
                cwd,
                extra,
                nonce: &nonce,
                resume: resume.as_deref(),
                new_session_id: new_id.as_deref(),
                context_lost,
            })
            .await?;
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
        let st = self.lock();
        let now = Instant::now();
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
                    "session": s.session.as_ref().map(|x| json!({
                        "id": x.id, "pid": x.pid.map(|p| p.0), "target": x.target,
                    })),
                    "pending": {"to": s.pending.to, "cc": s.pending.cc, "list": s.pending.list, "urgent": s.pending.urgent},
                    "stashed": s.stash.len(),
                    "stop_blocks": s.stop_blocks,
                    "wake_due_ms": s.wake_due.map(|d| d.saturating_duration_since(now).as_millis() as u64),
                    "wake_unconfirmed": s.wake_out.map(|(_, n)| n),
                    "wake_failed": s.wake_failed,
                    "wake_retry_ms": s.wake_retry_at.map(|d| d.saturating_duration_since(now).as_millis() as u64),
                    "wakes": s.wakes,
                })
            })
            .collect();
        seats.sort_by(|a, b| a["seat"].as_str().cmp(&b["seat"].as_str()));
        json!({
            "connected": st.connected,
            "server": self.cfg.url,
            "uptime_s": self.started.elapsed().as_secs(),
            "registered_sessions": st.saved.sessions.len(),
            "seats": seats,
        })
    }

    // --- server sync ----------------------------------------------------------

    fn apply_pending(&self, st: &mut State, counts: Vec<PendingCounts>) {
        for c in counts {
            let name = c.seat.clone();
            let seat = st.seat(&name);
            if c.seq < seat.pending.seq {
                continue;
            }
            // New mail that could wake the seat ends a wake-up failure.
            let arrived = c.to > seat.pending.to || c.urgent > seat.pending.urgent;
            seat.pending = c;
            if arrived && seat.wake_failed {
                seat.clear_wake_failure();
                st.dirty_presence.insert(name);
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
                            self.apply_pending(&mut st, p.seats);
                            st.connected = true;
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
                        if m.seq > st.seat(&m.seat).pending.seq {
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

    async fn refresh_loop(self: Arc<Self>) {
        loop {
            self.refresh_kick.notified().await;
            loop {
                let dirty: Vec<String> = std::mem::take(&mut self.lock().dirty_pending)
                    .into_iter()
                    .collect();
                if dirty.is_empty() {
                    break;
                }
                let results: Vec<Result<amail_proto::PendingResponse>> = if dirty.len() > 32 {
                    vec![self.server.pending(None).await]
                } else {
                    futures::future::join_all(dirty.iter().map(|s| self.server.pending(Some(s))))
                        .await
                };
                let mut st = self.lock();
                for r in results {
                    match r {
                        Ok(p) => self.apply_pending(&mut st, p.seats),
                        Err(e) => tracing::debug!("refreshing pending counts: {e:#}"),
                    }
                }
                drop(st);
                self.wake_kick.notify_one();
                self.presence_kick.notify_one();
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
    fn wake_scan(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let mut soonest = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        let mut fire: Vec<(Harness, String, String, u64)> = Vec::new();
        let mut st = self.lock();
        let connected = st.connected;
        let mut counter = st.saved.wake_counter;
        let mut changed = Vec::new();
        let (confirm, window) = (self.cfg.wake_confirm, self.cfg.wake_window);
        let (backoff0, backoff_max) = (self.cfg.wake_backoff, self.cfg.wake_backoff_max);
        for (name, s) in st.seats.iter_mut() {
            let target = s.session.as_ref().and_then(|x| x.target.clone());
            let eligible =
                connected && s.state == SeatState::Idle && target.is_some() && s.wants_wake();
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
        drop(st);
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

    // --- presence and liveness -------------------------------------------------

    async fn presence_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(self.cfg.heartbeat);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let full = tokio::select! {
                _ = self.presence_kick.notified() => {
                    // Batch changes that arrive together.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    false
                }
                _ = tick.tick() => true,
            };
            let (report, sent): (Vec<PresenceUpdate>, Vec<String>) = {
                let mut st = self.lock();
                if !st.connected {
                    continue;
                }
                let dirty = std::mem::take(&mut st.dirty_presence);
                let mut out = Vec::new();
                let mut names = Vec::new();
                for (name, s) in st.seats.iter_mut() {
                    let include = dirty.contains(name)
                        || (full && (s.state != SeatState::Offline || !s.reported));
                    if !include || s.harness.is_none() {
                        continue;
                    }
                    s.reported = s.state != SeatState::Offline;
                    names.push(name.clone());
                    out.push(PresenceUpdate {
                        seat: name.clone(),
                        state: s.state,
                        wake_failed: s.wake_failed,
                        harness: s.harness.map(|h| h.as_str().to_string()),
                        since: Some(ts(s.since)),
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
                st.set_state(&name, SeatState::Offline);
            }
            if !st.dirty_presence.is_empty() {
                self.presence_kick.notify_one();
            }
        }
    }
}

/// Ask a running node (as `amail run` and the CLI do).
pub fn request(socket: &std::path::Path, req: &Request, timeout: Duration) -> Result<Value> {
    use std::io::{BufRead, Write};
    let mut s = std::os::unix::net::UnixStream::connect(socket).with_context(|| {
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

#[cfg(test)]
mod tests {
    #[test]
    fn ts() {
        assert_eq!(super::ts(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(super::ts(1_791_050_365_210), "2026-10-03T17:59:25.210Z");
    }
}
