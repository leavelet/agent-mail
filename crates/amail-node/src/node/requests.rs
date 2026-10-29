//! Requests for the person and web control of seats (M10), the node's
//! part.
//!
//! - **Node commands.** The server sends `prompt`, `interrupt` and
//!   `attention_action` on the event stream (see
//!   [`amail_proto::NodeCommand`]). `prompt` gives the seat's session a
//!   text from the user the way a wake-up reaches it
//!   ([`crate::adapter::Adapter::send_prompt`]); it is not a wake-up, so
//!   the node neither waits for it to be confirmed nor treats the turn it
//!   starts as one. `interrupt` stops the running turn where the harness
//!   has a structured interface for it (Codex `turn/interrupt`, dsh
//!   `session/cancel`); Claude Code has none, and the server refuses the
//!   route for its seats. `attention_action` restarts a supervised seat or
//!   puts it to sleep. Each records a worklog entry (`prompt`,
//!   `interrupt`, `attention_action`).
//! - **Waiting on an answer.** A turn that ran `amail ask` (or the `ask`
//!   tool) and ended is followed by a look at the seat's open requests:
//!   when the seat made a `question`, `plan` or `merge` request that is
//!   still open, and no mail waits for it, the seat is marked as waiting
//!   for that request (presence `waiting_for`, `amail ps`). Nothing else
//!   changes: the team's idle-sleep rule puts the seat to sleep as for any
//!   idle seat, and the answer, mail `To` the seat, wakes or starts it
//!   through the usual path. The mark stays through later turns while the
//!   request is open: the node looks again at the end of every turn of a
//!   seat known to have an open request, and every
//!   [`Config::waiting_recheck`] while the seat is idle or asleep; it
//!   clears the mark when none of the seat's requests is open any more
//!   (answered, expired, withdrawn), and moves it to the newest one that
//!   is still open.
//!
//! Not built: the relay of harness permission prompts (Claude Code's
//! `PermissionRequest` hook, Codex approval requests). Every seat runs
//! unattended for now; the relay would add a command-free path here.

use super::*;
use amail_proto::{NodeCommand, RequestKind, RequestState};

/// How long a prompt for a seat without a running session waits for the
/// session the node starts for it.
const PROMPT_START_WAIT: Duration = Duration::from_secs(60);

/// How long the look for an open request waits for the turn's end to
/// settle (the end of a turn may be checked with the server or the
/// harness first).
const WAIT_SETTLE: Duration = Duration::from_secs(10);

/// A seat's part in requests for the person.
#[derive(Debug, Clone, Default)]
pub(super) struct SeatRq {
    /// The current turn ran `amail ask` or the `ask` tool.
    pub(super) asked: bool,
    /// The seat made a request that was open when the node last looked
    /// (with or without a mark: a turn that ended with mail waiting is
    /// not marked).
    pub(super) open: bool,
    /// The open request the seat waits for.
    pub(super) waiting_for: Option<i64>,
}

impl SeatRq {
    /// A turn started. The mark stays: the seat still waits for the answer
    /// while it handles other mail.
    pub(super) fn turn_started(&mut self) {
        self.asked = false;
    }

    /// Whether the end of a turn should be followed by a look at the
    /// seat's open requests.
    pub(super) fn look_at_turn_end(&mut self) -> bool {
        std::mem::take(&mut self.asked) || self.open || self.waiting_for.is_some()
    }
}

/// The kinds of request a seat makes and then waits on.
fn waited_on(kind: RequestKind) -> bool {
    matches!(
        kind,
        RequestKind::Question | RequestKind::Plan | RequestKind::Merge
    )
}

/// A prompt's text as the session gets it: the user's text behind
/// [`crate::inject::PROMPT_PREFIX`] on every harness. The prefix tells the
/// model that the person sent it (Claude Code otherwise frames text from
/// its inbox socket as a message from another session, not typed by the
/// user), and the text never reads as a wake-up notice.
pub(super) fn prompt_text(text: &str) -> String {
    format!("{}{text}", crate::inject::PROMPT_PREFIX)
}

/// How a prompt reaches a harness's session, for the worklog.
fn prompt_path(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "Claude Code inbox socket",
        Harness::Codex => "codex queue",
        Harness::Dsh => "dsh session/prompt",
    }
}

