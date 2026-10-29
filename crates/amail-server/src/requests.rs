//! Requests for the person (`/v1/requests…`, M10): one record per thing
//! that waits for the org's user, numbered per org (`r12`).
//!
//! - Seats ask (`question`, `plan`, `merge`) with a seat token, the user
//!   with a user token (naming the seat the request is about); nodes report
//!   `attention` (a seat needs a person and cannot ask), also through their
//!   presence reports ([`sync_attention`]). `permission` is part of the
//!   record but no route creates it yet.
//! - A new request (other than `attention`) is mailed to the user: `To`,
//!   subject `[r12] <title>`, the options as numbered lines; class `now`
//!   is urgent mail. The request's thread is that mail's thread.
//! - The user answers (`POST /requests/{num}/answer`, or a reply in the
//!   thread whose first line names an option). The answer is mailed in the
//!   thread `To` the seat the request is about, so a sleeping seat wakes.
//!   For `attention` the chosen action goes to the seat's node as a
//!   command and nothing is mailed.
//! - Any other reply of the user in the thread is a comment: recorded, and
//!   mailed to the seat when the reply did not reach it in `To`.
//! - The deadline pass runs with the task reminder pass
//!   ([`crate::tasks::scan`]): a request with a default takes it at its
//!   deadline (`expired`, answered by `deadline`, mailed to the seat); one
//!   without a default is reminded to the user, by class, in the user's
//!   one reminder mail of the pass.
//! - Changes publish `request` events (see [`RequestHub`]) on the user's
//!   event stream.
//! - A subject with `[r12]` links the message's thread to request 12.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, OptJson, Path, Query};
use crate::render;
use crate::tasks::Reminder;
use amail_proto::limits::MAX_SUBJECT_CHARS;
use amail_proto::{
    self as p, RequestClass, RequestEventKind, RequestKind, RequestSource, RequestState, SeatKind,
    TokenScope,
};
use amail_store::{
    NewDelivery, NewMessage, NewRequestRec, Org, RequestAnswerRec, RequestBounds, RequestQuery,
    RequestRec, Seat, StoreError,
};
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::broadcast;

// --- live updates ------------------------------------------------------------

const CHANNEL: usize = 256;

/// `request` events per org for this instance's event streams and waits
/// (as [`crate::tasks::TaskHub`] does for `task` events).
#[derive(Default)]
pub struct RequestHub {
    orgs: Mutex<HashMap<i64, broadcast::Sender<p::RequestBusEvent>>>,
}

impl RequestHub {
    pub fn deliver(&self, org_id: i64, ev: p::RequestBusEvent) {
        let mut orgs = self.orgs.lock().unwrap();
        if let Some(tx) = orgs.get(&org_id)
            && tx.send(ev).is_err()
        {
            orgs.remove(&org_id);
        }
    }

    pub fn subscribe(&self, org_id: i64) -> broadcast::Receiver<p::RequestBusEvent> {
        self.orgs
            .lock()
            .unwrap()
            .entry(org_id)
            .or_insert_with(|| broadcast::channel(CHANNEL).0)
            .subscribe()
    }

    pub fn close_org(&self, org_id: i64) {
        self.orgs.lock().unwrap().remove(&org_id);
    }
}

/// Tell this instance's streams and the other instances that a request
/// changed.
pub fn publish(st: &AppState, r: &RequestRec, change: &str) {
    let ev = p::RequestBusEvent {
        num: r.num,
        change: change.to_string(),
        state: r.state,
        seat: r.seat.clone(),
    };
    st.requests.deliver(r.org_id, ev.clone());
    if let Some(c) = &st.cluster {
        c.notify(crate::cluster::Wire::Request { o: r.org_id, v: ev });
    }
}

// --- rendering -----------------------------------------------------------------

pub fn request_out(r: &RequestRec) -> p::Request {
    let answer = r.answer_by.as_ref().map(|by| p::RequestAnswer {
        option: r.answer_key.clone(),
        text: r.answer_text.clone(),
        by: by.clone(),
        at: render::ts(r.answered_at.unwrap_or(r.updated_at)),
        note: r.answer_note.clone(),
    });
    p::Request {
        num: r.num,
        kind: r.kind,
        seat: r.seat.clone(),
        asker: r.asker.clone(),
        task: r.task,
        title: r.title.clone(),
        body: r.body.clone(),
        options: r.options.clone(),
        free_text: r.free_text,
        default: r.default.clone(),
        class: r.class,
        state: r.state,
        deadline: r.deadline.map(render::ts),
        answer,
        thread: r.thread_id,
        source: r.source,
        attention: r.attention.clone(),
        rev: r.rev,
        created_at: render::ts(r.created_at),
        answered_at: r.answered_at.map(render::ts),
    }
}

async fn detail_of(st: &AppState, r: &RequestRec) -> ApiResult<p::RequestDetail> {
    let events = st
        .store
        .request_events(r.org_id, r.num, p::REQUEST_SHOWN_EVENTS)
        .await?;
    let threads = st.store.request_threads(r.org_id, r.num).await?;
    Ok(p::RequestDetail {
        request: request_out(r),
        events: events
            .iter()
            .map(|e| p::RequestEvent {
                seq: e.seq,
                at: render::ts(e.at),
                kind: e.kind,
                actor: e.actor.clone(),
                note: e.note.clone(),
                message_id: e.message_id,
            })
            .collect(),
        threads,
        existing: false,
        terminal_url: None,
    })
}

