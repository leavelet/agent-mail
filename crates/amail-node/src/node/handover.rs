//! Documents and seat handover on the node's side (M9).
//!
//! - **Document index.** At session start, and after a compaction, the
//!   node shows the seat its team's document index (`GET /docs/index`)
//!   under `Documents:`, after the mail and the task line. At the start of
//!   a turn (and in a `Stop` answer that blocks anyway) it shows the index
//!   again only when the index text changed since the seat last saw it
//!   (the hash of the last text is kept per seat, in memory). The index
//!   gets at most [`INDEX_SHARE`] of the harness's injection budget and
//!   what the mail left of it: mail always wins.
//! - **Worklog.** The node records what it sees in hooks as worklog
//!   entries (`POST /worklog`, batched every [`Config::worklog_every`],
//!   best effort): session start and end, compaction, turn start and end,
//!   tool calls, and commits in the seat's working directory.
//! - **Handover note.** At session start (`startup`, `resume`, `clear`,
//!   `compact`) the node shows the seat's note before the mail, within
//!   [`NOTE_SHARE`] of the budget, and records the read. At the end of a
//!   turn it asks for a new note, once per turn, by blocking the `Stop`
//!   with [`crate::inject::HANDOVER_ASK`] when the note is older than the
//!   turn and the context passed [`Config::handover_threshold`] of the
//!   model's window, or the seat closed a task and still holds work, or a
//!   request for the note is open. The server is asked only when one of
//!   these may hold: the transcript shows the context over the threshold,
//!   the turn ran an `amail task` command or task tool, or a request was
//!   seen (at session start, at the last check, or as mail); any other
//!   turn's end is answered from memory. A seat idle with work in progress (for
//!   the team's `handover_idle`, or half its `idle_sleep`, whichever is
//!   shorter) is asked by a request mail through the server, since no turn
//!   runs to block. `PostCompact` files the harness's summary as a draft
//!   note when the note is older than the current turn.

use super::*;
use amail_proto::{WorklogPost, WorklogPostEntry};

/// Share of the injection budget the document index may take at most
/// (numerator, denominator): about 3,000 characters on Claude Code and
/// dsh (9,000) and 2,000 on Codex (6,000).
pub(super) const INDEX_SHARE: (usize, usize) = (1, 3);
/// Share of the injection budget the handover note may take at most:
/// about 6,000 characters on Claude Code and dsh, 4,000 on Codex.
pub(super) const NOTE_SHARE: (usize, usize) = (2, 3);
/// Lines of the index asked for.
pub(super) const INDEX_MAX_LINES: u32 = 60;
/// Worklog entries kept while the server cannot be reached; older ones
/// are dropped first.
const MAX_WORKLOG_QUEUE: usize = 10_000;
/// Entries per `POST /worklog`.
const WORKLOG_BATCH: usize = 1_000;
/// Bytes of the end of a transcript read for the latest usage figure.
const USAGE_TAIL: u64 = 512 * 1024;
/// Worklog entries of kind `task` read at a turn's end.
const TASK_ENTRIES: u32 = 50;
/// Margin before the turn's start when asking for the turn's task entries
/// (the server's clock may differ a little).
const CLOCK_MARGIN_MS: i64 = 2_000;
/// At session start with nothing cached, how long a hook waits for the
/// index at most (and at most half of what is left of its deadline).
pub(super) const INDEX_WAIT: Duration = Duration::from_millis(500);
/// The same for the handover note.
pub(super) const NOTE_WAIT: Duration = Duration::from_millis(700);
/// How long a background fetch of the index or the note may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Wait for `job` until `cap` or half of what is left before `deadline`,
/// whichever comes first. The job goes on when the wait ends.
pub(super) async fn wait_part(job: tokio::task::JoinHandle<()>, deadline: Instant, cap: Duration) {
    let left = deadline.saturating_duration_since(Instant::now());
    let _ = tokio::time::timeout((left / 2).min(cap), job).await;
}

/// The default of the team's `handover_idle`.
pub const HANDOVER_IDLE_DEFAULT_MS: u64 = 30 * 60 * 1000;

pub(super) fn share(budget: usize, (n, d): (usize, usize)) -> usize {
    budget * n / d
}

