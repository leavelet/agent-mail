//! Supervised seats (`amail up`): the node starts their sessions in the
//! multiplexer, puts idle ones to sleep, starts sleeping ones again when
//! mail arrives, and restarts crashed ones.
//!
//! Life of a supervised seat:
//!
//! - **start**: queued, then (at most `start_limit` at a time) a pane runs
//!   `amail run <seat>`, which asks the node for the launch plan. The plan
//!   resumes the seat's last harness session when there is one. The start
//!   slot is held until the session's first hook (or `start_timeout`, after
//!   which the pane's screen is checked for prompts that need a person).
//! - **sleep** (idle longer than `idle_sleep` with nothing pending, or the
//!   least recently active idle seats when more than `max_live` run, or
//!   `amail sleep`): the process gets SIGTERM (SIGKILL after `kill_grace`);
//!   the seat becomes `dormant`, keeping its session id and its pane.
//! - **cold start**: a dormant seat with pending `to` or urgent mail is
//!   started again (resuming its session); the mail then arrives through
//!   the usual wake-up path.
//! - **crash**: a process that ends without `down` or sleep is restarted
//!   (resuming) after a backoff that doubles per consecutive crash, unless
//!   `restart = "never"`; after `max_restarts` restarts it is marked failed.
//!   A clean exit (status 0, e.g. the person typed `/exit`) is not a crash:
//!   the seat goes offline until the next `amail up` or `amail wake`.

use super::*;
use crate::mux::PaneSpec;
use crate::persist::Managed;
use crate::team::Restart;
use crate::wire::{PsRow, SeatPlacement, SessionClaim};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Intent {
    Sleep,
    Down,
    /// Stop and start again at once (a seat stuck at a prompt).
    Restart,
}

/// Runtime state of a supervised seat.
#[derive(Debug, Clone, Default)]
pub(super) struct SupRt {
    /// Waiting in the start queue.
    queued: bool,
    /// The pane is being opened.
    opening: bool,
    /// Launched; no hook yet.
    starting_since: Option<Instant>,
    /// Counts against the start limit.
    holds_slot: bool,
    launched_at: Option<Instant>,
    resumed: bool,
    hooked: bool,
    stopping: Option<(Intent, Instant, bool)>,
    retry_at: Option<Instant>,
    exiting: bool,
    probing: bool,
    probed_at: Option<Instant>,
    /// Needs a person: (tag, hint).
    blocked: Option<(String, String)>,
    /// Counts launches; a probe result is used only for the launch it
    /// looked at.
    launch_gen: u64,
    /// The pane's screen just before it was run again: the previous run's
    /// output stays on it until the new session draws, and says nothing
    /// about the new session.
    old_screen: Option<String>,
}

impl SupRt {
    /// Read-only visibility of existing barriers, without held messages or
    /// session routing/authorization material.
    pub(super) fn quiet_recovery_diagnostics(&self) -> serde_json::Value {
        serde_json::json!({
            "blocked": self.quiet_recovery_blocked(),
            "queued": self.queued,
            "opening": self.opening,
            "starting": self.starting_since.is_some(),
            "slot": self.holds_slot,
            "stopping": self.stopping.is_some(),
            "retry": self.retry_at.is_some(),
            "exiting": self.exiting,
            "probing": self.probing,
            "support_block": self.blocked.is_some(),
        })
    }

    pub(super) fn quiet_recovery_blocked(&self) -> bool {
        self.queued
            || self.opening
            || self.starting_since.is_some()
            || self.holds_slot
            || self.stopping.is_some()
            || self.retry_at.is_some()
            || self.exiting
            || self.probing
            || self.blocked.is_some()
    }

    #[cfg(test)]
    pub(super) fn stopping_for_test(intent: Intent) -> Self {
        Self {
            stopping: Some((intent, Instant::now(), false)),
            ..Self::default()
        }
    }
}

pub fn uuid_v4() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Signal `pid`'s process group if it leads one (harnesses run as group
/// leaders in their panes; their tool processes are in the group), else
/// the process alone.
fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: getpgid and kill have no memory-safety preconditions.
    unsafe {
        if libc::getpgid(pid as i32) == pid as i32 {
            libc::kill(-(pid as i32), sig);
        } else {
            libc::kill(pid as i32, sig);
        }
    }
}

/// Recognize prompts that need a person on a pane's screen.
pub(super) fn classify_screen(screen: &str, seat: &str, cwd: &str) -> Option<(String, String)> {
    let s = screen.to_lowercase();
    let has = |w: &str| s.contains(w);
    if has("trust the files")
        || has("do you trust")
        || has("trust this folder")
        || has("one you trust")
        || (has("trust") && has("folder") && has("proceed"))
    {
        return Some((
            "trust-prompt".into(),
            format!(
                "Claude Code asks whether to trust {cwd}: answer it once in any pane of \
                 this repository (amail attach {seat}), then run amail wake on the other \
                 seats stuck at this prompt; amail does not answer it for you"
            ),
        ));
    }
    if has("incompatible feature settings")
        || (has("background server") && has("restart with these settings"))
    {
        return Some((
            "codex-daemon".into(),
            format!(
                "Codex refuses to attach the session to its background app-server (another \
                 version or other feature settings): choose Cancel in the pane (amail attach \
                 {seat}), then run amail down and amail up; the node then starts the daemon of \
                 the codex command's version again. Do not choose \"Restart with these \
                 settings\": it restarts the shared daemon from this session"
            ),
        ));
    }
    if has("/login")
        || has("not logged in")
        || has("invalid api key")
        || has("please log in")
        || has("login required")
        || has("authentication_error")
        || has("token has expired")
        || has("sign in with")
    {
        return Some((
            "login".into(),
            format!(
                "the harness is not logged in: log in (amail attach {seat}), or fix the \
                 login outside amail and run amail wake {seat}"
            ),
        ));
    }
    None
}

