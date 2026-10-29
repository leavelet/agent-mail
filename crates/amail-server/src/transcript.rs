//! The conversation mirror: browsers watch a seat's transcript
//! (`GET /v1/seats/{name}/transcript`, SSE); the node hosting the seat reads
//! the harness's session file and posts what it finds
//! (`POST /v1/seats/{name}/transcript`).
//!
//! Nothing is stored. While a seat has watchers the server keeps its most
//! recent events in memory (at most [`MIRROR_REPLAY_EVENTS`] and
//! [`MIRROR_REPLAY_BYTES`]) so that a second browser starts with context,
//! and renews the node's lease (a `transcript` command) every third of the
//! lease. When the last watcher leaves the buffer is dropped and renewals
//! stop; the node stops when its lease runs out.
//!
//! Memory: a post is at most [`MAX_TRANSCRIPT_POST_BYTES`]. Each watcher
//! has its own queue of messages not yet sent (the messages themselves are
//! shared between the watchers of a seat). A watcher whose queue would grow
//! beyond [`MIRROR_WATCHER_BYTES`] is disconnected and its queue freed; a
//! seat's replay events and queues together stay under
//! [`MIRROR_SEAT_BYTES`]; all seats together stay under
//! `config::Limits::mirror_bytes` (beyond it, the watcher furthest behind
//! is disconnected or the largest replay buffer shortened, whichever holds
//! more, until the total fits).
//!
//! With several instances, the browser and the node may talk to different
//! ones: the watching instance records the watch in the database (renewed
//! with the lease), and an instance that receives a node's post forwards it
//! to the watching instances over HTTP (see [`crate::cluster`]).

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path};
use amail_proto::limits::{
    MAX_NODE_LABEL_CHARS, MAX_TRANSCRIPT_POST_BYTES, MAX_TRANSCRIPT_POST_EVENTS,
    MAX_TRANSCRIPT_STATUS_CHARS, MAX_TRANSCRIPT_TEXT_CHARS, MIRROR_REPLAY_BYTES,
    MIRROR_REPLAY_EVENTS, MIRROR_SEAT_BYTES, MIRROR_WATCHER_BYTES,
};
use amail_proto::{
    NodeCommand, SeatKind, TokenScope, TranscriptEvent, TranscriptPost, TranscriptPostResponse,
    TranscriptState, TranscriptStatus,
};
use axum::extract::State;
use axum::extract::rejection::BytesRejection;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt, stream};
use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// Bytes of replay events sent to a new watcher in one `events` message.
const SNAPSHOT_CHUNK: usize = 256 * 1024;
/// Bytes counted for an event or message on top of its text.
const OVERHEAD: usize = 64;