/// Per-seat state of this module (in memory).
#[derive(Debug, Clone, Default)]
pub(super) struct SeatHo {
    /// Hash of the index text the seat was shown last.
    pub(super) index_hash: Option<u64>,
    /// When the index was last fetched for the seat.
    pub(super) index_checked: Option<Instant>,
    /// The index as last fetched (hooks show it from here).
    pub(super) index: Option<amail_proto::DocIndex>,
    /// A fetch of the index is running.
    pub(super) index_fetching: bool,
    /// The session has not been shown its note yet (session start with
    /// source `startup`, `resume`, `clear` or `compact`).
    pub(super) note_pending: bool,
    /// The note as fetched for the pending showing.
    pub(super) note_fetched: Option<amail_proto::HandoverResponse>,
    /// A fetch of the note is running.
    pub(super) note_fetching: bool,
    /// Counts session starts: a fetch made for an earlier one is dropped.
    pub(super) note_gen: u64,
    /// When the current (or last) turn started (Unix ms).
    pub(super) turn_start: Option<i64>,
    /// The current turn's `Stop` was blocked to ask for the note.
    pub(super) asked: bool,
    /// The current turn ran an `amail task` command or an amail task tool
    /// (it may have closed a task).
    pub(super) task_tool: bool,
    /// A request for the note may be open (seen at session start, at the
    /// last turn's end, or as mail since).
    pub(super) requested: bool,
    /// Turns started whose end was not logged yet (a Codex turn's end is
    /// settled after the next one may have started).
    pub(super) turns_open: u32,
    /// When the current note was written, as last fetched: `Some(None)`
    /// for no note, `None` for not known.
    pub(super) note_at: Option<Option<i64>>,
    /// When the seat's last turn started as the server's worklog records it
    /// (`Some(0)`: no turn recorded), fetched with the note's state when the
    /// node has not seen a turn of the seat (after a restart).
    pub(super) turn_seen: Option<i64>,
    /// A [`Job::NoteState`] for the seat is queued or running.
    pub(super) note_state_queued: bool,
    /// The idle period (the seat's `since`) the node asked about.
    pub(super) idle_asked: Option<i64>,
    /// The working directory's last commit, as last looked at.
    pub(super) head: Option<String>,
    pub(super) cwd: Option<String>,
    /// The seat's `doing` task, when it has exactly one.
    pub(super) doing: Option<i64>,
    /// The model the hooks named.
    pub(super) model: Option<String>,
}

impl SeatHo {
    /// `current`, `stale` or `none` for `amail ps` (`None`: not known).
    pub(super) fn note_word(&self) -> Option<&'static str> {
        match self.note_at {
            Some(None) => Some("none"),
            _ => self
                .note_current()
                .map(|c| if c { "current" } else { "stale" }),
        }
    }

    /// Whether the note is current: no turn started after it was written.
    /// `None` when the node does not know.
    pub(super) fn note_current(&self) -> Option<bool> {
        match (self.note_at?, self.turn_start.or(self.turn_seen)) {
            (None, _) => Some(false),
            (Some(n), Some(t)) => Some(n >= t),
            (Some(_), None) => None,
        }
    }
}

/// Work done off the hook path.
#[derive(Debug, Clone)]
pub(super) enum Job {
    /// Look at the last commit in `cwd`; with `log`, record it in the
    /// worklog when it changed.
    Head {
        seat: String,
        cwd: String,
        session: Option<String>,
        log: bool,
    },
    /// File a compaction summary as a draft note unless the note was
    /// written in the current turn (`turn_start`).
    Draft {
        seat: String,
        summary: String,
        session: Option<String>,
        turn_start: Option<i64>,
    },
    /// Record that `session` was shown revision `rev` of the note.
    Read {
        seat: String,
        session: String,
        rev: i64,
    },
    /// Fetch the note's state (when it was written) for `amail ps`: when
    /// the node connects, when a seat is added, and after the seat wrote
    /// its note.
    NoteState { seat: String },
}