impl Node {
    /// `prompt`, `interrupt` and `attention_action` commands (see the
    /// module docs). Seats this node does not host are left alone.
    pub(super) fn on_control(self: &Arc<Self>, cmd: NodeCommand) {
        {
            let st = self.lock();
            let managed = st.saved.managed.contains_key(&cmd.seat);
            let hosted = managed || st.seats.get(&cmd.seat).is_some_and(|s| s.harness.is_some());
            if !hosted || (cmd.op == "attention_action" && !managed) {
                return;
            }
        }
        let n = self.clone();
        tokio::spawn(async move {
            match cmd.op.as_str() {
                "prompt" => {
                    let text = cmd.text.clone().unwrap_or_default();
                    if !text.trim().is_empty() {
                        n.deliver_prompt(&cmd.seat, &text).await;
                    }
                }
                "interrupt" => n.interrupt_seat(&cmd.seat).await,
                "attention_action" => {
                    let action = cmd.action.as_deref().unwrap_or_default();
                    n.attention_action(&cmd.seat, cmd.request, action).await;
                }
                _ => {}
            }
        });
    }

    /// The harness and wake target of the seat's running session.
    fn live_target(&self, seat: &str) -> Option<(Harness, String)> {
        let st = self.lock();
        let s = st.seats.get(seat)?;
        if matches!(s.state, SeatState::Offline | SeatState::Dormant) {
            return None;
        }
        Some((s.harness?, s.session.as_ref()?.target.clone()?))
    }