/// Lease and backfill settings.
#[derive(Debug, Clone)]
pub struct MirrorConfig {
    pub lease: Duration,
    pub backfill: u32,
    /// How long to wait for any node to answer before saying so.
    pub answer_timeout: Duration,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        MirrorConfig {
            lease: Duration::from_secs(30),
            backfill: 200,
            answer_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
enum Msg {
    Status(TranscriptStatus),
    Reset,
    /// Shared by the queues of all watchers of the seat.
    Events(Arc<Vec<TranscriptEvent>>),
}

/// Bytes counted for an event: its text fields plus a fixed overhead.
fn event_bytes(e: &TranscriptEvent) -> usize {
    OVERHEAD
        + e.text.len()
        + e.ts.as_ref().map_or(0, String::len)
        + e.tool.as_ref().map_or(0, String::len)
}

fn msg_bytes(m: &Msg) -> usize {
    match m {
        Msg::Status(s) => {
            OVERHEAD
                + [&s.message, &s.harness, &s.path, &s.session]
                    .iter()
                    .map(|f| f.as_ref().map_or(0, String::len))
                    .sum::<usize>()
        }
        Msg::Reset => OVERHEAD,
        Msg::Events(events) => OVERHEAD + events.iter().map(event_bytes).sum::<usize>(),
    }
}

/// Messages waiting to be sent to one watcher.
struct Queue {
    msgs: VecDeque<(Msg, usize)>,
    bytes: usize,
    wake: Arc<Notify>,
}

struct Chan {
    /// Watchers on this instance, by id.
    watchers: HashMap<u64, Queue>,
    /// Replay events for watchers that join later.
    buffer: VecDeque<TranscriptEvent>,
    /// Bytes counted for `buffer`.
    bytes: usize,
    /// Bytes waiting in the queues of all watchers.
    queued: usize,
    status: TranscriptStatus,
    /// The node that answers for the seat (the first to post).
    node: Option<String>,
    answered: bool,
    renewer: Option<tokio::task::JoinHandle<()>>,
}

impl Chan {
    /// Queue a message for every watcher; a watcher whose queue would grow
    /// beyond [`MIRROR_WATCHER_BYTES`] is dropped.
    fn push(&mut self, m: Msg) {
        let size = msg_bytes(&m);
        let full: Vec<u64> = self
            .watchers
            .iter()
            .filter(|(_, q)| q.bytes + size > MIRROR_WATCHER_BYTES)
            .map(|(id, _)| *id)
            .collect();
        for id in full {
            self.drop_watcher(id);
        }
        for q in self.watchers.values_mut() {
            q.msgs.push_back((m.clone(), size));
            q.bytes += size;
            self.queued += size;
            q.wake.notify_one();
        }
        self.fit_seat(0);
    }

    /// Keep the seat under [`MIRROR_SEAT_BYTES`] with `need` more bytes to
    /// come by dropping the watchers furthest behind. Returns whether it
    /// fits.
    fn fit_seat(&mut self, need: usize) -> bool {
        while self.total() + need > MIRROR_SEAT_BYTES {
            match self.slowest() {
                Some((id, _)) => self.drop_watcher(id),
                None => return false,
            }
        }
        true
    }

    /// The watcher with the most bytes waiting (none if nothing waits).
    fn slowest(&self) -> Option<(u64, usize)> {
        self.watchers
            .iter()
            .filter(|(_, q)| q.bytes > 0)
            .max_by_key(|(_, q)| q.bytes)
            .map(|(id, q)| (*id, q.bytes))
    }

    /// Disconnect a watcher: its queue is freed and its stream ends.
    fn drop_watcher(&mut self, id: u64) {
        if let Some(q) = self.watchers.remove(&id) {
            self.queued -= q.bytes;
            q.wake.notify_one();
        }
    }

    /// Drop replay events from the front until `bytes` fits `max`
    /// (keeping at least `keep` events). Returns the bytes freed.
    fn trim(&mut self, max_events: usize, max: usize, keep: usize) -> usize {
        let before = self.bytes;
        while self.buffer.len() > keep
            && (self.buffer.len() > max_events || self.bytes > max)
            && let Some(old) = self.buffer.pop_front()
        {
            self.bytes -= event_bytes(&old);
        }
        before - self.bytes
    }

    fn total(&self) -> usize {
        self.bytes + self.queued
    }
}

#[derive(Default)]
struct Inner {
    seats: HashMap<(i64, String), Chan>,
    /// Bytes counted over all seats (replay events and queues).
    used: usize,
}

impl Inner {
    /// Apply the change of a seat's bytes to the total.
    fn account(&mut self, delta: isize) {
        self.used = self.used.saturating_add_signed(delta);
    }

    /// Remove a seat that has no watchers left.
    fn remove_if_unwatched(&mut self, key: &(i64, String)) {
        if self.seats.get(key).is_some_and(|c| c.watchers.is_empty())
            && let Some(c) = self.seats.remove(key)
        {
            self.used = self.used.saturating_sub(c.total());
            if let Some(t) = c.renewer {
                t.abort();
            }
        }
    }

    /// Bring the total under `limit` with `need` more bytes to come, taking
    /// from the largest holder first: disconnect the watcher furthest behind
    /// (on any seat) or shorten the largest replay buffer, whichever holds
    /// more. Returns whether it fits.
    fn make_room(&mut self, limit: Option<usize>, need: usize) -> bool {
        let Some(limit) = limit else {
            return true;
        };
        while self.used + need > limit {
            let slowest = self
                .seats
                .iter()
                .filter_map(|(k, c)| c.slowest().map(|(id, b)| (k.clone(), id, b)))
                .max_by_key(|(_, _, b)| *b);
            let largest = self
                .seats
                .iter()
                .filter(|(_, c)| !c.buffer.is_empty())
                .max_by_key(|(_, c)| c.bytes)
                .map(|(k, c)| (k.clone(), c.bytes));
            match (slowest, largest) {
                (Some((key, id, q)), l) if l.as_ref().is_none_or(|(_, b)| q >= *b) => {
                    if let Some(c) = self.seats.get_mut(&key) {
                        let before = c.total();
                        c.drop_watcher(id);
                        let freed = before - c.total();
                        self.used = self.used.saturating_sub(freed);
                    }
                    self.remove_if_unwatched(&key);
                }
                (_, Some((key, bytes))) => {
                    let over = self.used + need - limit;
                    if let Some(c) = self.seats.get_mut(&key) {
                        let freed = c.trim(usize::MAX, bytes.saturating_sub(over), 0);
                        self.used = self.used.saturating_sub(freed);
                    }
                }
                _ => return false,
            }
        }
        true
    }
}

/// The conversation mirrors watched on this instance, and the memory they
/// hold: per watcher at most [`MIRROR_WATCHER_BYTES`] waiting, per seat at
/// most [`MIRROR_SEAT_BYTES`], over all seats at most the configured limit.
#[derive(Default)]
pub struct MirrorHub {
    inner: Mutex<Inner>,
    /// Bytes over all seats (`config::Limits::mirror_bytes`).
    limit: Option<usize>,
    next_id: AtomicU64,
}

/// One watcher; dropping it may end the channel.
struct Watch {
    hub: Arc<MirrorHub>,
    key: (i64, String),
    id: u64,
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut inner = self.hub.inner.lock().unwrap();
        if let Some(c) = inner.seats.get_mut(&self.key) {
            let before = c.total();
            c.drop_watcher(self.id);
            let freed = before - c.total();
            inner.used = inner.used.saturating_sub(freed);
        }
        inner.remove_if_unwatched(&self.key);
    }
}

fn status(state: TranscriptState, message: &str) -> TranscriptStatus {
    TranscriptStatus {
        message: Some(message.to_string()),
        ..TranscriptStatus::new(state)
    }
}

/// Cut a status's text fields to [`MAX_TRANSCRIPT_STATUS_CHARS`].
fn cut_status(s: &mut TranscriptStatus) {
    for f in [&mut s.message, &mut s.harness, &mut s.path, &mut s.session]
        .into_iter()
        .flatten()
    {
        if f.chars().count() > MAX_TRANSCRIPT_STATUS_CHARS {
            *f = f.chars().take(MAX_TRANSCRIPT_STATUS_CHARS).collect();
        }
    }
}

impl MirrorHub {
    /// A hub without a limit over all seats.
    pub fn new() -> Self {
        Self::default()
    }