/// Whether a seat needs a person, and what to do: `(tag, hint)`.
pub(super) fn attention_of(st: &State, name: &str) -> Option<(String, String)> {
    let s = st.seats.get(name);
    let m = st.saved.managed.get(name);
    let mut attention = st.sup.get(name).and_then(|rt| rt.blocked.clone());
    if attention.is_none() && s.is_some_and(|s| s.state == SeatState::Awaiting) {
        attention = Some((
            "permission".into(),
            format!(
                "waiting for a permission decision: answer it in the pane (amail attach {name})"
            ),
        ));
    }
    if attention.is_none()
        && let Some(m) = m.filter(|m| m.failed)
    {
        attention = Some((
            "failed".into(),
            format!(
                "{} ({} times in a row); look at the pane (amail attach {name}), then amail wake {name}",
                m.last_exit.clone().unwrap_or_else(|| "failed".into()),
                m.failures
            ),
        ));
    }
    if attention.is_none()
        && let Some(t) = st.saved.turns.get(name).filter(|t| t.failures > 0)
    {
        let reason = t.reason.as_deref().unwrap_or("an error");
        attention = Some((
            "turn-failed".into(),
            if t.failures >= st.turn_fail_limit && !t.manual_retry {
                format!(
                    "its last {} turns failed ({reason}); amail stopped delivering their mail \
                     again: look at the session (amail attach {name}), then amail wake {name}",
                    t.failures
                )
            } else {
                format!(
                    "its last turn failed ({reason}); mail that turn showed, if any, is \
                     delivered again with the next turn ({} failed in a row)",
                    t.failures
                )
            },
        ));
    }
    if attention.is_none() && s.is_some_and(|s| s.wake_failed) {
        attention = Some((
            "wake-failed".into(),
            format!("wake-ups are not answered; look at the pane (amail attach {name})"),
        ));
    }
    if attention.is_none()
        && let Some(id) = m.and_then(|m| m.adopt_failed.as_deref())
    {
        attention = Some((
            "adopt-failed".into(),
            format!(
                "the adopted session {id} could not be resumed, so the seat runs a new \
                 session; check the id (amail seat adopt {name} <id>), or amail wake {name} \
                 to clear this"
            ),
        ));
    }
    if attention.is_none() && s.is_some_and(|s| s.restore_pending) {
        attention = Some((
            "recovery-unverified".into(),
            "surviving session has no proven normal completion; automatic mail wakes are held"
                .into(),
        ));
    }
    attention
}

/// Rebuild runtime state of supervised seats at node start.
pub(super) fn restore(st: &mut State, harness: impl Fn(&str) -> Option<Harness>) {
    for (name, m) in st.saved.managed.clone() {
        let s = st.seat(&name);
        s.harness = harness(&m.harness);
        if let Some(pid) = m.pid
            && alive(pid, m.pid_start)
        {
            if s.state == SeatState::Offline {
                s.restore_pending = s.harness == Some(Harness::Codex);
                s.state = if s.restore_pending {
                    SeatState::Busy
                } else {
                    SeatState::Idle
                };
            }
            let sess = s.session.get_or_insert_with(SessionRt::default);
            sess.pid = Some((pid, m.pid_start));
            if sess.id.is_none() {
                sess.id = m.resume.clone();
            }
            if s.restore_pending && sess.target.is_none() {
                sess.target = m.resume.clone();
            }
            st.sup.entry(name.clone()).or_default().hooked = true;
        } else if m.dormant {
            s.state = SeatState::Dormant;
        }
        // A dead pid is handled as an exit by the first supervisor pass.
    }
}

/// A hook from a supervised seat. Returns true if a start slot was freed.
/// The hook's session becomes the one the seat resumes, except while the
/// seat waits to start a session it adopted: hooks from another session
/// then come from the one that ran before the adoption. The adopted
/// session's own first hook ends that wait.
pub(super) fn on_hook(st: &mut State, seat: &str, session_id: Option<&str>) -> bool {
    if let (Some(m), Some(sid)) = (st.saved.managed.get_mut(seat), session_id) {
        if m.adopt_intro {
            if m.resume.as_deref() == Some(sid) {
                m.adopt_intro = false;
                st.save_dirty = true;
            }
        } else if m.resume.as_deref() != Some(sid) {
            m.resume = Some(sid.to_string());
            st.save_dirty = true;
        }
    }
    let rt = st.sup.entry(seat.to_string()).or_default();
    rt.hooked = true;
    rt.starting_since = None;
    rt.blocked = None;
    std::mem::take(&mut rt.holds_slot)
}

/// `amail run` of a supervised seat asked for its launch plan.
#[allow(clippy::too_many_arguments)]
pub(super) fn launched(
    st: &mut State,
    seat: &str,
    h: Harness,
    pid: u32,
    pid_start: Option<u64>,
    pane: Option<u32>,
    resume: Option<&str>,
    new_id: Option<&str>,
    lazy_resume: bool,
) {
    let now = Instant::now();
    if let Some(m) = st.saved.managed.get_mut(seat) {
        m.pid = Some(pid);
        m.pid_start = pid_start;
        if pane.is_some() {
            m.pane = pane;
        }
        if resume.is_none() {
            m.context_lost = false;
            m.adopt_intro = false;
        }
        m.dormant = false;
        st.save_dirty = true;
    }
    let rt = st.sup.entry(seat.to_string()).or_default();
    rt.launch_gen += 1;
    rt.launched_at = Some(now);
    rt.resumed = resume.is_some();
    rt.hooked = false;
    rt.blocked = None;
    rt.starting_since.get_or_insert(now);
    let lazy = lazy_resume && resume.is_some();
    if lazy {
        // Such a session fires no hook until a turn runs; it is ready to
        // be woken (its wake target is the session id) once launched.
        rt.holds_slot = false;
        rt.starting_since = None;
    }
    let s = st.seat(seat);
    s.harness = Some(h);
    s.last_activity = now_ms();
    s.session = Some(SessionRt {
        id: resume.or(new_id).map(str::to_string),
        pid: Some((pid, pid_start)),
        target: if lazy {
            resume.map(str::to_string)
        } else {
            None
        },
    });
    if lazy {
        st.set_state(seat, SeatState::Idle);
    } else {
        st.set_state(seat, SeatState::Offline);
    }
    // Report the seat now (with its terminal), not at its first hook: a
    // session stuck before its first hook is when people need it most.
    st.dirty_presence.insert(seat.to_string());
}

impl State {
    /// Seats with a process, or about to have one (not counting those
    /// already on their way out).
    fn live_count(&self) -> usize {
        self.saved
            .managed
            .iter()
            .filter(|(n, m)| {
                let rt = self.sup.get(*n);
                if rt.is_some_and(|r| r.stopping.is_some()) {
                    return false;
                }
                m.pid.is_some()
                    || rt.is_some_and(|r| r.queued || r.opening || r.starting_since.is_some())
            })
            .count()
    }
}

impl Node {
    fn pane_argv(&self, seat: &str) -> Vec<String> {
        vec![
            self.cfg.amail_exe.to_string_lossy().into_owned(),
            "run".into(),
            seat.into(),
            "--socket".into(),
            self.cfg.socket.to_string_lossy().into_owned(),
        ]
    }

    pub(super) async fn supervise_loop(self: Arc<Self>) {
        let mut maintained = Instant::now();
        loop {
            self.supervise_pass();
            if maintained.elapsed() > Duration::from_secs(5) {
                maintained = Instant::now();
                let sessions: Vec<String> = {
                    let st = self.lock();
                    let mut v: Vec<String> = st
                        .saved
                        .managed
                        .values()
                        .filter(|m| m.up || m.pid.is_some())
                        .map(|m| m.mux_session.clone())
                        .collect();
                    v.sort();
                    v.dedup();
                    v
                };
                self.mux.maintain(&sessions).await;
                self.check_closed_panes(&sessions).await;
            }
            tokio::select! {
                _ = self.sup_kick.notified() => {}
                _ = tokio::time::sleep(self.cfg.supervise_interval) => {}
            }
        }
    }