/// `key — label` of an option, numbered from 1.
fn option_line(r: &RequestRec, key: &str) -> String {
    match r.options.iter().position(|o| o.key == key) {
        Some(i) => format!("{}. {} — {}", i + 1, key, r.options[i].label),
        None => key.to_string(),
    }
}

/// The answer as one line: the option or the text.
fn answer_line(r: &RequestRec) -> String {
    match (&r.answer_key, &r.answer_text) {
        (Some(k), _) => option_line(r, k),
        (None, Some(t)) => t.clone(),
        _ => String::new(),
    }
}

/// The options as lines of the request's mail (or the note that free text
/// is wanted).
fn options_text(r: &RequestRec) -> String {
    let mut out = String::new();
    if !r.options.is_empty() {
        out.push_str("Options:\n");
        for (i, o) in r.options.iter().enumerate() {
            out.push_str(&format!("{}. {} — {}\n", i + 1, o.key, o.label));
            if let Some(d) = o.description.as_deref().filter(|d| !d.trim().is_empty()) {
                for l in d.lines() {
                    out.push_str(&format!("   {l}\n"));
                }
            }
        }
    }
    if r.free_text {
        out.push_str(if r.options.is_empty() {
            "Answer with free text.\n"
        } else {
            "Or answer with free text.\n"
        });
    }
    out
}

/// The block about a request in a reminder mail.
pub fn reminder_block(r: &RequestRec) -> String {
    let mut s = format!(
        "\nr{}: {}\nKind: {}  Class: {}  Seat: {}  Open since: {}",
        r.num,
        r.title,
        r.kind,
        r.class,
        r.seat,
        render::ts(r.created_at)
    );
    if let Some(t) = r.task {
        s.push_str(&format!("  Task: t{t}"));
    }
    s.push('\n');
    s.push_str(&options_text(r));
    s.push_str(&format!(
        "Answer it: amail answer r{} <option>, or reply in its thread with the option's number, key \
         or label on the first line.\n",
        r.num
    ));
    s
}

// --- checks --------------------------------------------------------------------------

fn parse_num(s: &str) -> ApiResult<i64> {
    let t = s.strip_prefix('r').unwrap_or(s);
    match t.parse::<i64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(ApiError::bad_request(format!(
            "{s:?} is not a request number (r12 or 12)"
        ))),
    }
}

fn parse_task(v: &serde_json::Value) -> ApiResult<i64> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.strip_prefix('t').unwrap_or(s).parse().ok(),
        _ => None,
    };
    n.filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad_request("task must be a task number (12 or \"t12\")"))
}

/// Store errors of request writes: `answered`, `stale` and `not_open` are
/// 409, the public bounds 429, other refusals 422, the storage quota 507.
fn store_err(st: &AppState, e: StoreError) -> ApiError {
    match e {
        StoreError::Refused(c @ ("answered" | "stale" | "not_open"), m) => {
            ApiError::new(StatusCode::CONFLICT, c, m)
        }
        StoreError::Refused(c @ ("too_many_open_requests" | "request_rate"), m) => {
            ApiError::too_many(c, m, 60_000)
        }
        StoreError::Refused(code, m) => ApiError::unprocessable(code, m),
        StoreError::StorageQuota(stored) => {
            let mut e = ApiError::storage_quota(stored, st.config.limits.org_storage_bytes);
            e.message = e.message.replace("this message", "this request");
            e
        }
        e => e.into(),
    }
}

/// A 409 that carries the request as it is (`{error, request}`).
async fn conflict_with(st: &AppState, e: ApiError, org_id: i64, num: i64) -> ApiError {
    let mut e = e;
    if let Ok(Some(r)) = st.store.request(org_id, num).await {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "request".into(),
            serde_json::to_value(request_out(&r)).unwrap_or_default(),
        );
        e.extra = Some(extra);
    }
    e
}

async fn agent_seat(st: &AppState, org: &Org, name: &str) -> ApiResult<Seat> {
    let seat = st
        .store
        .seat_by_name(org.id, name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "requests are about agent seats",
        ));
    }
    Ok(seat)
}

/// The request if the caller may see it: user and node tokens any of the
/// org's, seat tokens those about them or made by them.
async fn visible(st: &AppState, auth: &Auth, num: i64) -> ApiResult<RequestRec> {
    let r = st
        .store
        .request(auth.org.id, num)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no request r{num}")))?;
    match auth.scope {
        TokenScope::User | TokenScope::Node => Ok(r),
        TokenScope::Seat => {
            let me = auth.mail_seat()?;
            if r.seat_id == me.id || r.asker_id == Some(me.id) {
                Ok(r)
            } else {
                Err(ApiError::not_found(format!("no request r{num}")))
            }
        }
        TokenScope::Operator => Err(ApiError::forbidden("operator tokens have no requests")),
    }
}

fn bounds(st: &AppState, per_seat: bool) -> RequestBounds {
    let l = &st.config.limits.requests;
    RequestBounds {
        open_per_org: l.open_per_org,
        per_seat_per_hour: if per_seat { l.per_seat_per_hour } else { None },
    }
}