    /// A hub that holds at most `limit` bytes over all seats.
    pub fn with_limit(limit: Option<usize>) -> Self {
        MirrorHub {
            limit,
            ..Self::default()
        }
    }

    /// Bytes counted over all seats (replay events and queued messages).
    pub fn bytes(&self) -> usize {
        self.inner.lock().unwrap().used
    }

    /// End every mirror of the org on this instance.
    pub fn close_org(&self, org_id: i64) {
        let mut inner = self.inner.lock().unwrap();
        let keys: Vec<(i64, String)> = inner
            .seats
            .keys()
            .filter(|(o, _)| *o == org_id)
            .cloned()
            .collect();
        for k in keys {
            if let Some(mut c) = inner.seats.remove(&k) {
                inner.used = inner.used.saturating_sub(c.total());
                let ids: Vec<u64> = c.watchers.keys().copied().collect();
                for id in ids {
                    c.drop_watcher(id);
                }
                if let Some(t) = c.renewer {
                    t.abort();
                }
            }
        }
    }

    /// Seats being watched in the org (for tests and status).
    pub fn watched(&self, org_id: i64) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut v: Vec<String> = inner
            .seats
            .keys()
            .filter(|(o, _)| *o == org_id)
            .map(|(_, s)| s.clone())
            .collect();
        v.sort();
        v
    }

