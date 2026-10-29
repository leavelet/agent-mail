//! Turns: which mail a seat's current turn showed, and what happens when a
//! turn fails.
//!
//! Every claim the node makes has an idempotency key; the node remembers
//! which claim took each message ([`TurnTrack::keys`]) and which messages
//! the seat's current turn showed ([`TurnTrack::shown`]). A turn ends in
//! one of three ways:
//!
//! - **normally** (`Stop` with nothing left to show): the mail it showed
//!   counts as delivered, and the seat's failure count is cleared;
//! - **interrupted by the person** (`Interrupt`; a `SessionEnd`; a new
//!   turn that starts without an end of the last one being seen): the mail
//!   counts as delivered; the failure count stays;
//! - **failed**: the harness reported an error (Claude Code `StopFailure`,
//!   a dsh `turn/end` with an error, which the dsh wrapper reports as
//!   `TurnFailed`, a Codex turn whose status is `failed`), or the session
//!   process ended during the turn. The deliveries the turn showed go back
//!   to "not notified, not read" on the server (`POST /v1/claim/release`
//!   with the claim's key), so the sender sees the truth and the mail is
//!   delivered again through the usual wake-up, after a backoff that
//!   doubles per failure. After [`Config::turn_fail_limit`] failures in a
//!   row the node stops waking the seat for it (the mail stays unread and
//!   comes with the seat's next turn), keeps the seat flagged, and tells
//!   the person once by system mail. A manual retry (`amail wake`, or
//!   Restart on the seat's attention card) clears the seat's failure
//!   record: the flag goes, the count starts again at the next failure,
//!   and that failure flags the seat (and opens an attention card) anew.
//!
//! Codex hooks report no failed turn, so for Codex seats the node asks the
//! shared app-server how the turn ended (`thread/turns/list`): shortly
//! after `Stop` or `Interrupt`, and when a busy seat has sent no hook for
//! [`Config::turn_quiet`] (a failed Codex turn may end without any hook).
//!
//! The seat's presence carries the failure ([`amail_proto::TurnFailure`])
//! until a turn ends normally.

use super::*;
use crate::adapter::{QuietTurnContext, QuietTurnProof, TranscriptTurn, TurnInfo, TurnStatus};
use crate::persist::{Release, TurnTrack};
use amail_proto::{NodeNotice, TurnFailure};

/// Most claimed messages whose claim the node remembers per seat.
const MAX_KEYS: usize = 2000;
/// Longest failure reason kept (characters).
const MAX_REASON: usize = amail_proto::limits::MAX_HINT_CHARS - 100;
/// How often a deferred Codex check is tried before the turn counts as
/// ended the way its hook said.
const CHECK_TRIES: u8 = 5;

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TurnEnd {
    Normal,
    Person,
    Failed(String),
}

/// A turn whose end is checked with the harness (see the module docs).
#[derive(Debug, Clone)]
pub(super) struct TurnCheck {
    pub(super) due: Instant,
    /// When the hook that ended it came (Unix ms).
    stopped: i64,
    /// How the hook said it ended.
    end: TurnEnd,
    shown: Vec<i64>,
    tries: u8,
}

/// A failure reason as kept and shown: one line, at most [`MAX_REASON`]
/// characters.
pub(super) fn clean_reason(reason: &str) -> String {
    let line = amail_proto::text::line(reason.trim());
    let mut out: String = line.chars().take(MAX_REASON).collect();
    if line.chars().count() > MAX_REASON {
        out.push_str(" ...");
    }
    if out.is_empty() {
        out = "the turn ended with an error".into();
    }
    out
}

impl State {
    fn turn(&mut self, seat: &str) -> &mut TurnTrack {
        self.save_dirty = true;
        self.saved.turns.entry(seat.to_string()).or_default()
    }

    fn tidy_turn(&mut self, seat: &str) {
        if self.saved.turns.get(seat).is_some_and(TurnTrack::is_empty) {
            self.saved.turns.remove(seat);
        }
    }

    /// The claim with `key` took `entries` for `seat`.
    pub(super) fn note_claimed(&mut self, seat: &str, key: &str, entries: &[InboxEntry]) {
        if entries.is_empty() {
            return;
        }
        let t = self.turn(seat);
        for e in entries {
            t.keys.insert(e.message.id, key.to_string());
        }
        while t.keys.len() > MAX_KEYS {
            t.keys.pop_first();
        }
    }

    /// `entries` were shown in the seat's current turn.
    pub(super) fn note_shown(&mut self, seat: &str, entries: &[InboxEntry]) {
        if entries.is_empty() {
            return;
        }
        let t = self.turn(seat);
        for e in entries {
            if !t.shown.contains(&e.message.id) {
                t.shown.push(e.message.id);
            }
        }
    }

    /// The answer that would have shown `entries` was not delivered.
    pub(super) fn unshow(&mut self, seat: &str, entries: &[InboxEntry]) {
        if entries.is_empty() || !self.saved.turns.contains_key(seat) {
            return;
        }
        let t = self.turn(seat);
        t.shown
            .retain(|id| !entries.iter().any(|e| e.message.id == *id));
        self.tidy_turn(seat);
    }

    /// Mail shown in a turn that ended without failing counts as
    /// delivered: forget its claims.
    fn settle(&mut self, seat: &str, shown: &[i64]) {
        if shown.is_empty() {
            return;
        }
        let t = self.turn(seat);
        for id in shown {
            t.keys.remove(id);
        }
        self.tidy_turn(seat);
    }

    /// The failure shown in presence, if the seat's last turns failed.
    pub(super) fn turn_failure(&self, seat: &str, limit: u32) -> Option<TurnFailure> {
        let t = self.saved.turns.get(seat).filter(|t| t.failures > 0)?;
        Some(TurnFailure {
            reason: t.reason.clone().unwrap_or_default(),
            at: ts(t.failed_at.unwrap_or(0)),
            count: t.failures,
            retrying: t.failures < limit || t.manual_retry,
        })
    }