    /// A person closed a running seat's pane: its process may live on,
    /// invisible. Stop it; the seat is down until `amail up` or `wake`.
    async fn check_closed_panes(&self, sessions: &[String]) {
        for session in sessions {
            let Ok(Some(panes)) = self.mux.panes(session).await else {
                continue;
            };
            let gone: Vec<String> = {
                let st = self.lock();
                st.saved
                    .managed
                    .iter()
                    .filter(|(n, m)| {
                        &m.mux_session == session
                            && m.pid.is_some()
                            && m.pane.is_some_and(|p| !panes.iter().any(|x| x.id == p))
                            && st.sup.get(*n).is_none_or(|r| {
                                !r.opening && r.starting_since.is_none() && r.stopping.is_none()
                            })
                    })
                    .map(|(n, _)| n.clone())
                    .collect()
            };
            for n in gone {
                tracing::warn!("seat {n}: its pane was closed; stopping the seat");
                let mut st = self.lock();
                if let Some(m) = st.saved.managed.get_mut(&n) {
                    m.pane = None;
                    m.last_exit = Some("its pane was closed".into());
                }
                self.begin_stop(&mut st, &n, Intent::Down);
            }
        }
    }

    /// One pass: notice exits, escalate stops, put seats to sleep, start
    /// what should run.
    fn supervise_pass(self: &Arc<Self>) {
        let now = Instant::now();
        let mut exits = Vec::new();
        let mut probes = Vec::new();
        let mut sleeps = Vec::new();
        let mut starts = Vec::new();
        {
            let mut st = self.lock();
            let connected = st.connected;
            let settings = st.saved.settings.clone();
            let (wall, digest_every) = (now_ms(), st.digest_every);
            // Team order: parents (leads) start before their subordinates.
            let mut names: Vec<String> = st.saved.managed.keys().cloned().collect();
            names.sort_by_key(|n| st.saved.managed[n].order);
            let limit = self.cfg.turn_fail_limit;
            let exhausted_seats: HashSet<String> = names
                .iter()
                .filter(|n| st.turns_exhausted(n, limit))
                .cloned()
                .collect();
            let exhausted = |n: &str| exhausted_seats.contains(n);
            for name in &names {
                let m = st.saved.managed[name].clone();
                let wants_mail = st
                    .seats
                    .get(name)
                    .is_some_and(|s| s.wants_wake(wall, digest_every));
                let rt = st.sup.entry(name.clone()).or_default();
                if let Some(pid) = m.pid {
                    if !alive(pid, m.pid_start) {
                        if !rt.exiting {
                            rt.exiting = true;
                            exits.push(name.clone());
                        }
                        continue;
                    }
                    if let Some((intent, since, false)) = rt.stopping
                        && now >= since + self.cfg.kill_grace
                    {
                        tracing::warn!("seat {name}: still running; sending SIGKILL");
                        signal(pid, libc::SIGKILL);
                        rt.stopping = Some((intent, since, true));
                    }
                }
                if let Some(since) = rt.starting_since
                    && !rt.hooked
                    && !rt.opening
                    && !rt.probing
                {
                    let elapsed = now - since;
                    // Look at the screen early and often while starting (a
                    // prompt that needs a person should not hold a start
                    // slot), then less often while still stuck.
                    let every = if elapsed < self.cfg.start_timeout {
                        self.cfg.probe_interval
                    } else {
                        self.cfg.probe_interval.max(Duration::from_secs(10))
                    };
                    let due = rt
                        .probed_at
                        .map_or(elapsed >= self.cfg.probe_interval, |t| now - t >= every);
                    if due {
                        rt.probing = true;
                        rt.probed_at = Some(now);
                        if elapsed >= self.cfg.start_timeout {
                            rt.holds_slot = false;
                        }
                        probes.push(name.clone());
                    }
                }
                let busy = m.pid.is_some()
                    || rt.queued
                    || rt.opening
                    || rt.exiting
                    || rt.starting_since.is_some();
                if busy || !m.up || m.failed {
                    continue;
                }
                if rt.retry_at.is_some_and(|t| now < t) {
                    continue;
                }
                // A sleeping seat starts for mail, unless the person put it
                // to sleep (until new mail) or its turns failed too often.
                if m.dormant && (m.held.is_some() || !(connected && wants_mail) || exhausted(name))
                {
                    continue;
                }
                rt.retry_at = None;
                rt.queued = true;
                st.start_queue.push_back(name.clone());
            }

            // Sleep: idle too long, or too many live seats.
            let idle_ok = |st: &State, name: &str| -> bool {
                let (Some(s), Some(m)) = (st.seats.get(name), st.saved.managed.get(name)) else {
                    return false;
                };
                let rt = st.sup.get(name);
                m.pid.is_some()
                    && s.state == SeatState::Idle
                    // Pending cc and list mail never wakes a seat, so it must
                    // not keep one awake either; it is shown at the next turn.
                    && !s.wants_wake(now_ms(), st.digest_every)
                    && s.wake_out.is_none()
                    && rt.is_none_or(|r| r.stopping.is_none() && r.starting_since.is_none())
            };
            if let Some(ms) = settings.idle_sleep_ms {
                let cutoff = now_ms() - ms as i64;
                for name in &names {
                    if idle_ok(&st, name) && st.seats[name].since <= cutoff {
                        tracing::info!("seat {name}: idle for {} s; going to sleep", ms / 1000);
                        sleeps.push(name.clone());
                    }
                }
            }
            if let Some(max) = settings.max_live {
                let live = st.live_count().saturating_sub(sleeps.len());
                if live > max as usize {
                    let mut idle: Vec<(i64, String)> = names
                        .iter()
                        .filter(|n| idle_ok(&st, n) && !sleeps.contains(n))
                        .map(|n| (st.seats[n].last_activity, n.clone()))
                        .collect();
                    idle.sort();
                    for (_, n) in idle.into_iter().take(live - max as usize) {
                        tracing::info!("seat {n}: more than {max} live seats; going to sleep");
                        sleeps.push(n);
                    }
                }
            }
            for n in &sleeps {
                self.begin_stop(&mut st, n, Intent::Sleep);
            }

            // Starts, within the limit.
            let mut busy = st
                .sup
                .values()
                .filter(|r| r.opening || r.holds_slot)
                .count();
            while busy < self.cfg.start_limit.max(1) {
                let Some(name) = st.start_queue.pop_front() else {
                    break;
                };
                let rt = st.sup.entry(name.clone()).or_default();
                if !rt.queued {
                    continue;
                }
                rt.queued = false;
                rt.opening = true;
                busy += 1;
                starts.push(name);
            }
        }
        for n in exits {
            let me = self.clone();
            tokio::spawn(async move { me.handle_exit(&n).await });
        }
        for n in probes {
            let me = self.clone();
            tokio::spawn(async move { me.probe(&n).await });
        }
        for n in starts {
            let me = self.clone();
            tokio::spawn(async move { me.start_seat(&n).await });
        }
        if !sleeps.is_empty() {
            self.presence_kick.notify_one();
        }
    }