    fn set_status(&self, key: &(i64, String), s: TranscriptStatus) {
        let mut inner = self.inner.lock().unwrap();
        let Some(c) = inner.seats.get_mut(key) else {
            return;
        };
        let before = c.total();
        c.status = s.clone();
        c.push(Msg::Status(s));
        let delta = c.total() as isize - before as isize;
        inner.account(delta);
        inner.remove_if_unwatched(key);
        inner.make_room(self.limit, 0);
    }

    /// A node's post. Returns the number of watchers (0: nobody watches,
    /// the data is dropped).
    fn post(&self, key: &(i64, String), p: TranscriptPost) -> u32 {
        let mut inner = self.inner.lock().unwrap();
        let Some(c) = inner.seats.get_mut(key) else {
            return 0;
        };
        match (&c.node, &p.node) {
            (Some(a), Some(b)) if a != b => return c.watchers.len() as u32,
            (None, Some(b)) => c.node = Some(b.clone()),
            _ => {}
        }
        c.answered = true;
        let before = c.total();
        if p.reset {
            c.buffer.clear();
            c.bytes = 0;
            c.push(Msg::Reset);
        }
        if let Some(mut s) = p.status {
            cut_status(&mut s);
            if s != c.status {
                c.status = s.clone();
                c.push(Msg::Status(s));
            }
        }
        if !p.events.is_empty() {
            let mut events = p.events;
            if events.len() > MAX_TRANSCRIPT_POST_EVENTS {
                events.drain(..events.len() - MAX_TRANSCRIPT_POST_EVENTS);
            }
            for e in &mut events {
                if e.text.chars().count() > MAX_TRANSCRIPT_TEXT_CHARS {
                    e.text = e.text.chars().take(MAX_TRANSCRIPT_TEXT_CHARS).collect();
                    e.truncated = true;
                }
            }
            for e in &events {
                c.bytes += event_bytes(e);
                c.buffer.push_back(e.clone());
            }
            c.trim(MIRROR_REPLAY_EVENTS, MIRROR_REPLAY_BYTES, 1);
            c.push(Msg::Events(Arc::new(events)));
        }
        let watchers = c.watchers.len() as u32;
        let delta = c.total() as isize - before as isize;
        inner.account(delta);
        inner.remove_if_unwatched(key);
        inner.make_room(self.limit, 0);
        watchers
    }