// --- mail ----------------------------------------------------------------------------

/// Send a message from `from` in `thread` (`None`: a new thread) `To`
/// `to` and log it; the message's id and thread. Best effort.
#[allow(clippy::too_many_arguments)]
async fn mail(
    st: &AppState,
    org: &Org,
    from: &Seat,
    to: &Seat,
    thread: Option<(i64, i64)>,
    subject: String,
    body: String,
    urgent: bool,
) -> Option<(i64, i64)> {
    let subject = amail_proto::text::line(&p::cut_chars(&subject, MAX_SUBJECT_CHARS)).into_owned();
    let sent = st
        .store
        .send(&NewMessage {
            org_id: org.id,
            sender_seat_id: from.id,
            thread_id: thread.map(|t| t.0),
            parent_id: thread.map(|t| t.1),
            depth: thread.map_or(0, |_| 1),
            subject,
            body,
            to_addrs: vec![format!("{}@{}", to.name, org.name)],
            cc_addrs: Vec::new(),
            priority: if urgent {
                p::Priority::Urgent
            } else {
                p::Priority::Normal
            },
            ack_required: false,
            meta: Default::default(),
            deliveries: vec![NewDelivery {
                seat_id: to.id,
                org_id: org.id,
                class: p::DeliveryClass::To,
                mode: p::DeliveryMode::Immediate,
            }],
            ack_due: None,
            written_by: None,
            // Mail of a request is never refused: its record was charged.
            storage_quota: None,
            notice: false,
            guards: Vec::new(),
        })
        .await;
    match sent {
        Ok(s) => {
            let events: Vec<(String, i64)> =
                s.seqs.iter().map(|b| (to.name.clone(), b.seq)).collect();
            st.bus.publish(org.id, &events);
            st.activity.record(std::iter::once(org.id));
            Some((s.message_id, s.thread_id))
        }
        Err(e) => {
            tracing::warn!("org {}: request mail to {} not sent: {e}", org.id, to.name);
            None
        }
    }
}

async fn user_seat(st: &AppState, org_id: i64) -> Option<Seat> {
    match st.store.user_seat(org_id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("org {org_id}: no user seat: {e}");
            None
        }
    }
}

async fn system_seat(st: &AppState, org_id: i64) -> Option<Seat> {
    match st.store.system_seat(org_id).await {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("org {org_id}: no system seat: {e}");
            None
        }
    }
}

/// The thread of a request to reply in: (thread, the first message).
fn thread_of(r: &RequestRec) -> Option<(i64, i64)> {
    Some((r.thread_id?, r.message_id?))
}

/// Mail a new request to the user and record its thread.
async fn mail_created(st: &AppState, org: &Org, r: &RequestRec, from: &Seat) {
    let Some(user) = user_seat(st, org.id).await else {
        return;
    };
    let mut body = String::new();
    if !r.body.trim().is_empty() {
        body.push_str(r.body.trim_end());
        body.push_str("\n\n");
    }
    body.push_str(&options_text(r));
    body.push('\n');
    match (&r.default, r.deadline) {
        (Some(d), Some(at)) => body.push_str(&format!(
            "Default: {} (taken at {} if nobody answers).\n",
            option_line(r, d),
            render::ts(at)
        )),
        (None, Some(at)) => body.push_str(&format!(
            "No default: this request stays open until it is answered; reminders from {}.\n",
            render::ts(at)
        )),
        _ => {}
    }
    body.push_str(&format!(
        "Kind: {}  Class: {}  Seat: {}",
        r.kind, r.class, r.seat
    ));
    if let Some(t) = r.task {
        body.push_str(&format!("  Task: t{t}"));
    }
    body.push_str(&format!(
        "\n\nAnswer: reply in this thread with the option's number, key or label on the first \
         line, or run\n    amail answer r{n} <option>\nAny other reply is a comment for {seat}.\n",
        n = r.num,
        seat = r.seat
    ));
    let subject = format!("[r{}] {}", r.num, r.title);
    let urgent = r.class == RequestClass::Now;
    if let Some((mid, tid)) = mail(st, org, from, &user, None, subject, body, urgent).await
        && let Err(e) = st.store.set_request_thread(org.id, r.num, tid, mid).await
    {
        tracing::warn!("org {}: thread of r{} not recorded: {e}", org.id, r.num);
    }
}

/// Mail an answer (or the default taken at the deadline) to the seat the
/// request is about, in the request's thread, from `from`.
async fn mail_answer(st: &AppState, org: &Org, r: &RequestRec, from: &Seat) {
    let Ok(Some(seat)) = st.store.seat_by_name(org.id, &r.seat).await else {
        return;
    };
    if seat.kind != SeatKind::Agent {
        return;
    }
    let mut body = if r.state == RequestState::Expired {
        format!(
            "Nobody answered r{} by its deadline: it took its default.\nAnswer: {}\n",
            r.num,
            answer_line(r)
        )
    } else {
        format!(
            "r{} was answered by {}.\nAnswer: {}\n",
            r.num,
            r.answer_by.as_deref().unwrap_or(""),
            answer_line(r)
        )
    };
    if let Some(t) = &r.answer_text
        && t.contains('\n')
    {
        body = format!(
            "r{} was answered by {}:\n\n{t}\n",
            r.num,
            r.answer_by.as_deref().unwrap_or("")
        );
    }
    if let Some(n) = &r.answer_note {
        body.push_str(&format!("Note: {n}\n"));
    }
    body.push_str(&format!(
        "\nThe request: [r{}] {}\nShow it: amail request show r{}\n",
        r.num, r.title, r.num
    ));
    let subject = format!("Re: [r{}] {}", r.num, r.title);
    mail(st, org, from, &seat, thread_of(r), subject, body, false).await;
}