    /// A running seat that never got going: blocked at a prompt, or still
    /// without a hook after the start timeout.
    fn is_stuck(&self, st: &State, seat: &str) -> bool {
        st.sup.get(seat).is_some_and(|r| {
            r.stopping.is_none()
                && !r.hooked
                && (r.blocked.is_some()
                    || r.starting_since
                        .is_some_and(|t| t.elapsed() >= self.cfg.start_timeout))
        })
    }

    /// Ask the process of `seat` to end; the exit is handled when it is gone.
    pub(super) fn begin_stop(&self, st: &mut State, seat: &str, intent: Intent) {
        let Some(m) = st.saved.managed.get(seat).cloned() else {
            return;
        };
        let rt = st.sup.entry(seat.to_string()).or_default();
        rt.queued = false;
        rt.retry_at = None;
        match m.pid {
            Some(pid) if alive(pid, m.pid_start) => {
                if rt.stopping.is_none() {
                    rt.stopping = Some((intent, Instant::now(), false));
                    signal(pid, libc::SIGTERM);
                } else if let Some((i, since, k)) = rt.stopping
                    && i != Intent::Down
                    && intent == Intent::Down
                {
                    rt.stopping = Some((Intent::Down, since, k));
                }
            }
            _ => {
                // Not running (maybe starting): settle at once.
                rt.starting_since = None;
                rt.holds_slot = false;
                let m = st.saved.managed.get_mut(seat).expect("checked");
                match intent {
                    Intent::Sleep => {
                        m.dormant = true;
                        st.set_state(seat, SeatState::Dormant);
                    }
                    Intent::Down => {
                        m.up = false;
                        m.dormant = false;
                        st.set_state(seat, SeatState::Offline);
                    }
                    Intent::Restart => {
                        m.up = true;
                        m.dormant = false;
                    }
                }
                st.save_dirty = true;
            }
        }
        self.sup_kick.notify_one();
    }

    async fn start_seat(self: Arc<Self>, seat: &str) {
        let Some(m) = self.lock().saved.managed.get(seat).cloned() else {
            return;
        };
        let spec = PaneSpec {
            name: seat.to_string(),
            cwd: PathBuf::from(&m.cwd),
            argv: self.pane_argv(seat),
        };
        let opened_at = Instant::now();
        let mut old_screen = None;
        let r = async {
            if let Some(p) = m.pane
                && self.pane_of(&m.mux_session, p, seat).await?.is_some()
            {
                // The multiplexer marks a pane exited a moment after its
                // process ended (the node may already have handled the
                // exit, e.g. after a stop it asked for): wait for that.
                let mut exited = false;
                for _ in 0..30 {
                    match self.pane_of(&m.mux_session, p, seat).await? {
                        Some(true) => {
                            exited = true;
                            break;
                        }
                        Some(false) => tokio::time::sleep(Duration::from_millis(100)).await,
                        None => break,
                    }
                }
                if exited {
                    old_screen = self.mux.screen(&m.mux_session, p).await.ok();
                    self.mux.rerun(&m.mux_session, p).await?;
                    return Ok(p);
                }
                if self.pane_of(&m.mux_session, p, seat).await?.is_some() {
                    bail!("its pane terminal_{p} is still running");
                }
            }
            self.mux.open(&m.mux_session, &m.tab, &spec).await
        }
        .await;
        let mut st = self.lock();
        let rt = st.sup.entry(seat.to_string()).or_default();
        rt.opening = false;
        match r {
            Ok(pane) => {
                tracing::info!("seat {seat}: started in {} pane {pane}", m.mux_session);
                // The session may have launched (and even hooked) before
                // the multiplexer call returned.
                if rt.launched_at.is_some_and(|t| t >= opened_at) {
                    rt.holds_slot = !rt.hooked && rt.starting_since.is_some();
                } else {
                    rt.starting_since = Some(Instant::now());
                    rt.holds_slot = true;
                    rt.hooked = false;
                    rt.probed_at = None;
                }
                rt.old_screen = old_screen;
                if let Some(m) = st.saved.managed.get_mut(seat) {
                    m.pane = Some(pane);
                    m.dormant = false;
                }
                st.save_dirty = true;
            }
            Err(e) => {
                tracing::warn!("seat {seat}: could not start: {e:#}");
                let msg = format!("could not start: {e:#}");
                self.failed_run(&mut st, seat, None, msg);
            }
        }
        drop(st);
        self.sup_kick.notify_one();
    }

    /// Whether pane `pane` of `session`, named `seat`, exists, and if so
    /// whether it has exited.
    async fn pane_of(&self, session: &str, pane: u32, seat: &str) -> Result<Option<bool>> {
        Ok(self.mux.panes(session).await?.and_then(|panes| {
            panes
                .iter()
                .find(|x| x.id == pane && x.name == seat)
                .map(|x| x.exited)
        }))
    }

    /// Check a starting seat's pane: did it exit before launching, or is it
    /// waiting for a person?
    async fn probe(self: Arc<Self>, seat: &str) {
        let Some(m) = self.lock().saved.managed.get(seat).cloned() else {
            return;
        };
        let (gen0, old_screen) = self
            .lock()
            .sup
            .get(seat)
            .map(|r| (r.launch_gen, r.old_screen.clone()))
            .unwrap_or_default();
        let lazy = self.lock().sup.get(seat).is_some_and(|r| r.resumed)
            && Harness::parse(&m.harness).is_some_and(|h| self.adapter(h).resume_is_lazy());
        let mut exited = None;
        let mut blocked = None;
        if let Some(p) = m.pane {
            if let Ok(Some(panes)) = self.mux.panes(&m.mux_session).await
                && let Some(info) = panes.iter().find(|x| x.id == p)
                && info.exited
            {
                exited = Some(info.exit_status);
            }
            if exited.is_none()
                && let Ok(screen) = self.mux.screen(&m.mux_session, p).await
                && old_screen.as_ref() != Some(&screen)
            {
                blocked = classify_screen(&screen, seat, &m.cwd);
            }
        }
        let mut st = self.lock();
        let timed_out = st
            .sup
            .get(seat)
            .and_then(|r| r.starting_since)
            .is_some_and(|t| t.elapsed() >= self.cfg.start_timeout);
        let rt = st.sup.entry(seat.to_string()).or_default();
        rt.probing = false;
        // The seat was launched again while the screen was read: what the
        // probe saw belongs to the previous launch.
        if rt.hooked || rt.launch_gen != gen0 {
            return;
        }
        if let Some(status) = exited {
            if m.pid.is_none() {
                // The pane's command ended before it launched a harness.
                rt.starting_since = None;
                rt.holds_slot = false;
                let msg = format!(
                    "the pane exited before the harness started (status {})",
                    status.map_or("unknown".into(), |s| s.to_string())
                );
                self.failed_run(&mut st, seat, None, msg);
                drop(st);
                self.sup_kick.notify_one();
            }
            return;
        }
        if let Some(b) = blocked {
            if rt.blocked.as_ref().map(|x| &x.0) != Some(&b.0) {
                tracing::warn!("seat {seat}: needs a person: {}", b.0);
            }
            rt.blocked = Some(b);
            // Waiting for a person: do not hold up other starts.
            if std::mem::take(&mut rt.holds_slot) {
                drop(st);
                self.sup_kick.notify_one();
            }
        } else if timed_out && !lazy {
            rt.blocked = Some((
                "no-activity".into(),
                format!(
                    "no sign of life from the session {} s after its start: look at the pane \
                     (amail attach {seat})",
                    self.cfg.start_timeout.as_secs()
                ),
            ));
        } else {
            // The prompt is gone (answered, or the session moved on).
            rt.blocked = None;
        }
    }