/// The hash of an index text.
pub(super) fn text_hash(t: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// Whether a claimed message is the server's request for the note.
pub(super) fn is_request(e: &amail_proto::InboxEntry) -> bool {
    e.message.source == amail_proto::Source::System
        && e.message.subject.starts_with("Handover note requested by")
}

/// Whether a worklog entry of kind `task` records a close.
fn closes_task(text: &str) -> bool {
    text.contains("→ done") || text.contains("→ cancelled")
}

impl State {
    /// Queue a [`Job::NoteState`] for `seat` unless one is queued; with
    /// `known`, also when the node already knows the note's state.
    pub(super) fn want_note_state(&mut self, seat: &str, known: bool) -> bool {
        let ho = &mut self.seat(seat).ho;
        if ho.note_state_queued || (ho.note_at.is_some() && !known) {
            return false;
        }
        ho.note_state_queued = true;
        self.jobs.push_back(Job::NoteState {
            seat: seat.to_string(),
        });
        true
    }

    /// Queue a worklog entry for `seat`, marked with the seat's current
    /// session and its `doing` task when the node knows them.
    pub(super) fn log(&mut self, seat: &str, kind: &str, text: impl Into<String>) {
        let session = self
            .seats
            .get(seat)
            .and_then(|s| s.session.as_ref())
            .and_then(|s| s.id.clone());
        self.log_session(seat, session, kind, text);
    }

    /// [`State::log`] for a given session.
    pub(super) fn log_session(
        &mut self,
        seat: &str,
        session: Option<String>,
        kind: &str,
        text: impl Into<String>,
    ) {
        let task = self.seats.get(seat).and_then(|s| s.ho.doing);
        let text = crate::adapter::cut_chars(&text.into(), amail_proto::MAX_WORKLOG_TEXT_CHARS);
        self.worklog.push_back(WorklogPostEntry {
            seat: seat.to_string(),
            session,
            at: Some(ts(now_ms())),
            kind: kind.to_string(),
            text,
            task,
            doc: None,
        });
        while self.worklog.len() > MAX_WORKLOG_QUEUE {
            self.worklog.pop_front();
            self.worklog_dropped += 1;
        }
    }
}

/// What the end of a turn found about the note.
#[derive(Debug, Default)]
pub(super) struct StopCheck {
    /// Why the note is asked for, if it is.
    pub(super) ask: Option<&'static str>,
    /// The note's time, when the server answered.
    pub(super) note_at: Option<Option<i64>>,
    /// A request for the note is open.
    pub(super) requested: bool,
}

impl Node {
    /// Fetch `seat`'s document index into its cache, in a task of its own.
    /// `None` (and nothing fetched) when the node is shutting down.
    pub(super) fn spawn_index(
        &self,
        seat: &str,
        max: usize,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let Some(me) = self.me.upgrade() else {
            self.lock().seat(seat).ho.index_fetching = false;
            return None;
        };
        let seat = seat.to_string();
        Some(tokio::spawn(async move {
            let r = me
                .server
                .docs_index(&seat, INDEX_MAX_LINES, max, FETCH_TIMEOUT)
                .await;
            let mut st = me.lock();
            let ho = &mut st.seat(&seat).ho;
            ho.index_fetching = false;
            ho.index_checked = Some(Instant::now());
            match r {
                Ok(ix) => ho.index = Some(ix),
                Err(e) => tracing::debug!("document index of {seat}: {e:#}"),
            }
        }))
    }

    /// Fetch `seat`'s handover note for the pending showing, in a task of
    /// its own.
    pub(super) fn spawn_note(&self, seat: &str) -> Option<tokio::task::JoinHandle<()>> {
        let started = self.lock().seat(seat).ho.note_gen;
        let Some(me) = self.me.upgrade() else {
            self.lock().seat(seat).ho.note_fetching = false;
            return None;
        };
        let seat = seat.to_string();
        Some(tokio::spawn(async move {
            let r = me.server.handover(&seat, FETCH_TIMEOUT).await;
            let mut st = me.lock();
            let ho = &mut st.seat(&seat).ho;
            if ho.note_gen != started {
                return;
            }
            ho.note_fetching = false;
            match r {
                Ok(r) => {
                    ho.note_at = Some(r.note.as_ref().and_then(|n| parse_ts(&n.written_at)));
                    ho.requested = r.requested_at.is_some();
                    ho.note_fetched = Some(r);
                }
                Err(e) => tracing::debug!("handover note of {seat}: {e:#}"),
            }
        }))
    }

    /// Whether the session's context passed the handover threshold, from
    /// the latest usage figure in its transcript (false when unknown).
    pub(super) fn context_over(&self, h: Harness, seat: &str) -> bool {
        let (path, model) = {
            let st = self.lock();
            let path = st
                .saved
                .transcripts
                .get(seat)
                .filter(|t| t.harness == h.as_str())
                .map(|t| PathBuf::from(&t.path));
            let model = st.seats.get(seat).and_then(|s| s.ho.model.clone());
            (path, model)
        };
        let Some(path) = path else {
            return false;
        };
        let Ok((tail, _)) = crate::transcript::read_tail(&path, USAGE_TAIL) else {
            return false;
        };
        let Some(u) = crate::transcript::usage::last_usage(h, &tail) else {
            tracing::debug!("seat {seat}: no usage figure in its transcript");
            return false;
        };
        let model = u.model.clone().or(model);
        let (window, from) = self
            .cfg
            .context_windows
            .window(model.as_deref(), u.context_window);
        let pct = self.cfg.handover_threshold;
        let over = u.input_tokens as f64 > window as f64 * pct / 100.0;
        tracing::debug!(
            "seat {seat}: context {} of {window} tokens (window of {} from {from}); \
             handover threshold {pct}%{}",
            u.input_tokens,
            model.as_deref().unwrap_or("an unknown model"),
            if over { ": over" } else { "" }
        );
        over
    }

    /// At a turn's end: should the seat write its note now? Nothing is
    /// asked when the server does not answer in time.
    pub(super) async fn stop_check(
        &self,
        seat: &str,
        turn_start: Option<i64>,
        over: bool,
        deadline: Instant,
    ) -> StopCheck {
        let left = || deadline.saturating_duration_since(Instant::now());
        let note = self.server.handover(seat, left());
        let tasks = async {
            let t = turn_start?;
            let since = ts(t - CLOCK_MARGIN_MS);
            self.server
                .worklog(seat, "task", &since, TASK_ENTRIES, left())
                .await
                .ok()
        };
        let both = async { tokio::join!(note, tasks) };
        let Ok((note, tasks)) = tokio::time::timeout_at(deadline.into(), both).await else {
            return StopCheck::default();
        };
        let h = match note {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!("handover note of {seat}: {e:#}");
                return StopCheck::default();
            }
        };
        let note_at = h
            .note
            .as_ref()
            .map(|n| parse_ts(&n.written_at).unwrap_or(0));
        let mut out = StopCheck {
            ask: None,
            note_at: Some(note_at),
            requested: h.requested_at.is_some(),
        };
        let older = match (note_at, turn_start) {
            (None, _) => true,
            (Some(n), Some(t)) => n < t,
            (Some(_), None) => false,
        };
        if !older {
            return out;
        }
        if h.requested_at.is_some() {
            out.ask = Some("a note was requested");
        } else if over {
            out.ask = Some("the context passed the threshold");
        } else if tasks
            .as_ref()
            .is_some_and(|p| p.entries.iter().any(|e| closes_task(&e.text)))
        {
            // The server writes the note itself when the seat holds no
            // other work; ask only when it still holds some.
            let call = self.server.tasks_mine(seat, 1, left());
            if let Ok(Ok(r)) = tokio::time::timeout_at(deadline.into(), call).await
                && !r.tasks.is_empty()
            {
                out.ask = Some("a task was closed and other work remains");
            }
        }
        if let Some(why) = out.ask {
            tracing::info!("seat {seat}: asking for the handover note ({why})");
        }
        out
    }

    /// Send the queued worklog entries, every [`Config::worklog_every`].
    pub(super) async fn worklog_loop(self: Arc<Self>) {
        let node = self.cfg.label.clone();
        loop {
            tokio::time::sleep(self.cfg.worklog_every).await;
            loop {
                let batch: Vec<WorklogPostEntry> = {
                    let mut st = self.lock();
                    if !st.connected || st.worklog.is_empty() {
                        break;
                    }
                    if st.worklog_dropped > 0 {
                        tracing::warn!(
                            "{} worklog entries were dropped while the server could not be \
                             reached",
                            st.worklog_dropped
                        );
                        st.worklog_dropped = 0;
                    }
                    let n = st.worklog.len().min(WORKLOG_BATCH);
                    st.worklog.drain(..n).collect()
                };
                let post = WorklogPost {
                    node: node.clone(),
                    entries: batch,
                };
                match self.server.post_worklog(&post).await {
                    Ok(r) => {
                        if r.skipped > 0 {
                            tracing::debug!("worklog: {} entries skipped by the server", r.skipped);
                        }
                    }
                    Err(e) if crate::server::is_permanent(&e) => {
                        tracing::warn!("worklog entries refused: {e:#}");
                    }
                    Err(e) => {
                        tracing::debug!("worklog not sent: {e:#}");
                        let mut st = self.lock();
                        let mut back: VecDeque<WorklogPostEntry> = post.entries.into();
                        back.append(&mut st.worklog);
                        st.worklog = back;
                        while st.worklog.len() > MAX_WORKLOG_QUEUE {
                            st.worklog.pop_front();
                            st.worklog_dropped += 1;
                        }
                        break;
                    }
                }
            }
        }
    }

    /// Run queued [`Job`]s.
    pub(super) async fn jobs_loop(self: Arc<Self>) {
        loop {
            let job = self.lock().jobs.pop_front();
            let Some(job) = job else {
                self.jobs_kick.notified().await;
                continue;
            };
            match job {
                Job::Head {
                    seat,
                    cwd,
                    session,
                    log,
                } => {
                    let Some(head) = last_commit(&cwd).await else {
                        continue;
                    };
                    let mut st = self.lock();
                    let s = st.seat(&seat);
                    let changed = s.ho.head.as_deref() != Some(head.0.as_str());
                    let known = s.ho.head.is_some();
                    s.ho.head = Some(head.0.clone());
                    if log && changed && known {
                        let short: String = head.0.chars().take(12).collect();
                        st.log_session(&seat, session, "commit", format!("{short} {}", head.1));
                    }
                }
                Job::Draft {
                    seat,
                    summary,
                    session,
                    turn_start,
                } => {
                    let h = match self.server.handover(&seat, Duration::from_secs(10)).await {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::debug!("handover note of {seat}: {e:#}");
                            continue;
                        }
                    };
                    let note_at = h.note.as_ref().and_then(|n| parse_ts(&n.written_at));
                    let current = matches!((note_at, turn_start), (Some(n), Some(t)) if n >= t);
                    if current {
                        continue;
                    }
                    match self
                        .server
                        .handover_compaction(&seat, &summary, session.as_deref())
                        .await
                    {
                        Ok(r) => {
                            let at = r.note.as_ref().and_then(|n| parse_ts(&n.written_at));
                            self.lock().seat(&seat).ho.note_at = Some(at);
                            tracing::info!(
                                "seat {seat}: compaction summary filed as draft note rev {}",
                                r.current_rev
                            );
                        }
                        Err(e) => tracing::warn!("seat {seat}: draft note not filed: {e:#}"),
                    }
                }
                Job::Read { seat, session, rev } => {
                    if let Err(e) = self.server.handover_read(&seat, &session, rev).await {
                        tracing::debug!("seat {seat}: note read not recorded: {e:#}");
                    }
                }
                Job::NoteState { seat } => self.note_state(&seat).await,
            }
        }
    }

    /// Fetch when `seat`'s note was written and, when the node has seen no
    /// turn of the seat, when its last turn started (from the worklog).
    async fn note_state(&self, seat: &str) {
        let wait = Duration::from_secs(10);
        let note = self.server.handover(seat, wait).await;
        let need_turn = {
            let mut st = self.lock();
            let ho = &mut st.seat(seat).ho;
            ho.note_state_queued = false;
            match &note {
                Ok(h) => {
                    ho.note_at = Some(
                        h.note
                            .as_ref()
                            .map(|n| parse_ts(&n.written_at).unwrap_or(0)),
                    );
                    ho.requested = h.requested_at.is_some();
                }
                Err(e) => {
                    tracing::debug!("handover note of {seat}: {e:#}");
                    return;
                }
            }
            ho.turn_start.is_none() && ho.turn_seen.is_none()
        };
        if !need_turn {
            return;
        }
        match self
            .server
            .worklog(seat, "turn_start", "1970-01-01T00:00:00Z", 1, wait)
            .await
        {
            Ok(p) => {
                let at = p.entries.first().and_then(|e| parse_ts(&e.at)).unwrap_or(0);
                self.lock().seat(seat).ho.turn_seen = Some(at);
            }
            Err(e) => tracing::debug!("worklog of {seat}: {e:#}"),
        }
    }

    /// Ask seats that have been idle with work in progress for their note
    /// (a request mail through the server: no turn runs that a `Stop`
    /// block could ask in).
    pub(super) async fn handover_idle_loop(self: Arc<Self>) {
        loop {
            tokio::time::sleep(self.cfg.handover_scan).await;
            let due: Vec<(String, i64, Option<i64>)> = {
                let st = self.lock();
                if !st.connected {
                    continue;
                }
                let idle = st
                    .saved
                    .settings
                    .handover_idle_ms
                    .unwrap_or(HANDOVER_IDLE_DEFAULT_MS);
                if idle == 0 {
                    continue;
                }
                let now = now_ms();
                st.seats
                    .iter()
                    .filter(|(_, s)| s.state == SeatState::Idle && s.session.is_some())
                    .filter_map(|(name, s)| {
                        let mut after = idle;
                        if st.saved.managed.contains_key(name)
                            && let Some(sleep) = st.saved.settings.idle_sleep_ms
                        {
                            after = after.min(sleep / 2);
                        }
                        let waited = now - s.since >= after as i64;
                        let asked = s.ho.idle_asked == Some(s.since);
                        let current = s.ho.note_current() == Some(true);
                        (waited && !asked && !current && s.ho.turn_start.is_some())
                            .then(|| (name.clone(), s.since, s.ho.turn_start))
                    })
                    .collect()
            };
            for (seat, since, turn_start) in due {
                self.lock().seat(&seat).ho.idle_asked = Some(since);
                self.idle_request(&seat, turn_start).await;
            }
        }
    }

    async fn idle_request(&self, seat: &str, turn_start: Option<i64>) {
        let wait = Duration::from_secs(10);
        let h = match self.server.handover(seat, wait).await {
            Ok(h) => h,
            Err(e) => {
                tracing::debug!("handover note of {seat}: {e:#}");
                return;
            }
        };
        let note_at = h
            .note
            .as_ref()
            .map(|n| parse_ts(&n.written_at).unwrap_or(0));
        self.lock().seat(seat).ho.note_at = Some(note_at);
        let older = match (note_at, turn_start) {
            (None, _) => true,
            (Some(n), Some(t)) => n < t,
            (Some(_), None) => false,
        };
        if !older || h.requested_at.is_some() {
            return;
        }
        match self.server.tasks_mine(seat, 1, wait).await {
            Ok(r) if !r.tasks.is_empty() => {}
            _ => return,
        }
        match self.server.handover_request(seat).await {
            Ok(_) => tracing::info!(
                "seat {seat}: idle with work in progress and an old note; asked for its \
                 handover note"
            ),
            Err(e) => tracing::warn!("seat {seat}: handover request not sent: {e:#}"),
        }
    }
}