/// Worklog entry `request` for the seat a request is about.
async fn log(st: &AppState, r: &RequestRec, what: &str) {
    let text = format!("r{} {what}: {}", r.num, r.title);
    crate::handover::log(st, r.org_id, r.seat_id, "request", text, r.task).await;
}

// --- handlers ------------------------------------------------------------------

/// `POST /v1/requests`.
pub async fn create(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewRequest>,
) -> ApiResult<(StatusCode, Json<p::RequestDetail>)> {
    let org = auth.org.clone();
    let kind = req.kind.unwrap_or(RequestKind::Question);
    if kind == RequestKind::Permission {
        return Err(ApiError::unprocessable(
            "kind_not_supported",
            "permission requests are not created yet",
        ));
    }
    let node = auth.scope == TokenScope::Node;
    if node != (kind == RequestKind::Attention) {
        return Err(ApiError::unprocessable(
            "kind_not_allowed",
            if node {
                "node tokens make attention requests only"
            } else {
                "attention requests come from nodes"
            },
        ));
    }
    let (seat, asker, from) = match auth.scope {
        TokenScope::Seat => {
            let me = auth.mail_seat()?.clone();
            if req.seat.as_deref().is_some_and(|s| s != me.name) {
                return Err(ApiError::forbidden(
                    "a seat asks for itself: leave seat out or name your own seat",
                ));
            }
            (me.clone(), Some(me.id), Some(me))
        }
        TokenScope::User | TokenScope::Node => {
            let name = req.seat.as_deref().ok_or_else(|| {
                ApiError::bad_request("name the seat the request is about (seat)")
            })?;
            let seat = agent_seat(&st, &org, name).await?;
            let asker = if node {
                None
            } else {
                Some(auth.mail_seat()?.id)
            };
            (seat, asker, None)
        }
        TokenScope::Operator => {
            return Err(ApiError::forbidden("operator tokens make no requests"));
        }
    };
    let task = req.task.as_ref().map(parse_task).transpose()?;
    let mut n = NewRequestRec {
        org_id: org.id,
        kind,
        seat_id: seat.id,
        asker_id: asker,
        task,
        title: req.title.clone(),
        body: req.body.clone(),
        options: req.options.clone(),
        free_text: req.free_text,
        default: req.default.clone(),
        class: req.class.unwrap_or(RequestClass::Soon),
        deadline: req
            .deadline_s
            .map(|s| amail_store::now_ms() + (s.min(365 * 86_400) as i64) * 1000),
        source: req.source.unwrap_or(RequestSource::Cli),
        attention: None,
        quota: st.config.limits.org_storage_bytes,
        bounds: bounds(&st, auth.scope == TokenScope::Seat),
    };
    if node {
        let code = req
            .attention
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| ApiError::bad_request("attention requests name the code (attention)"))?;
        attention_rec(&mut n, &seat.name, code, Some(req.body.as_str()));
    }
    let (r, existing) = st
        .store
        .create_request(&n)
        .await
        .map_err(|e| store_err(&st, e))?;
    if !existing {
        created(&st, &org, &r, from.as_ref()).await;
    }
    let mut out = detail_of(&st, &st.store.request(org.id, r.num).await?.unwrap_or(r)).await?;
    out.existing = existing;
    let status = if existing {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(out)))
}

/// What follows a new request: its mail to the user (not for
/// `attention`), the seat's worklog entry and the event.
async fn created(st: &AppState, org: &Org, r: &RequestRec, from: Option<&Seat>) {
    if r.kind != RequestKind::Attention {
        let sender = match from {
            Some(s) => Some(s.clone()),
            None => system_seat(st, org.id).await,
        };
        if let Some(s) = sender {
            mail_created(st, org, r, &s).await;
        }
    }
    log(st, r, "created").await;
    publish(st, r, "created");
}

/// Fill in an `attention` request: the fixed options, class `now`, the
/// code, a title and the hint as the body.
fn attention_rec(n: &mut NewRequestRec, seat: &str, code: &str, hint: Option<&str>) {
    let code = p::cut_chars(code, p::MAX_REQUEST_ATTENTION_CHARS);
    n.kind = RequestKind::Attention;
    n.asker_id = None;
    n.title = format!("{seat} needs a person: {code}");
    n.body = hint
        .map(|h| p::cut_chars(h.trim(), 4_000))
        .unwrap_or_default();
    n.options = p::ATTENTION_OPTIONS
        .iter()
        .map(|(k, l)| p::RequestOption {
            key: k.to_string(),
            label: l.to_string(),
            description: None,
        })
        .collect();
    n.free_text = false;
    n.default = None;
    n.class = RequestClass::Now;
    n.deadline = None;
    n.source = RequestSource::Node;
    n.attention = Some(code);
    n.task = None;
}