    async fn handle_exit(self: Arc<Self>, seat: &str) {
        let Some(m) = self.lock().saved.managed.get(seat).cloned() else {
            return;
        };
        let intent = self
            .lock()
            .sup
            .get(seat)
            .and_then(|r| r.stopping.map(|s| s.0));
        let mut status = None;
        if intent.is_none()
            && let Some(p) = m.pane
        {
            // zellij needs a moment to mark the pane as exited.
            for _ in 0..10 {
                if let Ok(Some(panes)) = self.mux.panes(&m.mux_session).await
                    && let Some(info) = panes.iter().find(|x| x.id == p)
                    && info.exited
                {
                    status = Some(info.exit_status);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        // A launcher, configuration or authentication error can also exit
        // before the first hook. It does not mean the saved session is gone.
        // Only the harness's concrete diagnostic for this exact Claude
        // session allows the supervisor to replace it on the next start.
        let missing_resume = if intent.is_none()
            && status.flatten() == Some(1)
            && m.harness == "claude"
            && let (Some(p), Some(sid)) = (m.pane, m.resume.as_deref())
        {
            let expected = format!("No conversation found with session ID: {sid}");
            self.mux
                .screen(&m.mux_session, p)
                .await
                .is_ok_and(|screen| screen.lines().any(|line| line.trim() == expected))
        } else {
            false
        };
        let mut st = self.lock();
        let rt = st.sup.entry(seat.to_string()).or_default();
        let lived = rt.launched_at.map(|t| t.elapsed());
        let (resumed, hooked) = (rt.resumed, rt.hooked);
        rt.exiting = false;
        rt.stopping = None;
        rt.starting_since = None;
        rt.holds_slot = false;
        rt.blocked = None;
        rt.launched_at = None;
        let Some(mm) = st.saved.managed.get_mut(seat) else {
            return;
        };
        mm.pid = None;
        mm.pid_start = None;
        st.save_dirty = true;
        match intent {
            Some(Intent::Sleep) => {
                tracing::info!("seat {seat}: asleep");
                let mm = st.saved.managed.get_mut(seat).expect("present");
                mm.dormant = true;
                mm.last_exit = Some("put to sleep".into());
                st.set_state(seat, SeatState::Dormant);
                st.seat(seat).session = None;
            }
            Some(Intent::Restart) => {
                tracing::info!("seat {seat}: restarting");
                let mm = st.saved.managed.get_mut(seat).expect("present");
                mm.up = true;
                mm.dormant = false;
                mm.failed = false;
                mm.last_exit = Some("restarted".into());
                st.set_state(seat, SeatState::Offline);
                st.sup.entry(seat.to_string()).or_default().retry_at = None;
            }
            Some(Intent::Down) => {
                tracing::info!("seat {seat}: stopped");
                let mm = st.saved.managed.get_mut(seat).expect("present");
                mm.up = false;
                mm.dormant = false;
                mm.last_exit = Some("stopped by amail down".into());
                st.set_state(seat, SeatState::Offline);
            }
            None => {
                let busy = st
                    .seats
                    .get(seat)
                    .is_some_and(|s| s.state == SeatState::Busy);
                st.set_state(seat, SeatState::Offline);
                let resume_failed = missing_resume
                    && resumed
                    && !hooked
                    && lived.is_some_and(|l| l < self.cfg.resume_grace);
                if resume_failed {
                    tracing::warn!(
                        "seat {seat}: resuming its session failed; the next start is a new session"
                    );
                    let mm = st.saved.managed.get_mut(seat).expect("present");
                    if std::mem::take(&mut mm.adopt_intro) {
                        // The session the seat adopted: `amail ps` says so.
                        tracing::warn!(
                            "seat {seat}: the adopted session {} could not be resumed",
                            mm.resume.as_deref().unwrap_or("?")
                        );
                        mm.adopt_failed = mm.resume.clone();
                    }
                    mm.resume = None;
                    mm.context_lost = true;
                }
                let code = status.flatten();
                if code == Some(0) && !resume_failed {
                    self.turn_ended(&mut st, seat, super::turns::TurnEnd::Person);
                    tracing::info!("seat {seat}: the session ended normally");
                    let mm = st.saved.managed.get_mut(seat).expect("present");
                    mm.up = false;
                    mm.failures = 0;
                    mm.last_exit = Some("exited normally (status 0)".into());
                } else {
                    let adopted = st
                        .saved
                        .managed
                        .get(seat)
                        .and_then(|m| m.adopt_failed.clone())
                        .filter(|_| resume_failed);
                    let what = match (resume_failed, code) {
                        (true, _) if adopted.is_some() => format!(
                            "could not resume its adopted session {}",
                            adopted.unwrap_or_default()
                        ),
                        (true, _) => "could not resume its session".to_string(),
                        (false, Some(c)) => format!("exited with status {c}"),
                        (false, None) => "was killed or vanished".to_string(),
                    };
                    tracing::warn!("seat {seat}: the session {what}");
                    if busy {
                        self.turn_ended(
                            &mut st,
                            seat,
                            super::turns::TurnEnd::Failed(format!(
                                "the session {what} during the turn"
                            )),
                        );
                    }
                    let stable = lived.is_some_and(|l| l >= self.cfg.stable_after);
                    self.failed_run(&mut st, seat, Some(stable), what);
                }
            }
        }
        drop(st);
        self.presence_kick.notify_one();
        self.sup_kick.notify_one();
    }

    /// Count a failed run and schedule the restart (or give up).
    fn failed_run(&self, st: &mut State, seat: &str, stable: Option<bool>, what: String) {
        let restart = st.saved.settings.restart;
        let Some(m) = st.saved.managed.get_mut(seat) else {
            return;
        };
        m.failures = if stable == Some(true) {
            1
        } else {
            m.failures + 1
        };
        m.last_exit = Some(what);
        let failures = m.failures;
        st.save_dirty = true;
        let rt = st.sup.entry(seat.to_string()).or_default();
        if restart == Restart::Never {
            st.saved.managed.get_mut(seat).expect("present").up = false;
            return;
        }
        if failures > self.cfg.max_restarts {
            tracing::warn!(
                "seat {seat}: failed {failures} times in a row; not restarting it again"
            );
            st.saved.managed.get_mut(seat).expect("present").failed = true;
            return;
        }
        let delay = self.cfg.restart_backoff * 2u32.saturating_pow(failures - 1);
        rt.retry_at = Some(Instant::now() + delay);
        tracing::info!(
            "seat {seat}: restarting in {:.1} s (failure {failures} of {})",
            delay.as_secs_f64(),
            self.cfg.max_restarts
        );
    }

    // --- requests ---------------------------------------------------------

    pub(super) async fn sup_up(
        &self,
        seats: Vec<SeatPlacement>,
        settings: crate::team::Settings,
        start_all: bool,
        only: Option<Vec<String>>,
    ) -> Value {
        // Sessions the file gives seats for the first time (or changed):
        // all are checked before anything changes.
        let claims: Vec<SessionClaim> = {
            let st = self.lock();
            seats
                .iter()
                .filter_map(|p| {
                    let id = p.session.as_ref()?;
                    let known = st
                        .saved
                        .managed
                        .get(&p.seat)
                        .is_some_and(|m| m.adopted.as_ref() == Some(id));
                    (!known).then(|| SessionClaim {
                        seat: p.seat.clone(),
                        harness: p.harness.clone(),
                        session: id.clone(),
                        cwd: p.cwd.clone(),
                    })
                })
                .collect()
        };
        let found = match self.check_claims(&claims).await {
            Ok(f) => f,
            Err(errors) => return json!({"error": errors.join("\n")}),
        };
        let mut adopted = Vec::new();
        let mut out = Vec::new();
        let mut st = self.lock();
        let old = std::mem::replace(&mut st.saved.settings, settings.clone());
        let mut changed = Vec::new();
        // Nothing ran under the old settings on a first `up`.
        let first = st.saved.managed.is_empty();
        let show_ms = |v: Option<u64>| v.map_or("off".to_string(), |ms| format!("{}s", ms / 1000));
        if !first && old.idle_sleep_ms != settings.idle_sleep_ms {
            changed.push(format!(
                "idle_sleep {} -> {}",
                show_ms(old.idle_sleep_ms),
                show_ms(settings.idle_sleep_ms)
            ));
        }
        if !first && old.handover_idle_ms != settings.handover_idle_ms {
            let show = |v: Option<u64>| match v {
                None => "default".to_string(),
                Some(0) => "off".to_string(),
                Some(ms) => format!("{}s", ms / 1000),
            };
            changed.push(format!(
                "handover_idle {} -> {}",
                show(old.handover_idle_ms),
                show(settings.handover_idle_ms)
            ));
        }
        if !first && old.max_live != settings.max_live {
            let show = |v: Option<u32>| v.map_or("none".to_string(), |n| n.to_string());
            changed.push(format!(
                "max_live {} -> {}",
                show(old.max_live),
                show(settings.max_live)
            ));
        }
        let (was, now) = (
            self.policy_with(old.local_policy),
            self.policy_with(settings.local_policy),
        );
        if was != now {
            changed.push(format!("local_policy {was} -> {now}"));
        }
        if !first && old.restart != settings.restart {
            changed.push(format!(
                "restart {:?} -> {:?}",
                old.restart, settings.restart
            ));
        }
        let mut restart = Vec::new();
        for (order, p) in seats.into_iter().enumerate() {
            let start = start_all && only.as_ref().is_none_or(|o| o.contains(&p.seat));
            let h = Harness::parse(&p.harness);
            let entry = st.saved.managed.get(&p.seat).cloned();
            let mut m = entry.clone().unwrap_or(Managed {
                group: None,
                harness: p.harness.clone(),
                cwd: p.cwd.clone(),
                mux_session: p.mux_session.clone(),
                tab: p.tab.clone(),
                pane: None,
                up: false,
                dormant: false,
                resume: None,
                codex_effort: None,
                context_lost: false,
                adopted: None,
                adopt_intro: false,
                adopt_failed: None,
                failures: 0,
                failed: false,
                pid: None,
                pid_start: None,
                last_exit: None,
                order: 0,
                paste_fallback: false,
                permission_mode: None,
                git_access: None,
                dsh_jail: None,
                held: None,
            });
            m.order = order as u32;
            if m.harness != p.harness {
                // A session cannot move to another harness.
                if m.resume.take().is_some() {
                    m.context_lost = true;
                }
                m.adopt_intro = false;
            }
            if m.mux_session != p.mux_session || m.tab != p.tab {
                m.pane = None;
            }
            m.group = p.group.clone();
            m.paste_fallback = p.paste_fallback;
            m.permission_mode = p.permission_mode;
            m.git_access = p.git_access;
            m.dsh_jail = p.dsh_jail;
            m.harness = p.harness.clone();
            m.cwd = p.cwd.clone();
            m.mux_session = p.mux_session.clone();
            m.tab = p.tab.clone();
            let running = m.pid.is_some_and(|pid| alive(pid, m.pid_start));
            // A removed `session` changes nothing but lets the same id be
            // adopted again later.
            let adopt = p
                .session
                .clone()
                .filter(|id| m.adopted.as_ref() != Some(id));
            m.adopted = p.session.clone();
            let stuck = running && start && self.is_stuck(&st, &p.seat);
            let action = if stuck {
                restart.push(p.seat.clone());
                "restarting (was stuck at a prompt)"
            } else if running {
                "running"
            } else if m.dormant && m.up {
                "dormant"
            } else if !start && m.up {
                "waiting to start"
            } else if start {
                m.up = true;
                m.failed = false;
                m.failures = 0;
                "starting"
            } else {
                "not started"
            };
            if start {
                m.up = true;
                // `amail up` ends a sleep the person asked for: mail
                // waiting for the seat starts it again.
                m.held = None;
            }
            out.push(json!({"seat": p.seat, "action": action}));
            let name = p.seat.clone();
            st.saved.managed.insert(name.clone(), m);
            st.seat(&name).harness = h;
            // Its note's state, for `amail ps` (the jobs run once the node
            // is connected).
            if st.connected && st.want_note_state(&name, false) {
                self.jobs_kick.notify_one();
            }
            if let Some(id) = adopt {
                let registers = h.is_some_and(|h| self.adapter(h).registers_sessions());
                super::adopt::adopt_into(&mut st, &name, &id, registers);
                let f = found
                    .iter()
                    .find(|(c, _)| c.seat == name)
                    .map(|(_, f)| f.clone())
                    .unwrap_or_default();
                tracing::info!("seat {name}: adopts session {id}");
                adopted.push(json!({
                    "seat": name,
                    "session": id,
                    "cwd": f.cwd,
                    "warning": f.warning,
                    "running": running,
                }));
            }
            if action == "starting" {
                st.sup.entry(name).or_default().retry_at = None;
            }
        }
        for seat in &restart {
            self.begin_stop(&mut st, seat, Intent::Restart);
        }
        st.save_dirty = true;
        st.settings_due = true;
        drop(st);
        self.save_now();
        self.sup_kick.notify_one();
        let (policy, source) = (self.local_policy(), self.policy_source());
        json!({
            "seats": out,
            "adopted": adopted,
            "settings_changed": changed,
            "local_policy": policy,
            "local_policy_source": source,
        })
    }

    fn targets(&self, seats: &[String]) -> Result<Vec<String>, String> {
        let st = self.lock();
        if seats.is_empty() {
            return Ok(st.saved.managed.keys().cloned().collect());
        }
        for s in seats {
            if !st.saved.managed.contains_key(s) {
                return Err(format!(
                    "seat {s:?} is not supervised by this node (see amail ps)"
                ));
            }
        }
        Ok(seats.to_vec())
    }

    /// Wait until none of `seats` has a process (or the time is up).
    async fn wait_stopped(&self, seats: &[String]) -> Vec<String> {
        let deadline = Instant::now() + self.cfg.kill_grace + Duration::from_secs(10);
        loop {
            let running: Vec<String> = {
                let st = self.lock();
                seats
                    .iter()
                    .filter(|s| {
                        st.saved.managed.get(*s).is_some_and(|m| m.pid.is_some())
                            || st.sup.get(*s).is_some_and(|r| r.opening || r.exiting)
                    })
                    .cloned()
                    .collect()
            };
            if running.is_empty() || Instant::now() > deadline {
                return running;
            }
            self.sup_kick.notify_one();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub(super) async fn sup_down(&self, seats: Vec<String>) -> Value {
        let targets = match self.targets(&seats) {
            Ok(t) => t,
            Err(e) => return json!({"error": e}),
        };
        {
            let mut st = self.lock();
            for s in &targets {
                if let Some(m) = st.saved.managed.get_mut(s) {
                    m.up = false;
                    m.dormant = false;
                    m.held = None;
                    m.failed = false;
                    m.failures = 0;
                }
                if let Some(rt) = st.sup.get_mut(s) {
                    rt.blocked = None;
                }
                self.begin_stop(&mut st, s, Intent::Down);
            }
            st.start_queue.retain(|n| !targets.contains(n));
        }
        let left = self.wait_stopped(&targets).await;
        // Close sessions none of whose seats is wanted any more.
        let (close, codex_left) = {
            let st = self.lock();
            let mut by_session: BTreeMap<String, bool> = BTreeMap::new();
            for m in st.saved.managed.values() {
                let keep = m.up || m.pid.is_some();
                *by_session.entry(m.mux_session.clone()).or_default() |= keep;
            }
            let close: Vec<String> = by_session
                .into_iter()
                .filter(|(_, keep)| !keep)
                .map(|(s, _)| s)
                .collect();
            let codex_left = st.seats.values().any(|s| {
                s.harness == Some(Harness::Codex)
                    && !matches!(s.state, SeatState::Offline | SeatState::Dormant)
            }) || st
                .saved
                .managed
                .values()
                .any(|m| m.harness == "codex" && m.pid.is_some());
            (close, codex_left)
        };
        // Forget registrations of earlier sessions of the stopped seats; the
        // current one is kept so the next start can resume it.
        {
            let mut st = self.lock();
            let keep: HashSet<String> = st
                .saved
                .managed
                .values()
                .filter_map(|m| m.resume.clone())
                .collect();
            let before = st.saved.sessions.len();
            st.saved
                .sessions
                .retain(|id, r| !targets.contains(&r.seat) || keep.contains(id));
            if st.saved.sessions.len() != before {
                st.save_dirty = true;
            }
        }
        for s in &close {
            if let Err(e) = self.mux.kill_session(s).await {
                tracing::warn!("closing {s}: {e:#}");
            }
        }
        if !codex_left {
            self.codex.shutdown().await;
        }
        self.presence_kick.notify_one();
        self.save_now();
        json!({"stopped": targets, "still_running": left, "closed_sessions": close})
    }

    pub(super) async fn sup_forget(&self, seats: Vec<String>) -> Value {
        let targets = match self.targets(&seats) {
            Ok(t) => t,
            Err(e) => return json!({"error": e}),
        };
        let _ = self.sup_down(targets.clone()).await;
        let mut st = self.lock();
        for s in &targets {
            st.saved.managed.remove(s);
            st.sup.remove(s);
            st.set_state(s, SeatState::Offline);
        }
        st.save_dirty = true;
        drop(st);
        self.save_now();
        json!({"forgotten": targets})
    }

    pub(super) async fn sup_sleep(&self, seat: &str) -> Value {
        if let Err(e) = self.targets(&[seat.to_string()]) {
            return json!({"error": e});
        }
        {
            let mut st = self.lock();
            let up = st.saved.managed.get(seat).is_some_and(|m| m.up);
            if !up {
                return json!({"error": format!("seat {seat} is down; start it with amail up or amail wake")});
            }
            st.start_queue.retain(|n| n != seat);
            // Asked for by the person: mail already waiting does not start
            // the seat again, new mail does.
            let seq = st.seats.get(seat).map_or(0, |s| s.pending.seq);
            if let Some(m) = st.saved.managed.get_mut(seat) {
                m.held = Some(seq);
            }
            st.save_dirty = true;
            self.begin_stop(&mut st, seat, Intent::Sleep);
        }
        let left = self.wait_stopped(&[seat.to_string()]).await;
        self.presence_kick.notify_one();
        let pending = self.lock().seats.get(seat).map_or(0, |s| s.pending.to);
        json!({"seat": seat, "asleep": left.is_empty(), "pending": pending})
    }

    pub(super) async fn sup_wake(&self, seat: &str) -> Value {
        if let Err(e) = self.targets(&[seat.to_string()]) {
            return json!({"error": e});
        }
        let busy = {
            let mut st = self.lock();
            if let Some(m) = st.saved.managed.get_mut(seat) {
                m.up = true;
                m.dormant = false;
                m.held = None;
                m.failed = false;
                m.failures = 0;
                m.adopt_failed = None;
            }
            if let Some(rt) = st.sup.get_mut(seat) {
                rt.retry_at = None;
            }
            st.save_dirty = true;
            self.turns_retry(&mut st, seat);
            let running = st.saved.managed.get(seat).is_some_and(|m| m.pid.is_some());
            running
                && !self.is_stuck(&st, seat)
                && st
                    .seats
                    .get(seat)
                    .is_some_and(|s| s.state == SeatState::Busy)
        };
        // The node may have missed the end of a busy seat's turn (one the
        // person interrupted fires no hook in Claude Code): look again.
        let detail = if busy {
            Some(self.recheck_busy(seat).await)
        } else {
            None
        };
        let mut st = self.lock();
        let running = st.saved.managed.get(seat).is_some_and(|m| m.pid.is_some());
        let state = st.seats.get(seat).map(|s| s.state);
        // A seat stuck at a prompt (e.g. one answered in another pane) is
        // started again; a working one is left alone.
        let action = if running && self.is_stuck(&st, seat) {
            self.begin_stop(&mut st, seat, Intent::Restart);
            "restarting"
        } else if detail.is_some() {
            if state == Some(SeatState::Busy) {
                "checked: busy"
            } else {
                "checked: idle"
            }
        } else if running {
            "running"
        } else {
            "starting"
        };
        let pending = st.seats.get(seat).map_or(0, |s| {
            if s.wants_wake(now_ms(), st.digest_every) {
                s.pending.total() + s.stash.len() as i64
            } else {
                0
            }
        });
        drop(st);
        self.sup_kick.notify_one();
        self.wake_kick.notify_one();
        json!({"seat": seat, "action": action, "detail": detail, "pending": pending})
    }

    /// Start the seat's session again (the user answered its `attention`
    /// request with `restart`): a running process is stopped and started
    /// again at once (resuming its session), a seat that is not running is
    /// started; a failed seat's failures are cleared, as `amail wake` does.
    pub(super) async fn sup_restart(&self, seat: &str) -> Value {
        if let Err(e) = self.targets(&[seat.to_string()]) {
            return json!({"error": e});
        }
        let action = {
            let mut st = self.lock();
            if let Some(m) = st.saved.managed.get_mut(seat) {
                m.up = true;
                m.dormant = false;
                m.held = None;
                m.failed = false;
                m.failures = 0;
                m.adopt_failed = None;
            }
            if let Some(rt) = st.sup.get_mut(seat) {
                rt.retry_at = None;
            }
            st.save_dirty = true;
            self.turns_retry(&mut st, seat);
            let running = st.saved.managed.get(seat).is_some_and(|m| m.pid.is_some());
            if running {
                self.begin_stop(&mut st, seat, Intent::Restart);
                "restarting"
            } else {
                "starting"
            }
        };
        self.sup_kick.notify_one();
        self.presence_kick.notify_one();
        json!({"seat": seat, "action": action})
    }

    pub(super) fn sup_ps(&self) -> Value {
        let (policy, source) = (self.local_policy(), self.policy_source());
        let st = self.lock();
        let now = Instant::now();
        let mut rows: Vec<PsRow> = st
            .saved
            .managed
            .iter()
            .map(|(name, m)| {
                let s = st.seats.get(name);
                let rt = st.sup.get(name).cloned().unwrap_or_default();
                let running = m.pid.is_some();
                let state = if m.failed {
                    "failed".to_string()
                } else if rt.stopping.is_some() {
                    "stopping".into()
                } else if !running && rt.retry_at.is_some_and(|t| t > now) {
                    "restarting".into()
                } else if rt.queued || rt.opening || (rt.starting_since.is_some() && !rt.hooked) {
                    "starting".into()
                } else if running {
                    match s.map(|s| s.state) {
                        Some(SeatState::Offline) | None => "starting".into(),
                        Some(x) => x.as_str().to_string(),
                    }
                } else if m.dormant {
                    "dormant".into()
                } else {
                    "offline".into()
                };
                let attention = attention_of(&st, name);
                let (tag, hint) = attention.unzip();
                PsRow {
                    seat: name.clone(),
                    group: m.group.clone(),
                    harness: m.harness.clone(),
                    state,
                    mux_session: m.mux_session.clone(),
                    pane: m.pane,
                    pid: m.pid,
                    pending: s.map_or(0, |s| s.pending.total() + s.stash.len() as i64),
                    last_activity: s
                        .filter(|s| s.last_activity > 0)
                        .map(|s| ts(s.last_activity)),
                    attention: tag,
                    hint,
                    restarts: m.failures,
                    note: s.and_then(|s| s.ho.note_word()).map(str::to_string),
                    waiting_for: s.and_then(|s| s.rq.waiting_for),
                }
            })
            .collect();
        rows.sort_by(|a, b| (&a.mux_session, &a.seat).cmp(&(&b.mux_session, &b.seat)));
        json!({
            "connected": st.connected,
            "seats": rows,
            "local_policy": policy,
            "local_policy_source": source,
        })
    }

    pub(super) fn sup_locate(&self, target: &str) -> Value {
        let st = self.lock();
        let (session, tab, pane) = if let Some(m) = st.saved.managed.get(target) {
            (m.mux_session.clone(), Some(m.tab.clone()), m.pane)
        } else if let Some(m) = st
            .saved
            .managed
            .values()
            .find(|m| m.group.as_deref().unwrap_or("main") == target)
        {
            (m.mux_session.clone(), None, None)
        } else {
            return json!({"error": format!("no supervised seat or group {target:?} (see amail ps)")});
        };
        json!({
            "session": session,
            "tab": tab,
            "pane": pane,
            "plan": self.mux.attach_plan(&session, tab.as_deref(), pane),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_recovery_rejects_each_supervisor_transition() {
        assert!(!SupRt::default().quiet_recovery_blocked());
        for barrier in [
            "queued", "opening", "starting", "slot", "stopping", "retry", "exiting", "probing",
            "blocked",
        ] {
            let mut s = SupRt::default();
            match barrier {
                "queued" => s.queued = true,
                "opening" => s.opening = true,
                "starting" => s.starting_since = Some(Instant::now()),
                "slot" => s.holds_slot = true,
                "stopping" => s.stopping = Some((Intent::Sleep, Instant::now(), false)),
                "retry" => s.retry_at = Some(Instant::now()),
                "exiting" => s.exiting = true,
                "probing" => s.probing = true,
                "blocked" => s.blocked = Some(("blocked".into(), "needs person".into())),
                _ => unreachable!(),
            }
            assert!(s.quiet_recovery_blocked(), "{barrier}");
            let report = s.quiet_recovery_diagnostics();
            let key = if barrier == "blocked" {
                "support_block"
            } else {
                barrier
            };
            assert_eq!(report["blocked"], true);
            assert_eq!(report[key], true, "diagnostic lost {barrier}");
            assert_eq!(
                report
                    .as_object()
                    .unwrap()
                    .values()
                    .filter(|v| v.as_bool() == Some(true))
                    .count(),
                2
            );
        }
    }

    #[test]
    fn screens_that_need_a_person() {
        // As Claude Code 2.1 shows it.
        let real = "Accessing workspace:\n/w/a\nQuick safety check: Is this a project you \
                    created or one you trust?\n❯ No, exit\n  Yes, I trust this folder";
        assert_eq!(
            classify_screen(real, "a", "/w/a").map(|x| x.0),
            Some("trust-prompt".into())
        );
        let trust =
            "Do you trust the files in this folder?\n/w/a\n❯ 1. Yes, proceed\n  2. No, exit";
        assert_eq!(
            classify_screen(trust, "a", "/w/a").map(|x| x.0),
            Some("trust-prompt".into())
        );
        let login = "Invalid API key · Please run /login";
        assert_eq!(
            classify_screen(login, "a", "/w").map(|x| x.0),
            Some("login".into())
        );
        let daemon = "  Background server has incompatible feature settings\n  This session \
                      requires api_key_model_discovery to be disabled\n  1. Run without daemon \
                      this time\n  2. Restart with these settings\n› 3. Cancel";
        assert_eq!(
            classify_screen(daemon, "a", "/w").map(|x| x.0),
            Some("codex-daemon".into())
        );
        assert_eq!(classify_screen("> ready", "a", "/w"), None);
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
    }
}