/// `(hash, subject)` of the last commit in `cwd`, if it is a git working
/// tree. Runs `git log -1` with a timeout; any failure is unknown.
async fn last_commit(cwd: &str) -> Option<(String, String)> {
    let dir = std::path::Path::new(cwd);
    if !dir.is_absolute() || !dir.is_dir() {
        return None;
    }
    let mut c = tokio::process::Command::new("git");
    c.args([
        "-c",
        "core.fsmonitor=false",
        "log",
        "-1",
        "--no-color",
        "--format=%H%x09%s",
    ])
    .current_dir(dir)
    .env("GIT_OPTIONAL_LOCKS", "0")
    .env("GIT_TERMINAL_PROMPT", "0")
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null())
    .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(5), c.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (hash, subject) = text.trim_end().split_once('\t')?;
    (hash.len() >= 7 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then(|| {
        (
            hash.to_string(),
            amail_proto::text::line(subject).into_owned(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closes() {
        assert!(closes_task("t12: doing → done"));
        assert!(closes_task("t3: open → cancelled"));
        assert!(!closes_task("t12: open → doing"));
        assert!(!closes_task("t12: changed title"));
    }

    #[test]
    fn note_current() {
        let mut s = SeatHo::default();
        assert_eq!(s.note_current(), None);
        s.note_at = Some(None);
        assert_eq!(s.note_current(), Some(false));
        s.note_at = Some(Some(10));
        assert_eq!(s.note_current(), None);
        s.turn_start = Some(5);
        assert_eq!(s.note_current(), Some(true));
        s.turn_start = Some(11);
        assert_eq!(s.note_current(), Some(false));
    }
}