    /// Forget the seat's failed turns (a manual retry). Returns whether
    /// there were any. The claims the node remembers stay.
    pub(super) fn reset_turn_failures(&mut self, seat: &str) -> bool {
        let Some(t) = self.saved.turns.get_mut(seat).filter(|t| t.failures > 0) else {
            return false;
        };
        t.failures = 0;
        t.reason = None;
        t.failed_at = None;
        t.noticed = false;
        t.manual_retry = false;
        self.save_dirty = true;
        self.tidy_turn(seat);
        self.dirty_presence.insert(seat.to_string());
        true
    }

    /// The node does not wake the seat for mail: too many failed turns.
    pub(super) fn turns_exhausted(&self, seat: &str, limit: u32) -> bool {
        self.saved
            .turns
            .get(seat)
            .is_some_and(|t| t.failures >= limit && !t.manual_retry)
    }
}

impl Node {
    /// A new turn starts: mail shown in the last one, whose end was not
    /// seen, counts as delivered.
    pub(super) fn turn_started(&self, st: &mut State, seat: &str) {
        let shown = match st.saved.turns.get_mut(seat) {
            Some(t) if !t.shown.is_empty() => std::mem::take(&mut t.shown),
            _ => return,
        };
        st.settle(seat, &shown);
    }

    /// The seat's current turn ended (see the module docs).
    pub(super) fn turn_ended(&self, st: &mut State, seat: &str, end: TurnEnd) {
        let shown = match st.saved.turns.get_mut(seat) {
            Some(t) => {
                st.save_dirty = true;
                std::mem::take(&mut t.shown)
            }
            None => Vec::new(),
        };
        let checks = st
            .seats
            .get(seat)
            .and_then(|s| s.harness)
            .is_some_and(|h| self.adapter(h).checks_turns());
        if checks && !matches!(end, TurnEnd::Failed(_)) {
            let s = st.seat(seat);
            if let Some(old) = s.turn_check.take() {
                // An earlier check is still open: its turn counts as ended
                // the way its hook said.
                let (old_end, old_shown) = (old.end, old.shown);
                st.seat(seat).turn_check = Some(TurnCheck {
                    due: Instant::now() + self.cfg.turn_check_delay,
                    stopped: now_ms(),
                    end,
                    shown,
                    tries: 0,
                });
                self.apply_turn_end(st, seat, old_shown, old_end);
            } else {
                s.turn_check = Some(TurnCheck {
                    due: Instant::now() + self.cfg.turn_check_delay,
                    stopped: now_ms(),
                    end,
                    shown,
                    tries: 0,
                });
            }
            self.check_kick.notify_one();
            return;
        }
        self.apply_turn_end(st, seat, shown, end);
    }

    /// Settle or release the mail `shown` in a turn that ended with `end`.
    fn apply_turn_end(&self, st: &mut State, seat: &str, shown: Vec<i64>, end: TurnEnd) {
        let limit = self.cfg.turn_fail_limit;
        // Logged once per turn: a session's end also ends a turn that was
        // over already.
        let open = &mut st.seat(seat).ho.turns_open;
        if *open > 0 {
            *open -= 1;
            let text = match &end {
                TurnEnd::Normal => "turn ended".to_string(),
                TurnEnd::Person => {
                    "turn ended by the person (interrupted, or the session ended)".into()
                }
                TurnEnd::Failed(r) => format!("turn failed: {}", clean_reason(r)),
            };
            st.log(seat, "turn_end", text);
        }
        match end {
            TurnEnd::Person => st.settle(seat, &shown),
            TurnEnd::Normal => {
                st.settle(seat, &shown);
                let failed = st.saved.turns.get(seat).is_some_and(|t| t.failures > 0);
                if failed {
                    tracing::info!("seat {seat}: a turn ended normally; clearing its failures");
                    let t = st.turn(seat);
                    t.failures = 0;
                    t.reason = None;
                    t.failed_at = None;
                    t.noticed = false;
                    t.manual_retry = false;
                    st.tidy_turn(seat);
                    st.dirty_presence.insert(seat.to_string());
                    self.presence_kick.notify_one();
                }
                st.seat(seat).turn_retry_at = None;
            }
            TurnEnd::Failed(reason) => {
                let reason = clean_reason(&reason);
                let harness = st
                    .seats
                    .get(seat)
                    .and_then(|s| s.harness)
                    .map(|h| h.as_str().to_string());
                let t = st.turn(seat);
                let mut by_key: BTreeMap<String, Vec<i64>> = BTreeMap::new();
                for id in &shown {
                    if let Some(k) = t.keys.remove(id) {
                        by_key.entry(k).or_default().push(*id);
                    }
                }
                t.failures = t.failures.saturating_add(1);
                t.reason = Some(reason.clone());
                t.failed_at = Some(now_ms());
                t.manual_retry = false;
                let failures = t.failures;
                let exhausted = failures >= limit;
                let notice = exhausted && !t.noticed;
                if notice {
                    t.noticed = true;
                }
                let n = by_key.values().map(Vec::len).sum::<usize>();
                tracing::warn!(
                    "seat {seat}: turn failed ({failures} in a row): {reason}; {n} message(s) \
                     it showed will be delivered again{}",
                    if exhausted {
                        " with its next turn (not waking it: too many failures)"
                    } else {
                        ""
                    }
                );
                for (key, messages) in by_key {
                    st.saved.releases.push(Release {
                        seat: seat.to_string(),
                        key,
                        messages,
                    });
                }
                if notice {
                    st.turn_notices.push(NodeNotice::TurnFailures {
                        seat: seat.to_string(),
                        count: failures,
                        harness,
                        error: reason,
                    });
                }
                let backoff = self
                    .cfg
                    .turn_retry_backoff
                    .saturating_mul(2u32.saturating_pow(failures.saturating_sub(1)))
                    .min(self.cfg.wake_backoff_max);
                let s = st.seat(seat);
                s.turn_retry_at = (!exhausted).then(|| Instant::now() + backoff);
                s.wake_due = None;
                st.dirty_presence.insert(seat.to_string());
                self.presence_kick.notify_one();
                self.release_kick.notify_one();
                self.wake_kick.notify_one();
            }
        }
    }