#[derive(Deserialize, Default)]
pub struct ListParams {
    /// `open` (default), `all`, or a state.
    state: Option<String>,
    kind: Option<RequestKind>,
    seat: Option<String>,
    class: Option<RequestClass>,
    task: Option<String>,
    limit: Option<i64>,
}

/// `GET /v1/requests`: user tokens the org's requests, seat tokens their
/// own (about them or made by them), node tokens those of one seat
/// (`seat` is required; a node checks whether a seat that ended its turn
/// waits for an answer).
pub async fn list(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::RequestList>> {
    let mut query = RequestQuery {
        org_id: auth.org.id,
        kind: q.kind,
        class: q.class,
        limit: q.limit.unwrap_or(200).clamp(1, p::MAX_REQUEST_PAGE),
        ..Default::default()
    };
    match q.state.as_deref().unwrap_or("open") {
        "all" => {}
        s => {
            for part in s.split(',') {
                let state: RequestState = part
                    .trim()
                    .parse()
                    .map_err(|e: String| ApiError::bad_request(e))?;
                query.states.push(state);
            }
        }
    }
    if let Some(t) = &q.task {
        query.task = Some(parse_task(&serde_json::Value::String(t.clone()))?);
    }
    match auth.scope {
        TokenScope::User => {}
        TokenScope::Seat => query.mine = Some(auth.mail_seat()?.id),
        TokenScope::Node if q.seat.is_some() => {}
        TokenScope::Node => {
            return Err(ApiError::bad_request(
                "node tokens list the requests of one seat: give seat",
            ));
        }
        _ => return Err(ApiError::forbidden("requires a user or seat token")),
    }
    if let Some(name) = &q.seat {
        let seat = st
            .store
            .seat_by_name(auth.org.id, name)
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
        query.seat_id = Some(seat.id);
    }
    let (rows, truncated) = st.store.requests(&query).await?;
    Ok(Json(p::RequestList {
        requests: rows.iter().map(request_out).collect(),
        truncated,
    }))
}

/// `GET /v1/requests/{num}`.
pub async fn get(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
) -> ApiResult<Json<p::RequestDetail>> {
    if auth.scope == TokenScope::Node {
        return Err(ApiError::forbidden("requires a user or seat token"));
    }
    let r = visible(&st, &auth, parse_num(&num)?).await?;
    Ok(Json(detail_of(&st, &r).await?))
}

/// The option an answer names: by key, number or label.
fn resolve(r: &RequestRec, a: &p::AnswerRequest) -> ApiResult<(Option<String>, Option<String>)> {
    let text = a.text.clone().filter(|t| !t.trim().is_empty());
    match a.option.as_deref().map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) => match p::match_option(&r.options, o) {
            Some(opt) => Ok((Some(opt.key.clone()), text)),
            None => Err(ApiError::unprocessable(
                "invalid_option",
                format!("r{} has no option {o:?}", r.num),
            )),
        },
        // Text that names an option is that option, unless free text is
        // what the request asks for.
        None => match text {
            Some(t) if !r.free_text || !r.options.is_empty() => {
                match p::match_option(&r.options, &t) {
                    Some(opt) => Ok((Some(opt.key.clone()), None)),
                    None if r.free_text => Ok((None, Some(t))),
                    None => Err(ApiError::unprocessable(
                        "invalid_option",
                        format!("r{} has no option {t:?}", r.num),
                    )),
                }
            }
            t => Ok((None, t)),
        },
    }
}

/// `POST /v1/requests/{num}/answer` (user tokens).
pub async fn answer(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Json(body): Json<p::AnswerRequest>,
) -> ApiResult<Json<p::RequestDetail>> {
    auth.require_user()?;
    let user = auth.mail_seat()?.clone();
    let num = parse_num(&num)?;
    let r = visible(&st, &auth, num).await?;
    let (option, text) = resolve(&r, &body)?;
    let rec = RequestAnswerRec {
        org_id: auth.org.id,
        num,
        option,
        text,
        note: body.note.clone(),
        by: user.name.clone(),
        if_rev: body.if_rev,
        message_id: None,
        quota: st.config.limits.org_storage_bytes,
    };
    let done = match st.store.answer_request(&rec).await {
        Ok(d) => d,
        Err(e) => {
            let e = store_err(&st, e);
            return Err(if e.status == StatusCode::CONFLICT {
                conflict_with(&st, e, auth.org.id, num).await
            } else {
                e
            });
        }
    };
    let terminal_url = answered(&st, &auth.org, &done, &user, true).await;
    let mut out = detail_of(&st, &done).await?;
    out.terminal_url = terminal_url;
    Ok(Json(out))
}

/// What follows an answer by the user: the event, the seat's worklog
/// entry, and the answer mail to the seat (`mail`), or for `attention` the
/// action through the node. The seat's terminal link for `open_terminal`.
async fn answered(
    st: &AppState,
    org: &Org,
    r: &RequestRec,
    by: &Seat,
    mail: bool,
) -> Option<String> {
    publish(st, r, "answered");
    log(st, r, &format!("answered: {}", answer_line(r))).await;
    if r.kind == RequestKind::Attention {
        return attention_action(st, org, r).await;
    }
    if mail {
        mail_answer(st, org, r, by).await;
    }
    None
}

