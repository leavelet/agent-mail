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
//!   the person once by system mail. `amail wake` lets the node deliver it
//!   again until the next failure.
//!
//! Codex hooks report no failed turn, so for Codex seats the node asks the
//! shared app-server how the turn ended (`thread/turns/list`): shortly
//! after `Stop` or `Interrupt`, and when a busy seat has sent no hook for
//! [`Config::turn_quiet`] (a failed Codex turn may end without any hook).
//!
//! The seat's presence carries the failure ([`amail_proto::TurnFailure`])
//! until a turn ends normally.

use super::*;
use crate::adapter::{TurnInfo, TurnStatus};
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

    /// The person woke the seat: deliver its mail again even after too
    /// many failed turns (until the next failure).
    pub(super) fn turns_retry(&self, st: &mut State, seat: &str) {
        if st.turns_exhausted(seat, self.cfg.turn_fail_limit) {
            st.turn(seat).manual_retry = true;
            st.dirty_presence.insert(seat.to_string());
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
    /// report failed turns (see the module docs).
    pub(super) async fn turn_check_loop(self: Arc<Self>) {
        loop {
            let now = Instant::now();
            let quiet = self.cfg.turn_quiet;
            let mut next = now + quiet.min(Duration::from_secs(5));
            let mut todo: Vec<(String, Harness, String, bool)> = Vec::new();
            {
                let mut st = self.lock();
                let now_wall = now_ms();
                for (name, s) in st.seats.iter_mut() {
                    let Some(h) = s.harness.filter(|h| self.adapter(*h).checks_turns()) else {
                        continue;
                    };
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
                    if s.state == SeatState::Busy {
                        let since = (now_wall - s.last_activity).max(0) as u64;
                        let checked = s.turn_quiet_checked.is_some_and(|t| now < t + quiet);
                        if since >= quiet.as_millis() as u64 && !checked {
                            s.turn_quiet_checked = Some(now);
                            todo.push((name.clone(), h, target, false));
                        }
                    }
                }
            }
            for (seat, h, target, deferred) in todo {
                let turns = match self.adapter(h).last_turns(&target).await {
                    Ok(t) => Some(t),
                    Err(e) => {
                        tracing::debug!("seat {seat}: asking how its turn ended: {e:#}");
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
            tokio::select! {
                _ = self.check_kick.notified() => {}
                _ = tokio::time::sleep_until(next.into()) => {}
            }
        }
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
    /// it here too (Codex may end a failed turn without a hook).
    fn quiet_check(&self, st: &mut State, seat: &str, last: &TurnInfo) {
        if st
            .seats
            .get(seat)
            .is_none_or(|s| s.state != SeatState::Busy)
        {
            return;
        }
        let end = match &last.status {
            TurnStatus::InProgress => return,
            TurnStatus::Completed => TurnEnd::Normal,
            TurnStatus::Interrupted => TurnEnd::Person,
            TurnStatus::Failed(m) => TurnEnd::Failed(m.clone()),
        };
        tracing::info!("seat {seat}: its last turn ended without a hook ({end:?})");
        st.set_state(seat, SeatState::Idle);
        let shown = match st.saved.turns.get_mut(seat) {
            Some(t) => std::mem::take(&mut t.shown),
            None => Vec::new(),
        };
        st.save_dirty = true;
        self.apply_turn_end(st, seat, shown, end);
        self.presence_kick.notify_one();
        self.wake_kick.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