    /// The person woke or restarted the seat: forget its failed turns, so
    /// its mail is delivered again (also after too many failures) and a
    /// failure after this counts from one and flags the seat anew.
    pub(super) fn turns_retry(&self, st: &mut State, seat: &str) {
        if st.reset_turn_failures(seat) {
            tracing::info!("seat {seat}: woken or restarted by the person; clearing its failures");
            self.presence_kick.notify_one();
            self.wake_kick.notify_one();
        }
        st.seat(seat).turn_retry_at = None;
    }

    /// Release the claims of failed turns on the server, retrying while
    /// it cannot be reached.
    pub(super) async fn release_loop(self: Arc<Self>) {
        let mut backoff = Duration::from_millis(200);
        loop {
            let todo: Vec<Release> = self.lock().saved.releases.clone();
            if todo.is_empty() {
                backoff = Duration::from_millis(200);
                self.release_kick.notified().await;
                continue;
            }
            let mut failed = false;
            for r in todo {
                let done = match self.server.release(&r.seat, &r.key, &r.messages).await {
                    Ok(resp) => {
                        if resp.released.len() < r.messages.len() {
                            tracing::warn!(
                                "seat {}: {} of {} message(s) of a failed turn could not be \
                                 marked not read again (acknowledged or deleted meanwhile, \
                                 or the claim is too old)",
                                r.seat,
                                r.messages.len() - resp.released.len(),
                                r.messages.len()
                            );
                        }
                        let mut st = self.lock();
                        st.dirty_pending.insert(r.seat.clone());
                        self.refresh_kick.notify_one();
                        drop(st);
                        true
                    }
                    Err(e) if crate::server::is_permanent(&e) => {
                        tracing::warn!("seat {}: releasing mail of a failed turn: {e:#}", r.seat);
                        true
                    }
                    Err(e) => {
                        tracing::debug!("seat {}: releasing mail: {e:#}", r.seat);
                        failed = true;
                        false
                    }
                };
                if done {
                    let mut st = self.lock();
                    if let Some(i) = st.saved.releases.iter().position(|x| *x == r) {
                        st.saved.releases.remove(i);
                    }
                    st.save_dirty = true;
                }
            }
            if failed {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
    }

    /// Ask the harness how turns ended, for adapters whose hooks do not
    /// report failed turns, and look for turns that ended without any hook
    /// (see the module docs).
    pub(super) async fn turn_check_loop(self: Arc<Self>) {
        loop {
            let now = Instant::now();
            let quiet = self.cfg.turn_quiet;
            let mut next = now + quiet.min(Duration::from_secs(5));
            let mut todo: Vec<(String, Harness, String, bool)> = Vec::new();
            let mut quiet_seats: Vec<QuietProbe> = Vec::new();
            {
                let mut st = self.lock();
                let now_wall = now_ms();
                let panes: HashMap<String, (String, u32)> = st
                    .saved
                    .managed
                    .iter()
                    .filter(|(_, m)| m.pid.is_some())
                    .filter_map(|(n, m)| Some((n.clone(), (m.mux_session.clone(), m.pane?))))
                    .collect();
                let transcripts: HashMap<String, PathBuf> = st
                    .saved
                    .transcripts
                    .iter()
                    .map(|(n, t)| (n.clone(), PathBuf::from(&t.path)))
                    .collect();
                for (name, s) in st.seats.iter_mut() {
                    let Some(h) = s.harness else {
                        continue;
                    };
                    let adapter = self.adapter(h);
                    let quiet_for = (now_wall - s.last_activity).max(0) as u64;
                    let due = (s.state == SeatState::Busy
                        || (s.restore_pending && s.state == SeatState::Idle))
                        && quiet_for >= quiet.as_millis() as u64
                        && s.turn_quiet_checked.is_none_or(|t| now >= t + quiet);
                    if adapter.finds_interrupts() {
                        if due {
                            s.turn_quiet_checked = Some(now);
                            let screen_due =
                                quiet_for >= self.cfg.screen_check_after.as_millis() as u64;
                            quiet_seats.push(QuietProbe {
                                seat: name.clone(),
                                harness: h,
                                busy_since: s.since,
                                last_hook: s.last_activity,
                                transcript: transcripts.get(name).cloned(),
                                pane: panes.get(name).cloned().filter(|_| screen_due),
                            });
                        }
                        continue;
                    }
                    if !adapter.checks_turns() {
                        continue;
                    }
                    let Some(target) = s.session.as_ref().and_then(|x| x.target.clone()) else {
                        continue;
                    };
                    if let Some(c) = &s.turn_check {
                        if c.due <= now {
                            todo.push((name.clone(), h, target, true));
                        } else {
                            next = next.min(c.due);
                        }
                        continue;
                    }
                    // A busy seat without hooks for a while.
                    if due {
                        s.turn_quiet_checked = Some(now);
                        todo.push((name.clone(), h, target, false));
                    }
                }
            }
            for (seat, h, target, deferred) in todo {
                if !deferred
                    && self
                        .lock()
                        .seats
                        .get(&seat)
                        .is_some_and(|s| s.restore_pending)
                {
                    self.recover_completed_quiet_turn(&seat, h, &target, None)
                        .await;
                    continue;
                }
                let turns = match self.adapter(h).last_turns(&target).await {
                    Ok(t) => Some(t),
                    Err(e) => {
                        tracing::debug!("seat {seat}: asking how its turn ended: {e:#}");
                        if !deferred {
                            self.recover_completed_quiet_turn(&seat, h, &target, Some(&e))
                                .await;
                            continue;
                        }
                        None
                    }
                };
                let mut st = self.lock();
                if deferred {
                    self.finish_check(&mut st, &seat, turns);
                } else if let Some(t) = turns.as_deref().and_then(|t| t.first()) {
                    self.quiet_check(&mut st, &seat, t);
                }
            }
            for p in quiet_seats {
                if let Look::Ended(why) = self.look_at_quiet_seat(&p, false).await {
                    self.end_quiet_turn(&p, &why);
                }
            }
            tokio::select! {
                _ = self.check_kick.notified() => {}
                _ = tokio::time::sleep_until(next.into()) => {}
            }
        }
    }

    /// Look at a busy seat that sent no hooks for a while, for a harness
    /// whose interrupted turns fire no hook (Claude Code): the end of its
    /// transcript, then (supervised seats, after
    /// [`Config::screen_check_after`]) its pane. Returns why its turn
    /// counts as ended, or `None` while it may be running.
    ///
    /// - The transcript decides when its last turn record is the person's
    ///   interrupt and that record is not older than the turn (so an
    ///   interrupt of an earlier turn does not count). A long tool call or
    ///   a long model answer leaves a tool call or the prompt last, which
    ///   never counts.
    /// - The pane decides only when it shows no running turn twice, at
    ///   least [`Config::turn_quiet`] apart, with no hook in between
    ///   (`now`: the person asked, two looks one second apart).
    async fn look_at_quiet_seat(&self, p: &QuietProbe, now: bool) -> Look {
        let adapter = self.adapter(p.harness).clone();
        let mut seen = Vec::new();
        if let Some(path) = p.transcript.clone() {
            let read =
                tokio::task::spawn_blocking(move || crate::transcript::read_tail(&path, 256 << 10))
                    .await
                    .ok()
                    .and_then(|r| r.ok());
            if let Some((tail, sig)) = read {
                let cached = {
                    let st = self.lock();
                    st.seats
                        .get(&p.seat)
                        .and_then(|s| s.quiet.transcript.clone())
                        .filter(|(s, _)| *s == sig)
                        .map(|(_, t)| t)
                };
                let turn = match cached {
                    Some(t) => t,
                    None => {
                        let t = adapter.transcript_turn(&tail);
                        tracing::debug!("seat {}: its transcript ends with {t:?}", p.seat);
                        if let Some(s) = self.lock().seats.get_mut(&p.seat) {
                            s.quiet.transcript = Some((sig, t.clone()));
                        }
                        t
                    }
                };
                seen.push(match turn {
                    TranscriptTurn::Running => "its transcript shows the turn running",
                    TranscriptTurn::Interrupted(_) => {
                        "its transcript ends with an earlier interrupt"
                    }
                    TranscriptTurn::Unknown => "its transcript says nothing about the turn",
                });
                if let TranscriptTurn::Interrupted(at) = turn {
                    // Clocks of the same host; the margin covers the
                    // transcript's write lagging behind the hook.
                    let fresh = match at {
                        Some(ms) => ms >= p.busy_since - 2000,
                        None => (sig.1 / 1_000_000) as i64 >= p.busy_since,
                    };
                    if fresh {
                        return Look::Ended(
                            "the person interrupted it (its transcript says so)".into(),
                        );
                    }
                }
            }
        }
        let running = |seen: Vec<&str>| {
            Look::Running(if seen.is_empty() {
                "nothing shows it ended".to_string()
            } else {
                seen.join("; ")
            })
        };
        let Some((session, pane)) = p.pane.clone() else {
            return running(seen);
        };
        let apart = if now {
            Duration::from_millis(1000)
        } else {
            self.cfg.turn_quiet
        };
        let looks = if now { 2 } else { 1 };
        for i in 0..looks {
            if i > 0 {
                tokio::time::sleep(apart).await;
            }
            let Some(on_screen) = self
                .mux
                .screen(&session, pane)
                .await
                .ok()
                .and_then(|screen| adapter.screen_running(&screen))
            else {
                return running(seen);
            };
            tracing::debug!(
                "seat {}: its pane shows {}",
                p.seat,
                if on_screen {
                    "a running turn"
                } else {
                    "no running turn"
                }
            );
            let now_wall = now_ms();
            let mut st = self.lock();
            let Some(s) = st.seats.get_mut(&p.seat) else {
                return running(seen);
            };
            if on_screen || s.last_activity != p.last_hook || s.state != SeatState::Busy {
                s.quiet.screen_idle_at = None;
                if on_screen {
                    seen.push("its pane shows the turn running");
                }
                return running(seen);
            }
            // A look from before the seat's last hook does not count.
            match s
                .quiet
                .screen_idle_at
                .filter(|(_, hook)| *hook == p.last_hook)
            {
                Some((first, _)) if now_wall - first >= apart.as_millis() as i64 => {
                    return Look::Ended(format!(
                        "no hook for {} s and its pane shows no running turn",
                        (now_wall - p.last_hook).max(0) / 1000
                    ));
                }
                Some(_) => {}
                None => s.quiet.screen_idle_at = Some((now_wall, p.last_hook)),
            }
        }
        seen.push("its pane shows no running turn (not yet long enough)");
        running(seen)
    }

    /// End the turn of a quiet busy seat that [`Node::look_at_quiet_seat`]
    /// found ended: as one the person interrupted (its mail counts as
    /// delivered), unless a hook came in the meantime.
    fn end_quiet_turn(&self, p: &QuietProbe, why: &str) {
        let mut st = self.lock();
        let unchanged = st.seats.get(&p.seat).is_some_and(|s| {
            s.state == SeatState::Busy && s.last_activity == p.last_hook && s.since == p.busy_since
        });
        if !unchanged {
            return;
        }
        tracing::info!("seat {}: its turn ended without a hook: {why}", p.seat);
        st.set_state(&p.seat, SeatState::Idle);
        let shown = match st.saved.turns.get_mut(&p.seat) {
            Some(t) => std::mem::take(&mut t.shown),
            None => Vec::new(),
        };
        st.save_dirty = true;
        self.apply_turn_end(&mut st, &p.seat, shown, TurnEnd::Person);
        drop(st);
        self.presence_kick.notify_one();
        self.wake_kick.notify_one();
    }

    /// `amail wake` on a seat the node believes busy: look again at once
    /// (as the quiet checks do, without waiting) and say what was found.
    pub(super) async fn recheck_busy(&self, seat: &str) -> String {
        let (h, probe, target) = {
            let st = self.lock();
            let Some(s) = st.seats.get(seat) else {
                return "no session".into();
            };
            let Some(h) = s.harness else {
                return "no session".into();
            };
            if s.state != SeatState::Busy {
                return s.state.as_str().to_string();
            }
            let pane = st
                .saved
                .managed
                .get(seat)
                .filter(|m| m.pid.is_some())
                .and_then(|m| Some((m.mux_session.clone(), m.pane?)));
            (
                h,
                QuietProbe {
                    seat: seat.to_string(),
                    harness: h,
                    busy_since: s.since,
                    last_hook: s.last_activity,
                    transcript: st
                        .saved
                        .transcripts
                        .get(seat)
                        .map(|t| PathBuf::from(&t.path)),
                    pane,
                },
                s.session.as_ref().and_then(|x| x.target.clone()),
            )
        };
        let quiet_s = (now_ms() - probe.last_hook).max(0) / 1000;
        let adapter = self.adapter(h).clone();
        if adapter.finds_interrupts() {
            return match self.look_at_quiet_seat(&probe, true).await {
                Look::Ended(why) => {
                    self.end_quiet_turn(&probe, &why);
                    format!("was busy, but its turn had ended ({why}); now idle")
                }
                Look::Running(what) => {
                    format!("busy: its turn is running (last hook {quiet_s} s ago; {what})")
                }
            };
        }
        if adapter.checks_turns()
            && let Some(target) = target
        {
            if self
                .lock()
                .seats
                .get(seat)
                .is_some_and(|s| s.restore_pending)
            {
                return if self
                    .recover_completed_quiet_turn(seat, h, &target, None)
                    .await
                {
                    "restored session has proven normal completion; now idle".into()
                } else {
                    "restored session completion is not yet proven; automatic mail wakes remain held".into()
                };
            }
            for i in 0..2 {
                if i > 0 {
                    tokio::time::sleep(Duration::from_millis(1000)).await;
                }
                let turns = match adapter.last_turns(&target).await {
                    Ok(turns) => turns,
                    Err(e) => {
                        if self
                            .recover_completed_quiet_turn(seat, h, &target, Some(&e))
                            .await
                        {
                            return "was busy, but its normal completion is independently evidenced; now idle".into();
                        }
                        return "busy: turn history is unavailable and completion is not yet proven".into();
                    }
                };
                let Some(t) = turns.first() else {
                    break;
                };
                let mut st = self.lock();
                if self.quiet_check(&mut st, seat, t) {
                    return format!(
                        "was busy, but {} says its last turn had ended ({}); now idle",
                        h.as_str(),
                        status_word(&t.status)
                    );
                }
            }
            return format!("busy: its turn is running (last hook {quiet_s} s ago)");
        }
        format!("busy: its turn is running (last hook {quiet_s} s ago)")
    }

    /// Use the adapter's narrow fallback, then revalidate the original
    /// registration, process identities and hook snapshot under the node lock.
    /// The existing quiet_check still requires two positive observations.
    async fn recover_completed_quiet_turn(
        &self,
        seat: &str,
        harness: Harness,
        target: &str,
        error: Option<&anyhow::Error>,
    ) -> bool {
        let probe = {
            let mut st = self.lock();
            let Some(probe) = QuietRecovery::capture(&st, seat, harness, target) else {
                clear_quiet_verdict(&mut st, seat);
                return false;
            };
            probe
        };
        let result = self
            .adapter(harness)
            .completed_quiet_turn(&probe.context, error)
            .await;
        let mut st = self.lock();
        probe.commit(&mut st, seat, harness, result, |st, turn| {
            self.quiet_check(st, seat, turn)
        })
    }

    /// Settle a deferred check with what the harness says.
    fn finish_check(&self, st: &mut State, seat: &str, turns: Option<Vec<TurnInfo>>) {
        let Some(mut c) = st.seats.get_mut(seat).and_then(|s| s.turn_check.take()) else {
            return;
        };
        let stopped_s = c.stopped.div_euclid(1000);
        // The turn the hook ended: the newest one, unless that one started
        // after the hook (a new turn).
        let turn = turns.as_ref().and_then(|ts| {
            ts.iter().find(|t| {
                !(t.status == TurnStatus::InProgress && t.started_at.is_some_and(|s| s > stopped_s))
            })
        });
        let end = match turn.map(|t| &t.status) {
            Some(TurnStatus::InProgress) if c.tries + 1 < CHECK_TRIES => {
                // Not recorded as ended yet: ask again.
                c.tries += 1;
                c.due = Instant::now() + self.cfg.turn_check_delay;
                st.seat(seat).turn_check = Some(c);
                self.check_kick.notify_one();
                return;
            }
            Some(TurnStatus::Failed(msg)) => TurnEnd::Failed(msg.clone()),
            Some(TurnStatus::Interrupted) => TurnEnd::Person,
            _ => c.end.clone(),
        };
        self.apply_turn_end(st, seat, std::mem::take(&mut c.shown), end);
    }

    /// A busy seat sent no hook for a while: if its last turn ended, end
    /// it here too (Codex ends a failed turn without a hook). The same
    /// ended turn must be seen twice with no hook in between, so a turn
    /// that has just started and is not listed yet does not count as the
    /// end of the one before it. Returns true if the turn was ended.
    fn quiet_check(&self, st: &mut State, seat: &str, last: &TurnInfo) -> bool {
        let Some(s) = st.seats.get_mut(seat).filter(|s| {
            s.state == SeatState::Busy || (s.restore_pending && s.state == SeatState::Idle)
        }) else {
            return false;
        };
        let end = match &last.status {
            TurnStatus::InProgress => {
                s.quiet.verdict = None;
                return false;
            }
            TurnStatus::Completed => TurnEnd::Normal,
            TurnStatus::Interrupted => TurnEnd::Person,
            TurnStatus::Failed(m) => TurnEnd::Failed(m.clone()),
        };
        let seen = (last.id.clone(), end.clone());
        match &s.quiet.verdict {
            Some((v, at)) if *v == seen && *at >= s.last_activity => {}
            _ => {
                s.quiet.verdict = Some((seen, now_ms()));
                return false;
            }
        }
        tracing::info!("seat {seat}: its last turn ended without a hook ({end:?})");
        st.seat(seat).restore_pending = false;
        st.set_state(seat, SeatState::Idle);
        let shown = match st.saved.turns.get_mut(seat) {
            Some(t) => std::mem::take(&mut t.shown),
            None => Vec::new(),
        };
        st.save_dirty = true;
        self.apply_turn_end(st, seat, shown, end);
        self.presence_kick.notify_one();
        self.wake_kick.notify_one();
        true
    }
}

/// What the node found about a quiet busy seat (reset when the seat's
/// state changes).
#[derive(Debug, Clone, Default)]
pub(super) struct QuietRt {
    /// The app-server said the seat's last turn ended: `((turn id, how),
    /// when, Unix ms)`.
    verdict: Option<((Option<String>, TurnEnd), i64)>,
    /// The transcript's size and modification time at the last read, and
    /// what it said then.
    transcript: Option<((u64, i128), TranscriptTurn)>,
    /// When the pane first showed no running turn, and the seat's last
    /// hook then (Unix ms).
    screen_idle_at: Option<(i64, i64)>,
}

/// What [`Node::look_at_quiet_seat`] found.
enum Look {
    /// The turn ended; why.
    Ended(String),
    /// It may be running; what was seen.
    Running(String),
}

/// A quiet busy seat to look at.
#[derive(Debug, Clone)]
pub(super) struct QuietProbe {
    seat: String,
    harness: Harness,
    /// When the seat became busy, and its last hook (Unix ms).
    busy_since: i64,
    last_hook: i64,
    transcript: Option<PathBuf>,
    /// `(zellij session, pane)` when the pane is to be looked at.
    pane: Option<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QuietRecovery {
    context: QuietTurnContext,
    session_pid: Option<(u32, Option<u64>)>,
    managed_pid: Option<(Option<u32>, Option<u64>)>,
}

/// Local eligibility only. No RPC, transcript read, state change, failure
/// reset, or delivery action is performed by this diagnostic.
pub(super) fn quiet_recovery_diagnostics(st: &State, seat: &str) -> serde_json::Value {
    let Some(s) = st
        .seats
        .get(seat)
        .filter(|s| s.harness == Some(Harness::Codex))
    else {
        return serde_json::Value::Null;
    };
    let eligible = s
        .session
        .as_ref()
        .and_then(|s| s.target.as_deref())
        .is_some_and(|target| QuietRecovery::capture(st, seat, Harness::Codex, target).is_some());
    serde_json::json!({
        "capture_eligible": eligible,
        "turn_check_pending": s.turn_check.is_some(),
        "wake_in_flight": s.wake_out.is_some(),
        "has_first_observation": s.quiet.verdict.is_some(),
        "supervisor": st.sup.get(seat).map(|s| s.quiet_recovery_diagnostics()),
    })
}

impl QuietRecovery {
    fn capture(st: &State, seat: &str, harness: Harness, target: &str) -> Option<Self> {
        let s = st.seats.get(seat)?;
        if !(s.state == SeatState::Busy || (s.restore_pending && s.state == SeatState::Idle))
            || s.harness != Some(harness)
            || s.turn_check.is_some()
            || s.wake_out.is_some()
            || st.sup.get(seat).is_some_and(|s| s.quiet_recovery_blocked())
            || st.saved.turns.get(seat).is_some_and(|t| {
                t.failures != 0 || t.reason.is_some() || t.failed_at.is_some() || t.manual_retry
            })
        {
            return None;
        }
        let session = s.session.as_ref()?;
        if session.id.as_deref() != Some(target) || session.target.as_deref() != Some(target) {
            return None;
        }
        let (pid, Some(start)) = session.pid? else {
            return None;
        };
        if !alive(pid, Some(start)) {
            return None;
        }
        let managed_pid = match st.saved.managed.get(seat) {
            Some(m)
                if !m.up
                    || m.dormant
                    || m.failed
                    || m.failures != 0
                    || m.held.is_some()
                    || m.context_lost
                    || m.pid.is_none() =>
            {
                return None;
            }
            Some(m) if m.pid != Some(pid) || m.pid_start != Some(start) => return None,
            Some(m) => Some((m.pid, m.pid_start)),
            None => None,
        };
        Some(Self {
            context: QuietTurnContext {
                target: target.into(),
                transcript: PathBuf::from(&st.saved.transcripts.get(seat)?.path),
                busy_since: s.since,
                last_hook: s.last_activity,
                restoring: s.restore_pending,
            },
            session_pid: session.pid,
            managed_pid,
        })
    }

    /// Called under the node lock immediately before quiet_check. A rejected
    /// second observation also discards the first; it can never reach the
    /// completion callback (which changes presence and schedules mail wakes).
    fn commit(
        &self,
        st: &mut State,
        seat: &str,
        harness: Harness,
        result: Result<Option<QuietTurnProof>>,
        complete: impl FnOnce(&mut State, &TurnInfo) -> bool,
    ) -> bool {
        if Self::capture(st, seat, harness, &self.context.target).as_ref() == Some(self)
            && let Ok(Some(proof)) = result
            && proof.turn.status == TurnStatus::Completed
            && proof.turn.id.as_ref().is_some_and(|id| !id.is_empty())
            && proof.file.still_current(&self.context.transcript)
        {
            return complete(st, &proof.turn);
        }
        clear_quiet_verdict(st, seat);
        false
    }
}

fn clear_quiet_verdict(st: &mut State, seat: &str) {
    if let Some(s) = st.seats.get_mut(seat) {
        s.quiet.verdict = None;
    }
}

fn status_word(s: &TurnStatus) -> &'static str {
    match s {
        TurnStatus::InProgress => "running",
        TurnStatus::Completed => "completed",
        TurnStatus::Interrupted => "interrupted",
        TurnStatus::Failed(_) => "failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manual_retry_forgets_the_failures() {
        let mut st = State::default();
        st.seat("w1");
        assert!(!st.reset_turn_failures("w1"));
        let t = st.turn("w1");
        t.failures = 4;
        t.reason = Some("model_not_found".into());
        t.failed_at = Some(1);
        t.noticed = true;
        t.keys.insert(7, "k".into());
        assert!(st.turn_failure("w1", 3).is_some());
        assert!(st.turns_exhausted("w1", 3));
        assert!(st.reset_turn_failures("w1"));
        assert!(st.turn_failure("w1", 3).is_none());
        assert!(!st.turns_exhausted("w1", 3));
        let t = &st.saved.turns["w1"];
        assert!(!t.noticed && t.reason.is_none() && t.failed_at.is_none());
        assert_eq!(t.keys.get(&7).map(String::as_str), Some("k"));
        assert!(st.dirty_presence.contains("w1"));
    }

    // Seed the first positive observation, then create the second adapter
    // proof. Tests interleave changes after this return and before commit.
    fn ready_recovery(path: &std::path::Path) -> (State, QuietRecovery, QuietTurnProof) {
        let mut st = State::default();
        let pid = std::process::id();
        let start = proc_start(pid).unwrap();
        let s = st.seat("cx");
        s.harness = Some(Harness::Codex);
        s.state = SeatState::Busy;
        s.since = 10;
        s.last_activity = 20;
        s.session = Some(SessionRt {
            id: Some("session".into()),
            target: Some("session".into()),
            pid: Some((pid, Some(start))),
        });
        s.quiet.verdict = Some(((Some("turn".into()), TurnEnd::Normal), 30));
        st.saved.transcripts.insert(
            "cx".into(),
            crate::persist::TranscriptRef {
                harness: "codex".into(),
                path: path.to_string_lossy().into_owned(),
                session_id: Some("session".into()),
                base: None,
            },
        );
        let managed = serde_json::from_value(serde_json::json!({
            "harness":"codex", "cwd":"/fixture", "mux_session":"fixture", "tab":"cx",
            "up":true, "pid":pid, "pid_start":start
        }))
        .unwrap();
        st.saved.managed.insert("cx".into(), managed);
        st.saved.turns.entry("cx".into()).or_default().shown = vec![41];
        let probe = QuietRecovery::capture(&st, "cx", Harness::Codex, "session").unwrap();
        let proof = QuietTurnProof {
            turn: TurnInfo {
                status: TurnStatus::Completed,
                started_at: Some(0),
                id: Some("turn".into()),
            },
            file: crate::adapter::QuietFileProof::new(
                path.canonicalize().unwrap(),
                &std::fs::metadata(path).unwrap(),
            ),
        };
        (st, probe, proof)
    }

    fn assert_recovery_rejected(st: &mut State, probe: &QuietRecovery, proof: QuietTurnProof) {
        assert!(st.seats["cx"].quiet.verdict.is_some());
        assert!(
            !probe.commit(st, "cx", Harness::Codex, Ok(Some(proof)), |_, _| {
                panic!("stale completion reached quiet_check/presence/wake callback")
            })
        );
        assert_eq!(st.seats["cx"].state, SeatState::Busy);
        assert!(st.seats["cx"].quiet.verdict.is_none());
        assert_eq!(st.seats["cx"].last_activity, 20);
        assert_eq!(st.seats["cx"].wakes, 0);
        assert_eq!(st.saved.turns["cx"].shown, vec![41]);
        assert!(!st.save_dirty);
    }

    #[test]
    fn quiet_diagnostics_report_eligibility_without_completing_or_clearing_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(&path, "fixture\n").unwrap();
        let (mut st, probe, _) = ready_recovery(&path);
        let before = serde_json::to_value(&st.saved).unwrap();
        let verdict = st.seats["cx"].quiet.verdict.clone();
        let report = quiet_recovery_diagnostics(&st, "cx");
        assert_eq!(report["capture_eligible"], true);
        assert_eq!(report["turn_check_pending"], false);
        assert_eq!(report["wake_in_flight"], false);
        assert_eq!(report["has_first_observation"], true);
        assert_eq!(st.seats["cx"].state, SeatState::Busy);
        assert_eq!(st.seats["cx"].quiet.verdict, verdict);
        assert_eq!(serde_json::to_value(&st.saved).unwrap(), before);
        assert_eq!(
            QuietRecovery::capture(&st, "cx", Harness::Codex, "session"),
            Some(probe)
        );

        st.saved.turns.get_mut("cx").unwrap().failures = 1;
        let failed = serde_json::to_value(&st.saved).unwrap();
        assert_eq!(
            quiet_recovery_diagnostics(&st, "cx")["capture_eligible"],
            false
        );
        assert_eq!(serde_json::to_value(&st.saved).unwrap(), failed);
        assert_eq!(st.seats["cx"].state, SeatState::Busy);
        assert_eq!(st.seats["cx"].quiet.verdict, verdict);
    }

    #[test]
    fn quiet_recovery_rejects_file_changes_after_adapter_before_commit() {
        use std::io::Write;
        for change in ["append", "replace", "truncate", "symlink"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("rollout.jsonl");
            std::fs::write(&path, "completed\n").unwrap();
            let (mut st, probe, proof) = ready_recovery(&path);
            // A current second observation can reach the completion callback.
            assert!(probe.commit(
                &mut st,
                "cx",
                Harness::Codex,
                Ok(Some(proof.clone())),
                |_, _| true
            ));
            match change {
                "append" => {
                    let mut f = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&path)
                        .unwrap();
                    f.write_all(b"new task_started\n").unwrap();
                }
                "replace" => {
                    let replacement = dir.path().join("replacement.jsonl");
                    std::fs::write(&replacement, "completed\n").unwrap();
                    std::fs::rename(replacement, &path).unwrap();
                }
                "truncate" => {
                    std::fs::write(&path, "").unwrap();
                }
                "symlink" => {
                    let moved = dir.path().join("moved.jsonl");
                    std::fs::rename(&path, &moved).unwrap();
                    std::os::unix::fs::symlink(moved, &path).unwrap();
                }
                _ => unreachable!(),
            }
            // PID, hook and registration are unchanged: only the final file
            // check can reject this exact interleaving.
            assert_eq!(
                QuietRecovery::capture(&st, "cx", Harness::Codex, "session"),
                Some(probe.clone())
            );
            assert_recovery_rejected(&mut st, &probe, proof);
        }
    }