/// Carry out the answer of an `attention` request: `restart` and `sleep`
/// go to the seat's node as an `attention_action` command; `open_terminal`
/// gives the seat's terminal link; `dismiss` does nothing.
async fn attention_action(st: &AppState, org: &Org, r: &RequestRec) -> Option<String> {
    let presence = st.presence.get(org.id, &r.seat).await.ok().flatten();
    match r.answer_key.as_deref() {
        Some(a @ ("restart" | "sleep")) => {
            let cmd = p::NodeCommand {
                op: "attention_action".into(),
                seat: r.seat.clone(),
                node: presence.and_then(|p| p.node),
                request: Some(r.num),
                action: Some(a.to_string()),
                ..Default::default()
            };
            if st.nodes.send(org.id, cmd) == 0 {
                tracing::info!(
                    "org {}: r{} answered {a}, but no node is connected",
                    org.id,
                    r.num
                );
            }
            None
        }
        Some("open_terminal") => presence.and_then(|p| p.terminal_url),
        _ => None,
    }
}

#[derive(Deserialize, Default)]
pub struct WithdrawBody {
    note: Option<String>,
}

/// `POST /v1/requests/{num}/withdraw`: the seat the request is about, the
/// seat that made it, or the user.
pub async fn withdraw(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    OptJson(body): OptJson<WithdrawBody>,
) -> ApiResult<Json<p::RequestDetail>> {
    let me = match auth.scope {
        TokenScope::User | TokenScope::Seat => auth.mail_seat()?.clone(),
        _ => return Err(ApiError::forbidden("requires a user or seat token")),
    };
    let num = parse_num(&num)?;
    visible(&st, &auth, num).await?;
    let note = body
        .and_then(|b| b.note)
        .map(|n| p::cut_chars(n.trim(), p::MAX_REQUEST_NOTE_CHARS))
        .filter(|n| !n.is_empty());
    let r = match st
        .store
        .withdraw_request(auth.org.id, num, &me.name, note.as_deref())
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let e = store_err(&st, e);
            return Err(if e.status == StatusCode::CONFLICT {
                conflict_with(&st, e, auth.org.id, num).await
            } else {
                e
            });
        }
    };
    publish(&st, &r, "withdrawn");
    log(&st, &r, "withdrawn").await;
    // The other party is told in the thread: the user when a seat withdrew
    // it, the seat when the user did.
    let other = if auth.is_user() {
        st.store
            .seat_by_name(auth.org.id, &r.seat)
            .await?
            .filter(|s| s.kind == SeatKind::Agent)
    } else {
        user_seat(&st, auth.org.id).await
    };
    if let Some(to) = other
        && r.thread_id.is_some()
    {
        let mut text = format!("{} withdrew r{}: {}\n", me.name, r.num, r.title);
        if let Some(n) = &note {
            text.push_str(&format!("Note: {n}\n"));
        }
        mail(
            &st,
            &auth.org,
            &me,
            &to,
            thread_of(&r),
            format!("Re: [r{}] {}", r.num, r.title),
            text,
            false,
        )
        .await;
    }
    Ok(Json(detail_of(&st, &r).await?))
}

#[derive(Deserialize, Default)]
pub struct WaitParams {
    timeout_s: Option<u64>,
}

/// `GET /v1/requests/{num}/wait?timeout_s=`: wait until the request is no
/// longer open, at most `timeout_s` (default 60, at most 600) seconds.
/// Seat tokens wait for their own requests; user and node tokens for any.
pub async fn wait(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Query(q): Query<WaitParams>,
) -> ApiResult<Json<p::RequestWait>> {
    let num = parse_num(&num)?;
    let secs = q
        .timeout_s
        .unwrap_or(p::DEFAULT_REQUEST_WAIT_S)
        .min(p::MAX_REQUEST_WAIT_S);
    // Subscribe first, so that a change between the read and the wait is
    // not missed.
    let mut rx = st.requests.subscribe(auth.org.id);
    let mut r = visible(&st, &auth, num).await?;
    let end = tokio::time::Instant::now() + Duration::from_secs(secs);
    while r.is_open() {
        let tick = tokio::time::sleep(Duration::from_secs(5));
        tokio::select! {
            _ = tokio::time::sleep_until(end) => break,
            ev = rx.recv() => match ev {
                Ok(e) if e.num != num => continue,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    tokio::time::sleep_until(end.min(tokio::time::Instant::now() + Duration::from_secs(5))).await;
                    rx = st.requests.subscribe(auth.org.id);
                }
            },
            // A change on another instance whose notification was lost.
            _ = tick => {}
        }
        r = visible(&st, &auth, num).await?;
    }
    Ok(Json(p::RequestWait {
        done: !r.is_open(),
        request: request_out(&r),
    }))
}

// --- mail into requests ---------------------------------------------------------------

/// Request numbers named as `[r12]` in a subject (at most 10).
pub fn subject_tags(subject: &str) -> Vec<i64> {
    crate::tasks::subject_tags_with(subject, 'r')
}

/// The first line of a body that is not empty and not quoted.
fn first_line(body: &str) -> &str {
    body.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('>'))
        .unwrap_or("")
}