    /// Add a watcher to the seat: its queue starts with the status and the
    /// replay events. `None` when the instance has no room for it.
    fn subscribe(
        self: &Arc<Self>,
        key: &(i64, String),
        new_chan: impl FnOnce() -> Chan,
    ) -> Option<(Watch, Arc<Notify>, bool)> {
        let mut inner = self.inner.lock().unwrap();
        let mut msgs = VecDeque::new();
        let initial: Vec<Msg> = match inner.seats.get(key) {
            Some(c) => {
                let mut v = vec![Msg::Status(c.status.clone())];
                let mut chunk = Vec::new();
                let mut size = 0;
                for e in &c.buffer {
                    size += event_bytes(e);
                    chunk.push(e.clone());
                    if size >= SNAPSHOT_CHUNK {
                        v.push(Msg::Events(Arc::new(std::mem::take(&mut chunk))));
                        size = 0;
                    }
                }
                if !chunk.is_empty() {
                    v.push(Msg::Events(Arc::new(chunk)));
                }
                v
            }
            None => vec![],
        };
        let mut bytes = 0;
        for m in initial {
            let size = msg_bytes(&m);
            bytes += size;
            msgs.push_back((m, size));
        }
        if !inner.make_room(self.limit, bytes) {
            return None;
        }
        let fresh = !inner.seats.contains_key(key);
        let c = inner.seats.entry(key.clone()).or_insert_with(new_chan);
        let before = c.total();
        let fits = c.fit_seat(bytes);
        let freed = before - c.total();
        inner.used = inner.used.saturating_sub(freed);
        if !fits {
            inner.remove_if_unwatched(key);
            return None;
        }
        let c = inner.seats.get_mut(key)?;
        if fresh {
            let m = Msg::Status(c.status.clone());
            let size = msg_bytes(&m);
            bytes += size;
            msgs.push_back((m, size));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let wake = Arc::new(Notify::new());
        c.watchers.insert(
            id,
            Queue {
                msgs,
                bytes,
                wake: wake.clone(),
            },
        );
        c.queued += bytes;
        let needs_renewer = c.renewer.is_none();
        inner.used += bytes;
        let guard = Watch {
            hub: self.clone(),
            key: key.clone(),
            id,
        };
        Some((guard, wake, needs_renewer))
    }

    /// The next message for watcher `id`; `None` once it was disconnected.
    async fn next(&self, key: &(i64, String), id: u64, wake: &Notify) -> Option<Msg> {
        loop {
            {
                let mut inner = self.inner.lock().unwrap();
                let Inner { seats, used } = &mut *inner;
                let c = seats.get_mut(key)?;
                let q = c.watchers.get_mut(&id)?;
                if let Some((m, size)) = q.msgs.pop_front() {
                    q.bytes -= size;
                    c.queued -= size;
                    *used = used.saturating_sub(size);
                    return Some(m);
                }
            }
            wake.notified().await;
        }
    }

    fn set_renewer(&self, key: &(i64, String), t: tokio::task::JoinHandle<()>) {
        let mut inner = self.inner.lock().unwrap();
        match inner.seats.get_mut(key) {
            Some(c) if c.renewer.is_none() => c.renewer = Some(t),
            _ => t.abort(),
        }
    }
}

/// Keep the hosting node forwarding while the seat is watched.
async fn renew(st: AppState, key: (i64, String)) {
    let cfg = st.mirror_config.clone();
    // The first command that reaches a node asks for a backfill (a node
    // still forwarding from an earlier watch sends it again); renewals don't.
    let mut need_backfill = true;
    loop {
        if let Some(c) = &st.cluster {
            let expires = amail_store::now_ms() + cfg.lease.as_millis() as i64;
            if let Err(e) = st
                .store
                .watch_mirror(key.0, &key.1, &c.id, Some(expires))
                .await
            {
                tracing::warn!("recording a mirror watch: {e}");
            }
        }
        let node = match st.presence.get(key.0, &key.1).await {
            Ok(p) => p.and_then(|p| p.node),
            Err(e) => {
                tracing::debug!("reading presence: {e}");
                None
            }
        };
        let cmd = NodeCommand {
            op: "transcript".into(),
            seat: key.1.clone(),
            lease_ms: Some(cfg.lease.as_millis() as u64),
            backfill: need_backfill.then_some(cfg.backfill),
            node,
            ..Default::default()
        };
        if st.nodes.send(key.0, cmd) == 0 {
            need_backfill = true;
            st.mirror.set_status(
                &key,
                status(
                    TranscriptState::NodeOffline,
                    "No node of this org is connected, so the session cannot be read.",
                ),
            );
        } else if need_backfill {
            need_backfill = false;
            let (st2, key2) = (st.clone(), key.clone());
            tokio::spawn(async move {
                tokio::time::sleep(cfg.answer_timeout).await;
                let answered = st2
                    .mirror
                    .inner
                    .lock()
                    .unwrap()
                    .seats
                    .get(&key2)
                    .is_none_or(|c| c.answered);
                if !answered {
                    st2.mirror.set_status(
                        &key2,
                        status(
                            TranscriptState::Unknown,
                            "No connected node has a session for this seat.",
                        ),
                    );
                }
            });
        }
        tokio::time::sleep(cfg.lease / 3).await;
    }
}

/// `GET /v1/seats/{name}/transcript` (user tokens): server-sent `status`,
/// `reset` and `events` events.
pub async fn watch(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    auth.require_user()?;
    let seat = st
        .store
        .seat_by_name(auth.org.id, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats have sessions",
        ));
    }
    let open = st.limiters.streams.open(auth.org.id).ok_or_else(|| {
        ApiError::too_many(
            "too_many_streams",
            "this org has too many open event streams",
            5000,
        )
    })?;
    // The mirror ends when its token (or browser session) is revoked.
    let (cred, ended) = st.revocations.open(&auth);
    let key = (auth.org.id, seat.name.clone());
    let (guard, wake, needs_renewer) = st
        .mirror
        .subscribe(&key, || Chan {
            watchers: HashMap::new(),
            buffer: VecDeque::new(),
            bytes: 0,
            queued: 0,
            status: status(
                TranscriptState::Waiting,
                "Asking the node that hosts this seat for its session.",
            ),
            node: None,
            answered: false,
            renewer: None,
        })
        .ok_or_else(|| {
            let mut e = ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "mirror_full",
                "the server holds as much conversation-mirror data as it may; try again later",
            );
            e.retry_after_ms = Some(5000);
            e
        })?;
    if needs_renewer {
        st.mirror
            .set_renewer(&key, tokio::spawn(renew(st.clone(), key.clone())));
    }
    // A watcher that falls too far behind is disconnected (its queue is
    // dropped); the browser reconnects and starts again from the replay
    // events.
    let hub = st.mirror.clone();
    let live = stream::unfold(
        (hub, key, guard, wake),
        |(hub, key, guard, wake)| async move {
            let m = hub.next(&key, guard.id, &wake).await?;
            Some((m, (hub, key, guard, wake)))
        },
    );
    let stream = live.take_until(ended).map(move |m| {
        let _keep = (&open, &cred);
        Ok(sse(m))
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn sse(m: Msg) -> Event {
    match m {
        Msg::Status(s) => Event::default()
            .event("status")
            .data(serde_json::to_string(&s).unwrap_or_default()),
        Msg::Reset => Event::default().event("reset").data("{}"),
        Msg::Events(events) => Event::default().event("events").data(
            serde_json::to_string(&serde_json::json!({ "events": &*events })).unwrap_or_default(),
        ),
    }
}

/// `POST /v1/seats/{name}/transcript` (node tokens). Forwarded to the other
/// instances whose browsers watch the seat, unless it was relayed here.
/// Bodies larger than [`MAX_TRANSCRIPT_POST_BYTES`] are refused (413).
pub async fn post(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    body: Result<axum::body::Bytes, BytesRejection>,
) -> ApiResult<Json<TranscriptPostResponse>> {
    if auth.scope != TokenScope::Node {
        return Err(ApiError::forbidden("requires a node token"));
    }
    let too_large = || {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "a transcript post is limited to {MAX_TRANSCRIPT_POST_BYTES} bytes; send the events in several posts"
            ),
        )
    };
    let body = match body {
        Ok(b) if b.len() > MAX_TRANSCRIPT_POST_BYTES => return Err(too_large()),
        Ok(b) => b,
        Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => return Err(too_large()),
        Err(e) => return Err(ApiError::new(e.status(), "invalid", e.body_text())),
    };
    let body: TranscriptPost = serde_json::from_slice(&body)
        .map_err(|e| ApiError::unprocessable("invalid", format!("invalid transcript post: {e}")))?;
    if let Some(n) = &body.node {
        amail_proto::limits::check_chars("node", n, MAX_NODE_LABEL_CHARS)
            .map_err(ApiError::bad_request)?;
    }
    let relayed = headers.contains_key(crate::cluster::RELAY_HEADER);
    let remote = match (&st.cluster, relayed) {
        (Some(c), false) => {
            let authorization = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            c.relay_transcript(&*st.store, auth.org.id, &name, &authorization, &body)
                .await
        }
        _ => 0,
    };
    let subscribers = st.mirror.post(&(auth.org.id, name), body) + remote;
    Ok(Json(TranscriptPostResponse { subscribers }))
}