    /// The running session of `seat`; a supervised seat that is asleep or
    /// stopped is started first.
    async fn prompt_target(&self, seat: &str) -> Result<(Harness, String)> {
        if let Some(t) = self.live_target(seat) {
            return Ok(t);
        }
        if !self.lock().saved.managed.contains_key(seat) {
            bail!("the seat has no running session");
        }
        let r = self.sup_wake(seat).await;
        if let Some(e) = r.get("error").and_then(Value::as_str) {
            bail!("starting the seat: {e}");
        }
        let until = Instant::now() + PROMPT_START_WAIT;
        loop {
            if let Some(t) = self.live_target(seat) {
                return Ok(t);
            }
            if Instant::now() >= until {
                bail!(
                    "the seat's session did not start within {} s",
                    PROMPT_START_WAIT.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Give `text` to the seat's session; record the outcome.
    pub(super) async fn deliver_prompt(&self, seat: &str, text: &str) {
        let r = async {
            let (h, target) = self.prompt_target(seat).await?;
            self.adapter(h)
                .send_prompt(&target, &prompt_text(text))
                .await?;
            anyhow::Ok(h)
        }
        .await;
        let line = match &r {
            Ok(h) => {
                tracing::info!("seat {seat}: gave the user's prompt to the session");
                format!(
                    "the user's prompt was given to the session ({})",
                    prompt_path(*h)
                )
            }
            Err(e) => {
                tracing::warn!("seat {seat}: the user's prompt was not delivered: {e:#}");
                format!("the user's prompt was not delivered: {e:#}")
            }
        };
        self.lock().log(seat, "prompt", line);
    }

    /// Stop the seat's running turn; record the outcome.
    pub(super) async fn interrupt_seat(&self, seat: &str) {
        let Some((h, target)) = self.live_target(seat) else {
            self.lock().log(
                seat,
                "interrupt",
                "not interrupted: the seat has no running session",
            );
            return;
        };
        if h == Harness::Claude {
            // The server refuses the route for Claude Code seats.
            tracing::info!("seat {seat}: Claude Code has no structured interrupt; ignored");
            return;
        }
        let r = self.adapter(h).interrupt(&target).await;
        let mut st = self.lock();
        match r {
            Ok(true) => {
                st.log(
                    seat,
                    "interrupt",
                    format!("the running turn was interrupted ({})", h.as_str()),
                );
                // dsh fires no hook after a cancelled turn; the node ends
                // the turn as it ends one the person interrupted.
                if st
                    .seats
                    .get(seat)
                    .is_some_and(|s| s.state == SeatState::Busy)
                {
                    st.set_state(seat, SeatState::Idle);
                    self.turn_ended(&mut st, seat, turns::TurnEnd::Person);
                }
            }
            Ok(false) => st.log(seat, "interrupt", "not interrupted: no turn was running"),
            Err(e) => {
                tracing::warn!("seat {seat}: interrupt failed: {e:#}");
                st.log(seat, "interrupt", format!("the interrupt failed: {e:#}"));
            }
        }
        drop(st);
        self.presence_kick.notify_one();
        self.wake_kick.notify_one();
    }

    /// Every [`Config::waiting_recheck`]: look again at the requests of
    /// seats that are idle or asleep with an open request, so a mark goes
    /// when its request closes without a turn of the seat (a withdrawal
    /// whose mail did not wake it, an expiry), see the module docs.
    pub(super) async fn waiting_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(self.cfg.waiting_recheck).await;
            let due: Vec<(String, Option<i64>)> = {
                let st = self.lock();
                if !st.connected {
                    continue;
                }
                st.seats
                    .iter()
                    .filter(|(_, s)| {
                        (s.rq.open || s.rq.waiting_for.is_some())
                            && matches!(s.state, SeatState::Idle | SeatState::Dormant)
                    })
                    .map(|(name, s)| (name.clone(), s.ho.turn_start))
                    .collect()
            };
            for (seat, turn_start) in due {
                self.check_waiting(&seat, turn_start).await;
            }
        }
    }

    /// Carry out the user's answer to an `attention` request.
    pub(super) async fn attention_action(&self, seat: &str, request: Option<i64>, action: &str) {
        let r = match action {
            "restart" => self.sup_restart(seat).await,
            "sleep" => self.sup_sleep(seat).await,
            other => {
                tracing::debug!("ignoring attention action {other:?} for seat {seat}");
                return;
            }
        };
        let req = request.map(|n| format!("r{n}: ")).unwrap_or_default();
        let line = match r.get("error").and_then(Value::as_str) {
            Some(e) => format!("{req}{action} failed: {e}"),
            None if action == "sleep" => format!("{req}the seat was put to sleep"),
            None => format!(
                "{req}the seat's session is {}",
                r["action"].as_str().unwrap_or("restarting")
            ),
        };
        tracing::info!("seat {seat}: attention action {action}: {r}");
        self.lock().log(seat, "attention_action", line);
    }

    /// After a turn of a seat that asked the person (the turn started at
    /// `turn_start`), and periodically while it is marked: mark the seat as
    /// waiting when it made a request that is still open and nothing else
    /// waits for it; keep an existing mark on the newest open request;
    /// clear it when none is open (see the module docs).
    pub(super) async fn check_waiting(&self, seat: &str, turn_start: Option<i64>) {
        let list = match self.server.open_requests(seat).await {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("seat {seat}: open requests: {e:#}");
                return;
            }
        };
        let open = list
            .requests
            .iter()
            .filter(|r| {
                waited_on(r.kind)
                    && r.state == RequestState::Open
                    && r.seat == seat
                    && r.asker.as_deref() == Some(seat)
            })
            .map(|r| r.num)
            .max();
        {
            let mut st = self.lock();
            let Some(s) = st.seats.get_mut(seat) else {
                return;
            };
            s.rq.open = open.is_some();
            let line = match (s.rq.waiting_for, open) {
                (Some(old), None) => {
                    s.rq.waiting_for = None;
                    Some(format!("no longer waits: r{old} is closed"))
                }
                (Some(old), Some(num)) if old != num => {
                    s.rq.waiting_for = Some(num);
                    Some(format!("waits for the answer to r{num}"))
                }
                // Marked, and the request is still open: nothing changes.
                (Some(_), Some(_)) => return,
                (None, None) => return,
                (None, Some(_)) => None,
            };
            if let Some(line) = line {
                st.dirty_presence.insert(seat.to_string());
                st.log(seat, "waiting", line);
                drop(st);
                self.presence_kick.notify_one();
                return;
            }
        }
        let Some(num) = open else {
            return;
        };
        let until = Instant::now() + WAIT_SETTLE;
        loop {
            {
                let mut st = self.lock();
                let digest_every = st.digest_every;
                let Some(s) = st.seats.get_mut(seat) else {
                    return;
                };
                if s.ho.turn_start != turn_start {
                    // Another turn started meanwhile.
                    return;
                }
                match s.state {
                    SeatState::Idle | SeatState::Dormant => {
                        if s.wants_wake(now_ms(), digest_every) || s.rq.waiting_for == Some(num) {
                            return;
                        }
                        s.rq.waiting_for = Some(num);
                        st.dirty_presence.insert(seat.to_string());
                        st.log(seat, "waiting", format!("waits for the answer to r{num}"));
                        drop(st);
                        self.presence_kick.notify_one();
                        return;
                    }
                    SeatState::Busy => {}
                    _ => return,
                }
            }
            if Instant::now() >= until {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_marked_and_never_read_as_wake_ups() {
        assert_eq!(
            prompt_text("look at t3"),
            "[amail] Prompt from the user: look at t3"
        );
        let t = prompt_text("[amail] You have new mail (wake #3).");
        assert!(!crate::inject::is_wake_text(&t), "{t}");
        assert!(t.starts_with("[amail] Prompt from the user: "), "{t}");
    }

    #[test]
    fn a_new_turn_keeps_the_mark_and_its_end_looks_again() {
        let mut r = SeatRq {
            asked: true,
            open: true,
            waiting_for: Some(4),
        };
        r.turn_started();
        assert!(!r.asked);
        assert_eq!(r.waiting_for, Some(4));
        // A turn that did not ask still looks at its end: the seat has an
        // open request.
        assert!(r.look_at_turn_end());
        let mut idle = SeatRq::default();
        assert!(!idle.look_at_turn_end());
        idle.asked = true;
        assert!(idle.look_at_turn_end());
        assert!(!idle.asked);
    }
}