/// After a message is stored: a reply of the user in a request's thread
/// answers it when its first line names an option (or, for a free-text
/// question, with its text) and is a comment otherwise; a `[r12]` subject
/// links the thread to request 12. Best effort: errors are logged.
pub async fn after_send(
    st: &AppState,
    org: &Org,
    sender: &Seat,
    written_by: Option<&str>,
    msg: &NewMessage,
    sent: &amail_store::Sent,
) {
    if msg.thread_id.is_some() {
        match st.store.thread_requests(org.id, sent.thread_id).await {
            Ok(rs) => {
                for r in rs.iter().filter(|r| r.is_open()) {
                    reply(st, org, sender, written_by, msg, sent, r).await;
                }
            }
            Err(e) => tracing::warn!("requests of thread {}: {e}", sent.thread_id),
        }
    }
    for num in subject_tags(&msg.subject) {
        let note = format!("[r{num}] in the subject of message {}", sent.message_id);
        match st
            .store
            .link_request_thread(org.id, num, sent.thread_id, Some(&sender.name), &note)
            .await
        {
            Ok(true) => {
                if let Ok(Some(r)) = st.store.request(org.id, num).await {
                    publish(st, &r, "linked");
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("linking thread {} to r{num}: {e}", sent.thread_id),
        }
    }
}

async fn reply(
    st: &AppState,
    org: &Org,
    sender: &Seat,
    written_by: Option<&str>,
    msg: &NewMessage,
    sent: &amail_store::Sent,
    r: &RequestRec,
) {
    let by_user = sender.kind == SeatKind::User && written_by.is_none();
    if by_user {
        let line = first_line(&msg.body);
        let answer = match p::match_option(&r.options, line) {
            Some(o) => Some((Some(o.key.clone()), None)),
            None if r.free_text && r.options.is_empty() && !msg.body.trim().is_empty() => Some((
                None,
                Some(p::cut_chars(msg.body.trim(), p::MAX_REQUEST_ANSWER_CHARS)),
            )),
            None => None,
        };
        if let Some((option, text)) = answer {
            let rec = RequestAnswerRec {
                org_id: org.id,
                num: r.num,
                option,
                text,
                by: sender.name.clone(),
                message_id: Some(sent.message_id),
                quota: None,
                ..Default::default()
            };
            match st.store.answer_request(&rec).await {
                Ok(done) => {
                    // The reply itself tells the seat when it reached it
                    // in `To`.
                    let reached = msg
                        .deliveries
                        .iter()
                        .any(|d| d.seat_id == r.seat_id && d.class == p::DeliveryClass::To);
                    answered(st, org, &done, sender, !reached).await;
                }
                Err(e) => tracing::info!("reply {} to r{}: {e}", sent.message_id, r.num),
            }
            return;
        }
    }
    match st
        .store
        .record_request_event(
            org.id,
            r.num,
            Some(&sender.name),
            RequestEventKind::Commented,
            None,
            Some(sent.message_id),
        )
        .await
    {
        Ok(Some(c)) => publish(st, &c, "commented"),
        Ok(None) => {}
        Err(e) => tracing::warn!("recording a comment of r{}: {e}", r.num),
    }
    // A comment of the user that did not reach the seat is mailed to it.
    let reached = msg.deliveries.iter().any(|d| d.seat_id == r.seat_id);
    if by_user
        && !reached
        && let Ok(Some(seat)) = st.store.seat_by_name(org.id, &r.seat).await
        && seat.kind == SeatKind::Agent
    {
        let body = format!(
            "{} commented on r{} (the request stays open):\n\n{}\n",
            sender.name,
            r.num,
            msg.body.trim_end()
        );
        mail(
            st,
            org,
            sender,
            &seat,
            thread_of(r),
            format!("Re: [r{}] {}", r.num, r.title),
            body,
            false,
        )
        .await;
    }
}

// --- deadlines and reminders -------------------------------------------------------------

/// The deadline pass of [`crate::tasks::scan`]: requests whose default
/// was taken are mailed to their seat; open requests without a default
/// that are due a reminder come back as reminders for the org's user (by
/// org), to go out in the user's one reminder mail of the pass.
pub async fn due_reminders(st: &AppState, now: i64) -> amail_store::Result<Vec<(i64, Reminder)>> {
    let due = st.store.claim_due_requests(now, 500).await?;
    let mut orgs: HashMap<i64, Option<Org>> = HashMap::new();
    for r in &due.expired {
        publish(st, r, "expired");
        log(st, r, &format!("expired: {}", answer_line(r))).await;
        if let std::collections::hash_map::Entry::Vacant(e) = orgs.entry(r.org_id) {
            e.insert(st.store.org_by_id(r.org_id).await?);
        }
        if let (Some(Some(org)), Some(system)) =
            (orgs.get(&r.org_id), system_seat(st, r.org_id).await)
        {
            mail_answer(st, org, r, &system).await;
        }
    }
    let mut users: HashMap<i64, Option<Seat>> = HashMap::new();
    let mut out = Vec::new();
    for r in due.remind {
        publish(st, &r, "reminded");
        if let std::collections::hash_map::Entry::Vacant(e) = users.entry(r.org_id) {
            e.insert(st.store.user_seat(r.org_id).await?);
        }
        let Some(Some(user)) = users.get(&r.org_id) else {
            continue;
        };
        let line = format!(
            "r{} waits for your answer ({}, since {}): {}",
            r.num,
            r.class,
            render::ts(r.created_at),
            r.title
        );
        out.push((
            r.org_id,
            Reminder::request(user.id, user.name.clone(), "request", &r, line),
        ));
    }
    Ok(out)
}

// --- attention from presence -------------------------------------------------------------

/// The attention code of a presence report's entry, as `GET /attention`
/// shows it: the node's `attention`, else a failed turn, else a failed
/// wake-up; with the hint.
pub fn attention_code(u: &p::PresenceUpdate) -> Option<(String, Option<String>)> {
    if let Some(a) = u.attention.as_deref().filter(|a| !a.trim().is_empty()) {
        return Some((a.trim().to_string(), u.attention_hint.clone()));
    }
    if let Some(t) = &u.turn_failed {
        return Some(("turn-failed".into(), Some(t.reason.clone())));
    }
    u.wake_failed.then(|| ("wake-failed".to_string(), None))
}

/// Keep the `attention` requests in step with a node's presence report:
/// a seat reported with a condition gets one live request per code (a new
/// one when none is live); a live request whose seat is reported without
/// that condition ends (withdrawn by `node` if still open). Seats the
/// report leaves out are not changed. Best effort: errors are logged.
pub async fn sync_attention(st: &AppState, org: &Org, seats: &[p::PresenceUpdate]) {
    let live = match st.store.live_attention_requests(org.id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("org {}: attention requests: {e}", org.id);
            return;
        }
    };
    let reported: HashMap<&str, Option<(String, Option<String>)>> = seats
        .iter()
        .map(|u| (u.seat.as_str(), attention_code(u)))
        .collect();
    // Live requests whose condition is gone.
    for r in &live {
        let Some(now) = reported.get(r.seat.as_str()) else {
            continue;
        };
        if now.as_ref().map(|(c, _)| c.as_str()) == r.attention.as_deref() {
            continue;
        }
        match st.store.end_attention_request(org.id, r.num).await {
            Ok(Some(done)) => {
                if done.state != r.state {
                    publish(st, &done, "withdrawn");
                    log(st, &done, "closed (the node reports the condition gone)").await;
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("org {}: ending r{}: {e}", org.id, r.num),
        }
    }
    // New conditions.
    for u in seats {
        let Some(Some((code, hint))) = reported.get(u.seat.as_str()) else {
            continue;
        };
        let code = p::cut_chars(code, p::MAX_REQUEST_ATTENTION_CHARS);
        if live
            .iter()
            .any(|r| r.seat == u.seat && r.attention.as_deref() == Some(code.as_str()))
        {
            continue;
        }
        let seat = match st.store.seat_by_name(org.id, &u.seat).await {
            Ok(Some(s)) if s.kind == SeatKind::Agent => s,
            _ => continue,
        };
        let mut n = NewRequestRec {
            org_id: org.id,
            kind: RequestKind::Attention,
            seat_id: seat.id,
            asker_id: None,
            task: None,
            title: String::new(),
            body: String::new(),
            options: Vec::new(),
            free_text: false,
            default: None,
            class: RequestClass::Now,
            deadline: None,
            source: RequestSource::Node,
            attention: None,
            quota: st.config.limits.org_storage_bytes,
            bounds: bounds(st, false),
        };
        attention_rec(&mut n, &seat.name, &code, hint.as_deref());
        match st.store.create_request(&n).await {
            Ok((r, false)) => created(st, org, &r, None).await,
            Ok((_, true)) => {}
            Err(e) => tracing::warn!("org {}: attention request for {}: {e}", org.id, seat.name),
        }
    }
}

/// Open requests as entries of `GET /attention`, `now` first, oldest
/// first.
pub async fn attention_items(st: &AppState, org_id: i64) -> ApiResult<Vec<p::AttentionItem>> {
    let (rows, _) = st
        .store
        .requests(&RequestQuery {
            org_id,
            states: vec![RequestState::Open],
            limit: p::MAX_REQUEST_PAGE,
            ..Default::default()
        })
        .await?;
    Ok(rows
        .iter()
        .map(|r| p::AttentionItem {
            kind: p::AttentionKind::Request,
            since: render::ts(r.created_at),
            seat: Some(r.seat.clone()),
            message: None,
            delivery: None,
            state: None,
            attention: r.attention.clone(),
            hint: None,
            wake_failed: false,
            turn_failed: None,
            preview: Some(render::lead(&r.body, 200)).filter(|s| !s.is_empty()),
            request: Some(Box::new(request_out(r))),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_lines() {
        assert_eq!(parse_num("r12").unwrap(), 12);
        assert_eq!(parse_num("3").unwrap(), 3);
        assert!(parse_num("t3").is_err() && parse_num("r0").is_err());
        assert_eq!(subject_tags("Re: [r4] ship [t2] [r5]"), vec![4, 5]);
        assert_eq!(first_line("\n\n> quoted\n 2 \nmore"), "2");
        assert_eq!(parse_task(&serde_json::json!("t7")).unwrap(), 7);
        assert_eq!(parse_task(&serde_json::json!(7)).unwrap(), 7);
        assert!(parse_task(&serde_json::json!("x")).is_err());
    }
}