    #[test]
    fn quiet_recovery_rejects_stop_or_hold_while_original_pid_is_alive() {
        use super::super::supervise::{Intent, SupRt};
        for barrier in ["sleep", "down", "restart", "held", "retry", "lost"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("rollout.jsonl");
            std::fs::write(&path, "completed\n").unwrap();
            let (mut st, probe, proof) = ready_recovery(&path);
            match barrier {
                "sleep" | "down" | "restart" => {
                    let intent = match barrier {
                        "sleep" => Intent::Sleep,
                        "down" => Intent::Down,
                        _ => Intent::Restart,
                    };
                    st.sup.insert("cx".into(), SupRt::stopping_for_test(intent));
                }
                "held" => st.saved.managed.get_mut("cx").unwrap().held = Some(42),
                "retry" => st.saved.managed.get_mut("cx").unwrap().failures = 1,
                "lost" => st.saved.managed.get_mut("cx").unwrap().context_lost = true,
                _ => unreachable!(),
            }
            let (pid, start) = st.seats["cx"].session.as_ref().unwrap().pid.unwrap();
            assert!(alive(pid, start));
            assert!(proof.file.still_current(&path));
            assert_recovery_rejected(&mut st, &probe, proof);
        }
    }

    #[test]
    fn quiet_recovery_does_not_clear_failed_or_replaced_sessions() {
        let mut st = State::default();
        let pid = std::process::id();
        let start = proc_start(pid).unwrap();
        let s = st.seat("cx");
        s.harness = Some(Harness::Codex);
        s.state = SeatState::Busy;
        s.since = 10;
        s.last_activity = 20;
        s.session = Some(SessionRt {
            id: Some("session".into()),
            target: Some("session".into()),
            pid: Some((pid, Some(start))),
        });
        st.saved.transcripts.insert(
            "cx".into(),
            crate::persist::TranscriptRef {
                harness: "codex".into(),
                path: "/unused/rollout.jsonl".into(),
                session_id: Some("session".into()),
                base: None,
            },
        );
        let original = QuietRecovery::capture(&st, "cx", Harness::Codex, "session").unwrap();
        st.saved.turns.entry("cx".into()).or_default().failures = 3;
        assert!(QuietRecovery::capture(&st, "cx", Harness::Codex, "session").is_none());
        st.saved.turns.remove("cx");
        st.seat("cx").session.as_mut().unwrap().pid = Some((pid, Some(start + 1)));
        assert!(QuietRecovery::capture(&st, "cx", Harness::Codex, "session").is_none());
        st.seat("cx").session.as_mut().unwrap().pid = Some((pid, Some(start)));
        st.seat("cx").last_activity += 1;
        assert_ne!(
            QuietRecovery::capture(&st, "cx", Harness::Codex, "session"),
            Some(original)
        );
        st.seat("cx").session.as_mut().unwrap().target = Some("replacement".into());
        assert!(QuietRecovery::capture(&st, "cx", Harness::Codex, "session").is_none());
    }

    #[test]
    fn reasons_are_one_short_line() {
        assert_eq!(clean_reason("  "), "the turn ended with an error");
        let r = clean_reason("401\nAPI key is invalid\u{1b}[31m");
        assert!(!r.contains('\n') && !r.contains('\u{1b}'), "{r}");
        let long = clean_reason(&"x".repeat(5000));
        assert!(long.chars().count() <= MAX_REASON + 4);
        assert!(long.ends_with(" ..."));
    }
}
