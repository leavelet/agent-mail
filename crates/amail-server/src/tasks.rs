//! Tasks (`/v1/tasks…`): one shared record per task of an org, with
//! dependencies, linked threads, a discussion thread and a history.
//!
//! - Every call needs a user or seat token, except `GET /tasks/mine`,
//!   which a node token may call for an agent seat (`seat=`) to show the
//!   seat its task count when a session starts. Every call stays inside
//!   the caller's org: task numbers name tasks of that org only, and nothing of a task
//!   reaches another org (a comment mail that reaches another org's seat is
//!   ordinary mail to it).
//! - The actor of every change is the token's seat (a user token: the
//!   user seat). Replies in a discussion thread written by the user in the
//!   name of a seat record the user as `written_by`, as mail does.
//! - Reminders are system mail addressed in `To` (so an idle agent is
//!   woken): never to the seat that made the change, one mail per
//!   recipient per change, and one mail per recipient per background pass
//!   that lists everything due for it (tasks and documents together, see
//!   [`run`]). There is no count limit: the storage quota and the per-org
//!   send limits bound them as they bound all mail.
//! - Changes publish `task` events (see [`TaskHub`]) for the web
//!   interface; nodes do not get them.
//! - A subject with `[t12]` links the message's thread to task 12 of the
//!   sender's org when the message is stored ([`after_send`]).

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, OptJson, Path, Query};
use crate::limits::Pool;
use crate::render;
use amail_proto::limits::*;
use amail_proto::{self as p, TaskPriority, TaskState, TokenScope};
use amail_store::{
    NewDelivery, NewMessage, NewTaskRec, Org, Seat, StoreError, TaskChange, TaskChanged,
    TaskDetailRec, TaskEventRec, TaskOrder, TaskQuery, TaskRec, TaskWaitRec, TaskWrite,
};
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

// --- live updates ------------------------------------------------------------

const CHANNEL: usize = 256;

/// `task` events per org for this instance's event streams (as
/// [`crate::dirwatch::DirectoryHub`] does for `directory` events).
#[derive(Default)]
pub struct TaskHub {
    orgs: Mutex<HashMap<i64, broadcast::Sender<p::TaskBusEvent>>>,
}

impl TaskHub {
    pub fn deliver(&self, org_id: i64, ev: p::TaskBusEvent) {
        let mut orgs = self.orgs.lock().unwrap();
        if let Some(tx) = orgs.get(&org_id)
            && tx.send(ev).is_err()
        {
            orgs.remove(&org_id);
        }
    }

    pub fn subscribe(&self, org_id: i64) -> broadcast::Receiver<p::TaskBusEvent> {
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

/// Tell this instance's streams and the other instances that a task
/// changed.
pub fn publish(st: &AppState, org_id: i64, num: i64, change: &str, fields: &[String]) {
    let ev = p::TaskBusEvent {
        num,
        change: change.to_string(),
        fields: fields.to_vec(),
    };
    st.tasks.deliver(org_id, ev.clone());
    if let Some(c) = &st.cluster {
        c.notify(crate::cluster::Wire::Task { o: org_id, v: ev });
    }
}

// --- rendering -----------------------------------------------------------------

pub fn task_out(t: &TaskRec) -> p::Task {
    p::Task {
        num: t.num,
        title: t.title.clone(),
        body: t.body.clone(),
        state: t.shown_state(),
        stored_state: t.state,
        priority: t.priority,
        rank: t.rank,
        reporter: t.reporter.clone(),
        assignee: t.assignee.clone(),
        team: t.team.clone(),
        parent: t.parent,
        labels: t.labels.clone(),
        due: t.due.map(render::ts),
        waiting_on: t.waiting_on.clone(),
        blocked_reason: t.blocked_reason.clone(),
        thread: t.thread_id,
        waits_on: t.waits_on.clone(),
        waits_on_ok: t.waits_on_ok.clone(),
        rev: t.event_seq,
        progress: t
            .progress
            .map(|(done, total)| p::TaskProgress { done, total }),
        result: t.result,
        result_ref: t.result_ref.clone(),
        result_note: t.result_note.clone(),
        result_by: t.result_by.clone(),
        result_at: t.result_at.map(render::ts),
        import_key: t.import_key.clone(),
        steal_requested_by: t.steal_requested_by.clone(),
        steal_requested_at: t.steal_requested_at.map(render::ts),
        created_at: render::ts(t.created_at),
        updated_at: render::ts(t.updated_at),
        closed_at: t.closed_at.map(render::ts),
    }
}

pub fn task_ref(t: &TaskRec) -> p::TaskRef {
    p::TaskRef {
        num: t.num,
        title: t.title.clone(),
        state: t.shown_state(),
        assignee: t.assignee.clone(),
        result: t.result,
        needs_ok: false,
    }
}

fn event_out(e: &TaskEventRec) -> p::TaskEvent {
    p::TaskEvent {
        seq: e.seq,
        at: render::ts(e.at),
        actor: e.actor.clone(),
        written_by: e.written_by.clone(),
        kind: e.kind,
        field: e.field.clone(),
        from: e.from.clone(),
        to: e.to.clone(),
        note: e.note.clone(),
        message_id: e.message_id,
    }
}

fn detail_out(d: &TaskDetailRec) -> p::TaskDetail {
    p::TaskDetail {
        task: task_out(&d.task),
        blockers: d
            .blockers
            .iter()
            .map(|b| p::TaskRef {
                needs_ok: d.blockers_needs_ok.contains(&b.num),
                ..task_ref(b)
            })
            .collect(),
        blocked: d.blocked.iter().map(task_ref).collect(),
        children: d.children.iter().map(task_ref).collect(),
        links: d
            .links
            .iter()
            .map(|l| p::TaskLink {
                thread_id: l.thread_id,
                linked_by: l.seat.clone(),
                linked_at: render::ts(l.created_at),
            })
            .collect(),
        events: d.events.iter().map(event_out).collect(),
        event_count: d.task.event_seq,
        existing: false,
    }
}

// --- request checks --------------------------------------------------------------

/// The caller's seat: the actor of its changes.
fn actor(auth: &Auth) -> ApiResult<&Seat> {
    if auth.scope == TokenScope::Node {
        return Err(ApiError::forbidden("node tokens cannot use tasks"));
    }
    auth.mail_seat()
}

fn write_ctx(st: &AppState, auth: &Auth, num: i64, if_rev: Option<i64>) -> ApiResult<TaskWrite> {
    Ok(TaskWrite {
        org_id: auth.org.id,
        num,
        actor_id: Some(actor(auth)?.id),
        written_by: None,
        quota: st.config.limits.org_storage_bytes,
        if_rev,
    })
}

#[derive(Deserialize, Default)]
pub struct RevParams {
    if_rev: Option<i64>,
}

/// The error of a refused write to one task. `stale` (the task is not at
/// the revision `if_rev` names) is 409 with the task and the events since
/// that revision: `{error, rev, task, since}`.
async fn write_failed(st: &AppState, w: &TaskWrite, e: StoreError) -> ApiError {
    let StoreError::Refused("stale", msg) = e else {
        return store_err(st, e);
    };
    let mut err = ApiError::new(StatusCode::CONFLICT, "stale", msg);
    let since = w.if_rev.unwrap_or(0);
    let detail = st
        .store
        .task_detail(w.org_id, w.num, TASK_SHOWN_EVENTS)
        .await;
    let events = st
        .store
        .task_events(w.org_id, w.num, since, MAX_TASK_EVENTS_PAGE)
        .await;
    if let (Ok(Some(d)), Ok(Some((evs, _)))) = (detail, events) {
        let mut extra = serde_json::Map::new();
        extra.insert("rev".into(), serde_json::json!(d.task.event_seq));
        extra.insert(
            "task".into(),
            serde_json::to_value(detail_out(&d)).unwrap_or_default(),
        );
        extra.insert(
            "since".into(),
            serde_json::to_value(evs.iter().map(event_out).collect::<Vec<_>>()).unwrap_or_default(),
        );
        err.extra = Some(extra);
    }
    err
}

fn check_result_ref(s: &str) -> ApiResult<String> {
    let t = s.trim();
    if !one_line(t) {
        return Err(ApiError::unprocessable(
            "invalid_ref",
            "result_ref must be a single line without control characters",
        ));
    }
    if t.chars().count() > MAX_TASK_RESULT_REF_CHARS {
        return Err(ApiError::unprocessable(
            "ref_too_long",
            format!("result_ref is limited to {MAX_TASK_RESULT_REF_CHARS} characters"),
        ));
    }
    Ok(t.to_string())
}

fn check_result_note(field: &str, s: &str) -> ApiResult<String> {
    let t = s.trim();
    if t.chars().count() > MAX_TASK_RESULT_NOTE_CHARS {
        return Err(ApiError::unprocessable(
            "note_too_long",
            format!("{field} is limited to {MAX_TASK_RESULT_NOTE_CHARS} characters"),
        ));
    }
    Ok(t.to_string())
}

/// `12` or `t12`.
fn parse_num(s: &str) -> ApiResult<i64> {
    let n = s.strip_prefix('t').unwrap_or(s);
    n.parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad_request(format!("{s:?} is not a task number (t12 or 12)")))
}

fn one_line(s: &str) -> bool {
    !s.chars()
        .any(|c| c.is_control() || amail_proto::text::is_line_break(c))
}

fn check_title(s: &str) -> ApiResult<String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(ApiError::unprocessable(
            "invalid_title",
            "the title is empty",
        ));
    }
    if !one_line(t) {
        return Err(ApiError::unprocessable(
            "invalid_title",
            "the title must be a single line without control characters",
        ));
    }
    if t.chars().count() > MAX_TASK_TITLE_CHARS {
        return Err(ApiError::unprocessable(
            "title_too_long",
            format!("the title is limited to {MAX_TASK_TITLE_CHARS} characters"),
        ));
    }
    Ok(t.to_string())
}

fn check_body(s: &str) -> ApiResult<()> {
    if s.len() > MAX_TASK_BODY_BYTES {
        return Err(ApiError::unprocessable(
            "body_too_large",
            format!("the body is limited to {} KiB", MAX_TASK_BODY_BYTES / 1024),
        ));
    }
    Ok(())
}

fn check_labels(v: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for l in v {
        let l = l.trim();
        if l.is_empty()
            || l.chars().count() > MAX_TASK_LABEL_CHARS
            || l.chars()
                .any(|c| c == ',' || c.is_whitespace() || c.is_control())
        {
            return Err(ApiError::unprocessable(
                "invalid_label",
                format!(
                    "invalid label {l:?}: 1-{MAX_TASK_LABEL_CHARS} characters, no commas, spaces or control characters"
                ),
            ));
        }
        if !out.iter().any(|x| x == l) {
            out.push(l.to_string());
        }
    }
    if out.len() > MAX_TASK_LABELS {
        return Err(ApiError::unprocessable(
            "too_many_labels",
            format!("a task has at most {MAX_TASK_LABELS} labels"),
        ));
    }
    Ok(out)
}

fn check_reason(s: &str) -> ApiResult<String> {
    let t = s.trim();
    if !one_line(t) {
        return Err(ApiError::unprocessable(
            "invalid_reason",
            "blocked_reason must be a single line without control characters",
        ));
    }
    if t.chars().count() > MAX_TASK_REASON_CHARS {
        return Err(ApiError::unprocessable(
            "reason_too_long",
            format!("blocked_reason is limited to {MAX_TASK_REASON_CHARS} characters"),
        ));
    }
    Ok(t.to_string())
}

fn parse_due(s: &str) -> ApiResult<i64> {
    render::parse_ts(s.trim())
        .ok_or_else(|| ApiError::bad_request("due must be an RFC 3339 timestamp"))
}

/// A live seat of the org (not the hidden system seat).
async fn seat_id(st: &AppState, org: &Org, name: &str) -> ApiResult<i64> {
    match st.store.seat_by_name(org.id, name.trim()).await? {
        Some(s) if s.kind != p::SeatKind::System => Ok(s.id),
        _ => Err(ApiError::not_found(format!("no seat {name:?}"))),
    }
}

async fn team_id(st: &AppState, org: &Org, name: &str) -> ApiResult<i64> {
    st.store
        .teams(org.id)
        .await?
        .into_iter()
        .find(|t| t.name == name.trim())
        .map(|t| t.id)
        .ok_or_else(|| ApiError::not_found(format!("no team {name:?}")))
}

/// Store errors of task writes: refusals are 422 with their code (`taken`
/// is 409), the storage quota 507 with the server's limit.
fn store_err(st: &AppState, e: StoreError) -> ApiError {
    match e {
        StoreError::Refused("taken", m) => ApiError::new(StatusCode::CONFLICT, "taken", m),
        StoreError::Refused("steal_pending", m) => {
            ApiError::new(StatusCode::CONFLICT, "steal_pending", m)
        }
        StoreError::Refused("not_assignee", m) => {
            ApiError::new(StatusCode::FORBIDDEN, "not_assignee", m)
        }
        StoreError::Refused("children_open", m) => {
            ApiError::new(StatusCode::CONFLICT, "children_open", m)
        }
        StoreError::Refused("stale", m) => ApiError::new(StatusCode::CONFLICT, "stale", m),
        StoreError::Refused(code, m) => ApiError::unprocessable(code, m),
        StoreError::StorageQuota(stored) => {
            let mut e = ApiError::storage_quota(stored, st.config.limits.org_storage_bytes);
            e.message = e.message.replace("this message", "this change");
            e
        }
        e => e.into(),
    }
}

async fn detail(st: &AppState, org_id: i64, num: i64) -> ApiResult<p::TaskDetail> {
    st.store
        .task_detail(org_id, num, TASK_SHOWN_EVENTS)
        .await?
        .map(|d| detail_out(&d))
        .ok_or_else(|| ApiError::not_found(format!("no task t{num}")))
}

/// The thread a link request names, if the caller sees it: user tokens
/// any thread of the org's mail, seat tokens the threads they are in.
async fn visible_thread(st: &AppState, auth: &Auth, req: &p::TaskLinkRequest) -> ApiResult<i64> {
    let seat = actor(auth)?;
    let thread = match (req.thread_id, req.message_id) {
        (Some(t), _) => t,
        (None, Some(m)) => match st.store.message(m).await? {
            Some(msg) if crate::mail::can_read(st, auth, &msg).await? => msg.thread_id,
            _ => return Err(ApiError::not_found(format!("no message {m}"))),
        },
        (None, None) => return Err(ApiError::bad_request("give thread_id or message_id")),
    };
    let visible = match auth.scope {
        TokenScope::Seat => st.store.seat_in_thread(seat.id, thread).await?,
        _ => st.store.org_sees_thread(auth.org.id, thread).await?,
    };
    if !visible {
        return Err(ApiError::not_found(format!("no thread {thread}")));
    }
    Ok(thread)
}

// --- handlers ------------------------------------------------------------------

/// `POST /v1/tasks`.
pub async fn create(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::NewTask>,
) -> ApiResult<(StatusCode, Json<p::TaskDetail>)> {
    let me = actor(&auth)?.clone();
    let org = &auth.org;
    let mut link_thread = None;
    let mut default_title = None;
    if let Some(m) = req.from_message {
        match st.store.message(m).await? {
            Some(msg) if crate::mail::can_read(&st, &auth, &msg).await? => {
                link_thread = Some(msg.thread_id);
                let t: String = amail_proto::text::line(&msg.subject)
                    .chars()
                    .take(MAX_TASK_TITLE_CHARS)
                    .collect();
                default_title = Some(if t.trim().is_empty() {
                    format!("message {m}")
                } else {
                    t
                });
            }
            _ => return Err(ApiError::not_found(format!("no message {m}"))),
        }
    }
    let title = match (req.title.as_deref(), default_title) {
        (Some(t), _) => check_title(t)?,
        (None, Some(t)) => check_title(&t)?,
        (None, None) => return Err(ApiError::bad_request("title is required")),
    };
    let body = req.body.unwrap_or_default();
    check_body(&body)?;
    let labels = check_labels(&req.labels)?;
    let assignee_id = match &req.assignee {
        Some(n) => Some(seat_id(&st, org, n).await?),
        None => None,
    };
    let team = match &req.team {
        Some(n) => Some(team_id(&st, org, n).await?),
        None => None,
    };
    let due = req.due.as_deref().map(parse_due).transpose()?;
    if let Some(k) = &req.client_key {
        let n = k.chars().count();
        if n == 0 || n > MAX_TASK_CLIENT_KEY_CHARS || k.chars().any(char::is_control) {
            return Err(ApiError::unprocessable(
                "invalid_client_key",
                format!(
                    "client_key is 1 to {MAX_TASK_CLIENT_KEY_CHARS} characters without control characters"
                ),
            ));
        }
    }
    let (t, existing) = st
        .store
        .create_task_once(&NewTaskRec {
            org_id: org.id,
            reporter_id: me.id,
            written_by: None,
            title,
            body,
            priority: req.priority,
            assignee_id,
            team_id: team,
            parent: req.parent,
            labels,
            due,
            link_thread,
            quota: st.config.limits.org_storage_bytes,
            client_key: req.client_key.clone(),
        })
        .await
        .map_err(|e| store_err(&st, e))?;
    if existing {
        let mut d = detail(&st, org.id, t.num).await?;
        d.existing = true;
        return Ok((StatusCode::OK, Json(d)));
    }
    publish(&st, org.id, t.num, "created", &[]);
    crate::docs::link_task_title(&st, org.id, Some(me.id), &t).await;
    crate::handover::task_created(&st, org, &me, &t).await;
    if let Some(a) = t.assignee_id
        && a != me.id
    {
        let r = Reminder::new(
            a,
            t.assignee.clone().unwrap_or_default(),
            "assigned",
            &t,
            format!("t{} assigned to you: {}", t.num, t.title),
        );
        remind(&st, org, Some(me.id), vec![r]).await;
    }
    Ok((StatusCode::CREATED, Json(detail(&st, org.id, t.num).await?)))
}

/// `GET /v1/tasks/{num}`.
pub async fn get(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
) -> ApiResult<Json<p::TaskDetail>> {
    actor(&auth)?;
    Ok(Json(detail(&st, auth.org.id, parse_num(&num)?).await?))
}

/// `PATCH /v1/tasks/{num}`.
pub async fn patch(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Json(req): Json<p::TaskPatch>,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let org = &auth.org;
    if req.state == Some(TaskState::Ready) {
        return Err(ApiError::bad_request(
            "`ready` is derived and cannot be set: set `open`; the task is ready when its blockers are closed",
        ));
    }
    let mut ch = TaskChange {
        title: req.title.as_deref().map(check_title).transpose()?,
        state: req.state,
        priority: req.priority,
        rank: req.rank,
        parent: req.parent,
        labels: req.labels.as_deref().map(check_labels).transpose()?,
        ..Default::default()
    };
    if let Some(b) = req.body {
        check_body(&b)?;
        ch.body = Some(b);
    }
    if let Some(a) = &req.assignee {
        ch.assignee_id = Some(match a {
            Some(n) => Some(seat_id(&st, org, n).await?),
            None => None,
        });
    }
    if let Some(t) = &req.team {
        ch.team_id = Some(match t {
            Some(n) => Some(team_id(&st, org, n).await?),
            None => None,
        });
    }
    if let Some(d) = &req.due {
        ch.due = Some(d.as_deref().map(parse_due).transpose()?);
    }
    if let Some(wo) = &req.waiting_on {
        ch.waiting_on_id = Some(match wo {
            Some(n) => Some(seat_id(&st, org, n).await?),
            None => None,
        });
    }
    if let Some(r) = &req.blocked_reason {
        ch.blocked_reason = Some(match r {
            Some(r) => Some(check_reason(r)?).filter(|r| !r.is_empty()),
            None => None,
        });
    }
    if !matches!(req.result, Some(Some(_)))
        && (req.result_ref.is_some() || req.result_note.is_some())
    {
        return Err(ApiError::bad_request(
            "result_ref and result_note go with result (ok or rejected)",
        ));
    }
    ch.result = req.result;
    ch.result_ref = match &req.result_ref {
        Some(r) => Some(check_result_ref(r)?).filter(|r| !r.is_empty()),
        None => None,
    };
    ch.result_note = match &req.result_note {
        Some(n) => Some(check_result_note("result_note", n)?).filter(|n| !n.is_empty()),
        None => None,
    };
    ch.note = match &req.note {
        Some(n) => Some(check_result_note("note", n)?).filter(|n| !n.is_empty()),
        None => None,
    };
    ch.force = req.force;
    let changed = match st.store.change_task(&w, &ch).await {
        Ok(c) => c,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    after_change(&st, org, w.actor_id, &changed, "changed").await;
    Ok(Json(detail(&st, org.id, num).await?))
}

/// `POST /v1/tasks/{num}/take`.
/// `POST /v1/tasks/{num}/steal`: ask the assignee to hand over a task
/// that is not started; the assignee is told by system mail what to run.
pub async fn steal(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, None)?;
    let ch = st
        .store
        .steal_task(&w)
        .await
        .map_err(|e| store_err(&st, e))?;
    if !ch.fields.is_empty() {
        publish(&st, auth.org.id, num, "steal", &ch.fields);
        let t = &ch.task;
        if let (Some(a), Some(an), Some(by)) = (t.assignee_id, &t.assignee, &t.steal_requested_by) {
            let mut r = Reminder::new(
                a,
                an.clone(),
                "steal",
                t,
                format!("{by} asks to take over t{}: {}", t.num, t.title),
            );
            r.extra = Some(format!(
                "\nTo hand the task to {by}, run:\n    amail task yield t{n}\nTo keep it, run:\n    amail task keep t{n}\n\
                 Only the command answers: a reply to this mail does not. Without an answer the \
                 request expires after 24 hours and the task stays with you.\n",
                n = t.num
            ));
            remind(&st, &auth.org, w.actor_id, vec![r]).await;
        }
    }
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

/// `POST /v1/tasks/{num}/yield` and `/keep`: the assignee (or the org's
/// user) answers a pending steal request.
async fn answer(
    st: AppState,
    auth: Auth,
    num: String,
    give: bool,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, None)?;
    let ch = st
        .store
        .answer_task_steal(&w, give, auth.is_user())
        .await
        .map_err(|e| store_err(&st, e))?;
    if give {
        after_change(&st, &auth.org, w.actor_id, &ch, "assigned").await;
    } else {
        publish(&st, auth.org.id, num, "steal", &ch.fields);
        remind(&st, &auth.org, w.actor_id, steal_ended(&ch)).await;
    }
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

pub async fn yield_task(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
) -> ApiResult<Json<p::TaskDetail>> {
    answer(st, auth, num, true).await
}

pub async fn keep(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
) -> ApiResult<Json<p::TaskDetail>> {
    answer(st, auth, num, false).await
}

/// The requester of a steal request that this change ended without giving
/// it the task: told why.
fn steal_ended(ch: &TaskChanged) -> Vec<Reminder> {
    let (b, t) = (&ch.before, &ch.task);
    let (Some(by), Some(by_name)) = (b.steal_requested_by_id, &b.steal_requested_by) else {
        return Vec::new();
    };
    if t.steal_requested_by_id.is_some() || t.assignee_id == Some(by) {
        return Vec::new();
    }
    let owner = b.assignee.clone().unwrap_or_default();
    let line = if t.assignee_id == b.assignee_id && t.state == b.state {
        format!(
            "t{} stays with {owner}: your request to take it over was declined",
            t.num
        )
    } else if t.assignee_id != b.assignee_id {
        format!(
            "t{}: your request to take it over ended: the task was reassigned",
            t.num
        )
    } else {
        format!(
            "t{}: your request to take it over ended: the task is now {}",
            t.num, t.state
        )
    };
    vec![Reminder::new(
        by,
        by_name.clone(),
        "steal_declined",
        t,
        line,
    )]
}

pub async fn take(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    OptJson(req): OptJson<p::TaskRevRequest>,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, req.and_then(|r| r.if_rev))?;
    let changed = match st.store.take_task(&w).await {
        Ok(c) => c,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    after_change(&st, &auth.org, w.actor_id, &changed, "assigned").await;
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

/// `POST /v1/tasks/{num}/deps`.
pub async fn add_dep(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Json(req): Json<p::TaskDepRequest>,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let changed = match st
        .store
        .set_task_dep(&w, req.blocked_by, req.needs_ok)
        .await
    {
        Ok(c) => c,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    if !changed.fields.is_empty() {
        publish(&st, auth.org.id, num, "dependency", &changed.fields);
        publish(&st, auth.org.id, req.blocked_by, "dependency", &[]);
    }
    let rs = unblocked_reminders(&changed, |_| {
        format!(
            "t{} is no longer blocked: any close of t{} satisfies it now",
            num, req.blocked_by
        )
    });
    remind(&st, &auth.org, w.actor_id, rs).await;
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

/// `DELETE /v1/tasks/{num}/deps/{other}`.
pub async fn remove_dep(
    State(st): State<AppState>,
    auth: Auth,
    Path((num, other)): Path<(String, String)>,
    Query(q): Query<RevParams>,
) -> ApiResult<Json<p::TaskDetail>> {
    let (num, other) = (parse_num(&num)?, parse_num(&other)?);
    let w = write_ctx(&st, &auth, num, q.if_rev)?;
    let changed = match st.store.remove_task_dep(&w, other).await {
        Ok(c) => c,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    publish(&st, auth.org.id, num, "dependency", &changed.fields);
    publish(&st, auth.org.id, other, "dependency", &[]);
    let rs = unblocked_reminders(&changed, |t| {
        format!(
            "t{} is no longer blocked: dependency on t{other} removed",
            t.num
        )
    });
    remind(&st, &auth.org, w.actor_id, rs).await;
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

/// `POST /v1/tasks/{num}/links`.
pub async fn add_link(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Json(req): Json<p::TaskLinkRequest>,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, req.if_rev)?;
    let thread = visible_thread(&st, &auth, &req).await?;
    let linked = match st.store.add_task_link(&w, thread, false, None).await {
        Ok(l) => l,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    if linked {
        publish(&st, auth.org.id, num, "link", &[]);
    }
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

/// `DELETE /v1/tasks/{num}/links/{thread}`.
pub async fn remove_link(
    State(st): State<AppState>,
    auth: Auth,
    Path((num, thread)): Path<(String, i64)>,
    Query(q): Query<RevParams>,
) -> ApiResult<Json<p::TaskDetail>> {
    let num = parse_num(&num)?;
    let w = write_ctx(&st, &auth, num, q.if_rev)?;
    let removed = match st.store.remove_task_link(&w, thread).await {
        Ok(r) => r,
        Err(e) => return Err(write_failed(&st, &w, e).await),
    };
    if !removed {
        return Err(ApiError::not_found(format!(
            "thread {thread} is not linked to t{num}"
        )));
    }
    publish(&st, auth.org.id, num, "link", &[]);
    Ok(Json(detail(&st, auth.org.id, num).await?))
}

/// `POST /v1/tasks/{num}/comments`: a message in the task's discussion
/// thread (created by the first comment) with subject `[t12] <title>`, to
/// the assignee and the reporter, copied to the seats that commented
/// before (the sender left out; with nobody else, to the sender).
pub async fn comment(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Json(req): Json<p::TaskCommentRequest>,
) -> ApiResult<Json<p::TaskCommentResponse>> {
    let num = parse_num(&num)?;
    let me = actor(&auth)?.clone();
    if req.body.trim().is_empty() {
        return Err(ApiError::bad_request("the comment is empty"));
    }
    let t = st
        .store
        .task(auth.org.id, num)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no task t{num}")))?;
    let mut to: Vec<String> = Vec::new();
    for n in [t.assignee.as_deref(), Some(t.reporter.as_str())]
        .into_iter()
        .flatten()
    {
        if n != me.name && !to.iter().any(|x| x == n) {
            to.push(n.to_string());
        }
    }
    let mut cc: Vec<String> = Vec::new();
    for a in st.store.task_commenters(auth.org.id, num).await? {
        if a != me.name && !to.contains(&a) && !cc.contains(&a) {
            cc.push(a);
        }
    }
    // Seats that were deleted meanwhile are left out.
    let mut names = to.clone();
    names.extend(cc.iter().cloned());
    let live: Vec<String> = st
        .store
        .seats_by_names(auth.org.id, &names)
        .await?
        .into_iter()
        .map(|s| s.name)
        .collect();
    to.retain(|n| live.contains(n));
    cc.retain(|n| live.contains(n));
    if to.is_empty() && cc.is_empty() {
        to.push(me.name.clone());
    }
    let subject: String = format!("[t{num}] {}", t.title)
        .chars()
        .take(MAX_SUBJECT_CHARS)
        .collect();
    let sent = crate::send::send_comment(
        &st,
        &auth,
        &me,
        p::SendRequest {
            to,
            cc,
            subject,
            body: req.body,
            in_reply_to: t.thread_id,
            ..Default::default()
        },
    )
    .await?;
    let w = write_ctx(&st, &auth, num, None)?;
    let t = st
        .store
        .record_task_comment(&w, sent.id, sent.thread_id)
        .await
        .map_err(|e| store_err(&st, e))?;
    publish(&st, auth.org.id, num, "commented", &[]);
    Ok(Json(p::TaskCommentResponse {
        id: sent.id,
        thread_id: t.thread_id.unwrap_or(sent.thread_id),
        task: num,
    }))
}

#[derive(Deserialize)]
pub struct EventsParams {
    after_seq: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/tasks/{num}/events`.
pub async fn events(
    State(st): State<AppState>,
    auth: Auth,
    Path(num): Path<String>,
    Query(q): Query<EventsParams>,
) -> ApiResult<Json<p::TaskEventsPage>> {
    actor(&auth)?;
    let num = parse_num(&num)?;
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_TASK_EVENTS_PAGE);
    let (evs, compacted) = st
        .store
        .task_events(auth.org.id, num, q.after_seq.unwrap_or(0), limit + 1)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no task t{num}")))?;
    let more = evs.len() as i64 > limit;
    let events: Vec<p::TaskEvent> = evs.iter().take(limit as usize).map(event_out).collect();
    Ok(Json(p::TaskEventsPage {
        next_after_seq: if more {
            events.last().map(|e| e.seq)
        } else {
            None
        },
        events,
        events_compacted: compacted,
    }))
}

#[derive(Deserialize, Default)]
pub struct ListParams {
    state: Option<String>,
    assignee: Option<String>,
    reporter: Option<String>,
    team: Option<String>,
    priority: Option<String>,
    label: Option<String>,
    parent: Option<String>,
    due_before: Option<String>,
    stale: Option<String>,
    text: Option<String>,
    before: Option<i64>,
    limit: Option<i64>,
    pool: Option<String>,
    /// `mine` with a node token: the agent seat.
    seat: Option<String>,
    /// `ok`, `rejected` or `none`.
    result: Option<String>,
    /// The task imported from the record with this id.
    import_key: Option<String>,
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true" | "yes" | ""))
}

/// `state=`: comma-separated shown states, `all`, or (absent) every state
/// but `done` and `cancelled`.
fn states(v: Option<&str>) -> ApiResult<Vec<TaskState>> {
    match v.map(str::trim) {
        None | Some("") => Ok(vec![
            TaskState::Ready,
            TaskState::Open,
            TaskState::Doing,
            TaskState::Blocked,
            TaskState::Review,
        ]),
        Some("all") => Ok(Vec::new()),
        Some(s) => s
            .split(',')
            .map(|x| x.trim().parse::<TaskState>().map_err(ApiError::bad_request))
            .collect(),
    }
}

fn stale_cut(org: &Org) -> Option<i64> {
    (org.task_stale_hours > 0).then(|| amail_store::now_ms() - org.task_stale_hours * 3_600_000)
}

/// `GET /v1/tasks`.
pub async fn list(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::TaskList>> {
    actor(&auth)?;
    let org = &auth.org;
    let mut query = TaskQuery {
        org_id: org.id,
        states: states(q.state.as_deref())?,
        before: q.before,
        limit: q.limit.unwrap_or(50).clamp(1, MAX_TASK_PAGE),
        order: TaskOrder::Newest,
        ..Default::default()
    };
    if let Some(a) = &q.assignee {
        query.assignee_id = Some(match a.as_str() {
            "none" | "" => None,
            n => Some(seat_id(&st, org, n).await?),
        });
    }
    if let Some(r) = &q.reporter {
        query.reporter_id = Some(seat_id(&st, org, r).await?);
    }
    if let Some(t) = &q.team {
        query.team_id = Some(team_id(&st, org, t).await?);
    }
    if let Some(pr) = &q.priority {
        query.priority = Some(pr.parse::<TaskPriority>().map_err(ApiError::bad_request)?);
    }
    if let Some(l) = &q.label {
        query.label = Some(l.trim().to_string());
    }
    if let Some(pn) = &q.parent {
        query.parent = Some(parse_num(pn)?);
    }
    if let Some(d) = &q.due_before {
        query.due_before = Some(parse_due(d)?);
    }
    if let Some(r) = q.result.as_deref().map(str::trim) {
        query.result = Some(match r {
            "none" => None,
            r => Some(r.parse::<p::TaskResult>().map_err(ApiError::bad_request)?),
        });
    }
    if let Some(k) = &q.import_key {
        query.import_key = Some(k.clone());
        // An imported task is found in every state.
        if q.state.is_none() {
            query.states = Vec::new();
        }
    }
    if flag(&q.stale) {
        match stale_cut(org) {
            Some(cut) => query.stale_before = Some(cut),
            None => {
                return Ok(Json(p::TaskList {
                    tasks: Vec::new(),
                    next_before: None,
                    truncated: false,
                }));
            }
        }
    }
    if let Some(t) = q.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        if t.chars().count() > 200 {
            return Err(ApiError::bad_request("text is limited to 200 characters"));
        }
        query.text = Some(t.to_string());
    }
    let page = if query.text.is_some() {
        let store = st.store.clone();
        crate::mail::limited(&st, &auth, Pool::Other, async move {
            Ok(store.tasks(&query).await?)
        })
        .await?
    } else {
        st.store.tasks(&query).await?
    };
    Ok(Json(p::TaskList {
        tasks: page.tasks.iter().map(task_out).collect(),
        next_before: page.next_before,
        truncated: page.truncated,
    }))
}

/// `GET /v1/tasks/mine`: the caller's unfinished tasks, `ready` first, then
/// `doing`, `review`, `blocked`, `open`; within each by priority, rank
/// and number. A node token names an agent seat with `seat=` (as for the
/// seat's inbox): the node shows the seat its count at session start.
pub async fn mine(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::TaskList>> {
    let node_seat;
    let me = if auth.scope == TokenScope::Node {
        node_seat = auth.mailbox(&st, q.seat.as_deref()).await?;
        &node_seat
    } else {
        actor(&auth)?
    };
    let page = st
        .store
        .tasks(&TaskQuery {
            org_id: auth.org.id,
            states: states(q.state.as_deref())?,
            assignee_id: Some(Some(me.id)),
            limit: q.limit.unwrap_or(200).clamp(1, MAX_TASK_PAGE),
            order: TaskOrder::Personal,
            ..Default::default()
        })
        .await?;
    Ok(Json(p::TaskList {
        tasks: page.tasks.iter().map(task_out).collect(),
        next_before: None,
        truncated: page.truncated,
    }))
}

/// `GET /v1/tasks/next[?pool=1]`: the caller's `ready` tasks (with
/// `pool`, also the unassigned `ready` tasks of its team), by priority,
/// rank and number.
pub async fn next(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::TaskList>> {
    let me = actor(&auth)?;
    let pool_team = if flag(&q.pool) { me.team_id } else { None };
    let page = st
        .store
        .tasks(&TaskQuery {
            org_id: auth.org.id,
            states: vec![TaskState::Ready],
            assignee_id: Some(Some(me.id)),
            pool_team_id: pool_team,
            limit: q.limit.unwrap_or(50).clamp(1, MAX_TASK_PAGE),
            order: TaskOrder::Priority,
            ..Default::default()
        })
        .await?;
    Ok(Json(p::TaskList {
        tasks: page.tasks.iter().map(task_out).collect(),
        next_before: None,
        truncated: page.truncated,
    }))
}

/// `GET /v1/tasks/waiting`.
pub async fn waiting(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::WaitingList>> {
    let me = actor(&auth)?;
    let (items, truncated) = st
        .store
        .tasks_waiting(auth.org.id, me.id, q.limit.unwrap_or(200))
        .await?;
    Ok(Json(p::WaitingList {
        items: items
            .iter()
            .map(|w: &TaskWaitRec| p::WaitingItem {
                task: task_out(&w.task),
                seat: w.seat.clone(),
                blocker: w.other.as_ref().map(task_ref),
            })
            .collect(),
        truncated,
    }))
}

/// `GET /v1/tasks/owed`.
pub async fn owed(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::OwedList>> {
    let me = actor(&auth)?;
    let (items, truncated) = st
        .store
        .tasks_owed(auth.org.id, me.id, q.limit.unwrap_or(200))
        .await?;
    Ok(Json(p::OwedList {
        items: items
            .iter()
            .map(|w| p::OwedItem {
                task: task_out(&w.task),
                seat: w.seat.clone(),
                blocks: w.other.as_ref().map(task_ref),
            })
            .collect(),
        truncated,
    }))
}

/// `GET /v1/tasks/board[?team=]`.
pub async fn board(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::TaskBoard>> {
    actor(&auth)?;
    let team = match &q.team {
        Some(t) => Some(team_id(&st, &auth.org, t).await?),
        None => None,
    };
    let (store, org_id, hours) = (st.store.clone(), auth.org.id, auth.org.task_stale_hours);
    let b = crate::mail::limited(&st, &auth, Pool::Other, async move {
        Ok(store
            .task_board(org_id, team, hours, amail_store::now_ms())
            .await?)
    })
    .await?;
    let list = |v: &[TaskRec]| v.iter().map(task_out).collect::<Vec<_>>();
    Ok(Json(p::TaskBoard {
        team: q.team.clone(),
        columns: b
            .columns
            .iter()
            .map(|(state, count, tasks)| p::BoardColumn {
                state: *state,
                count: *count,
                tasks: list(tasks),
            })
            .collect(),
        load: b
            .load
            .iter()
            .map(|l| p::SeatLoad {
                seat: l.seat.clone(),
                doing: l.doing,
                ready: l.ready,
                blocked: l.blocked,
                waiting: l.waiting,
                review: l.review,
            })
            .collect(),
        stale: list(&b.stale),
        overdue: list(&b.overdue),
        chains: b
            .chains
            .iter()
            .map(|t| p::BoardChain {
                num: t.num,
                waits_on: t.waits_on.clone(),
                waits_on_ok: t.waits_on_ok.clone(),
            })
            .collect(),
    }))
}

/// `GET /v1/tasks/report[?team=]`: the lead's report.
pub async fn report(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<p::TaskReport>> {
    actor(&auth)?;
    let team = match &q.team {
        Some(t) => Some(team_id(&st, &auth.org, t).await?),
        None => None,
    };
    let (store, org_id) = (st.store.clone(), auth.org.id);
    let r = crate::mail::limited(&st, &auth, Pool::Other, async move {
        Ok(store.task_report(org_id, team, MAX_TASK_PAGE).await?)
    })
    .await?;
    Ok(Json(p::TaskReport {
        team: q.team.clone(),
        sections: r
            .sections
            .into_iter()
            .map(|(name, count, items)| p::ReportSection {
                name: name.to_string(),
                count,
                items: items
                    .iter()
                    .map(|i| p::ReportItem {
                        task: task_out(&i.task),
                        seat: i.seat.clone(),
                        waiting_for: i.waiting_for.clone(),
                        dependents: i.dependents.iter().map(task_ref).collect(),
                    })
                    .collect(),
            })
            .collect(),
    }))
}

/// The fields of an import record (others are ignored with a warning).
const IMPORT_FIELDS: &[&str] = &[
    "id",
    "title",
    "body",
    "state",
    "priority",
    "assignee",
    "reporter",
    "labels",
    "due",
    "parent",
    "depends_on",
    "result",
    "created_at",
    "updated_at",
    "history",
];

/// A name or text of an import record: one line, at most `max`
/// characters.
fn import_line(what: &str, s: &str, max: usize) -> Result<String, String> {
    let t = s.trim();
    if t.is_empty() || !one_line(t) || t.chars().count() > max {
        return Err(format!("{what} must be one line of 1 to {max} characters"));
    }
    Ok(t.to_string())
}

/// Check one import record and resolve its seats.
async fn import_record(
    st: &AppState,
    org: &Org,
    v: &serde_json::Value,
    warnings: &mut Vec<String>,
) -> Result<amail_store::ImportTaskRec, String> {
    let Some(obj) = v.as_object() else {
        return Err("not a JSON object".into());
    };
    let id = match obj.get("id") {
        Some(serde_json::Value::String(s)) => import_line("id", s, MAX_TASK_IMPORT_ID_CHARS)?,
        Some(serde_json::Value::Number(n)) => n.to_string(),
        _ => return Err("id is required (a string)".into()),
    };
    for k in obj.keys().filter(|k| !IMPORT_FIELDS.contains(&k.as_str())) {
        warnings.push(format!("{id}: unknown field {k:?} ignored"));
    }
    let mut v = v.clone();
    v["id"] = serde_json::json!(id);
    if let Some(serde_json::Value::String(s)) = v.get("state")
        && s == "ready"
    {
        v["state"] = serde_json::json!("open");
    }
    let r: p::TaskImportRecord = serde_json::from_value(v).map_err(|e| format!("{id}: {e}"))?;
    let at = |what: &str, s: &str| {
        render::parse_ts(s.trim()).ok_or_else(|| format!("{id}: {what} must be an RFC 3339 time"))
    };
    let title = check_title(&r.title).map_err(|e| format!("{id}: {}", e.message))?;
    if let Some(b) = &r.body {
        check_body(b).map_err(|e| format!("{id}: {}", e.message))?;
    }
    let labels = r
        .labels
        .as_deref()
        .map(check_labels)
        .transpose()
        .map_err(|e| format!("{id}: {}", e.message))?;
    let seat = |name: &str| {
        let name = name.trim().to_string();
        async move { st.store.seat_by_name(org.id, &name).await }
    };
    let assignee_id = match r.assignee.as_deref().map(str::trim) {
        None => None,
        Some("") | Some("none") => Some(None),
        Some(n) => match seat(n).await.map_err(|e| e.to_string())? {
            Some(s) if s.kind != p::SeatKind::System => Some(Some(s.id)),
            _ => {
                warnings.push(format!("{id}: no seat {n:?}; the task is left unassigned"));
                Some(None)
            }
        },
    };
    let reporter_id = match r.reporter.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(n) => match seat(n).await.map_err(|e| e.to_string())? {
            Some(s) if s.kind != p::SeatKind::System => Some(s.id),
            _ => {
                warnings.push(format!(
                    "{id}: no seat {n:?}; the importing user is the reporter"
                ));
                None
            }
        },
    };
    let result = match &r.result {
        None => None,
        Some(x) => Some(amail_store::ImportResultRec {
            result: x.value,
            result_ref: x
                .ref_
                .as_deref()
                .map(check_result_ref)
                .transpose()
                .map_err(|e| format!("{id}: {}", e.message))?
                .filter(|s| !s.is_empty()),
            note: x
                .note
                .as_deref()
                .map(|n| check_result_note("result note", n))
                .transpose()
                .map_err(|e| format!("{id}: {}", e.message))?
                .filter(|s| !s.is_empty()),
            by: x
                .by
                .as_deref()
                .map(|b| import_line("result by", b, 200))
                .transpose()
                .map_err(|e| format!("{id}: {e}"))?,
            at: x.at.as_deref().map(|a| at("result at", a)).transpose()?,
        }),
    };
    let mut history = Vec::new();
    let now = amail_store::now_ms();
    for h in &r.history {
        let text = check_result_note("history text", &h.text)
            .map_err(|e| format!("{id}: {}", e.message))?;
        history.push(amail_store::ImportHistoryRec {
            at: h
                .at
                .as_deref()
                .map(|a| at("history at", a))
                .transpose()?
                .unwrap_or(now),
            by: h
                .by
                .as_deref()
                .map(|b| import_line("history by", b, 200))
                .transpose()
                .map_err(|e| format!("{id}: {e}"))?,
            text,
        });
    }
    Ok(amail_store::ImportTaskRec {
        key: id.clone(),
        title,
        body: r.body.clone(),
        state: r.state,
        priority: r.priority,
        assignee_id,
        reporter_id,
        labels,
        due: r
            .due
            .as_deref()
            .map(|d| at("due", d))
            .transpose()?
            .map(Some),
        parent: r.parent.clone(),
        deps: r
            .depends_on
            .iter()
            .map(|d| (d.id.clone(), d.needs_ok))
            .collect(),
        result,
        created_at: r
            .created_at
            .as_deref()
            .map(|a| at("created_at", a))
            .transpose()?,
        updated_at: r
            .updated_at
            .as_deref()
            .map(|a| at("updated_at", a))
            .transpose()?,
        history,
    })
}

/// `POST /v1/tasks/import` (user token): create or update tasks from the
/// records of a JSON Lines file. Nothing is mailed.
pub async fn import(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::TaskImportRequest>,
) -> ApiResult<Json<p::TaskImportResponse>> {
    auth.require_user()?;
    let me = actor(&auth)?.clone();
    if req.records.is_empty() {
        return Err(ApiError::bad_request("no records"));
    }
    if req.records.len() > MAX_TASK_IMPORT_RECORDS {
        return Err(ApiError::unprocessable(
            "too_many_records",
            format!("an import has at most {MAX_TASK_IMPORT_RECORDS} records"),
        ));
    }
    let mut warnings = Vec::new();
    let mut records = Vec::with_capacity(req.records.len());
    let mut seen = std::collections::HashSet::new();
    for (i, v) in req.records.iter().enumerate() {
        let r = import_record(&st, &auth.org, v, &mut warnings)
            .await
            .map_err(|m| {
                ApiError::unprocessable("invalid_import", format!("record {}: {m}", i + 1))
            })?;
        if !seen.insert(r.key.clone()) {
            return Err(ApiError::unprocessable(
                "invalid_import",
                format!("record {}: id {:?} is used twice in the file", i + 1, r.key),
            ));
        }
        records.push(r);
    }
    let res = st
        .store
        .import_tasks(&amail_store::TaskImportRec {
            org_id: auth.org.id,
            actor_id: me.id,
            quota: st.config.limits.org_storage_bytes,
            dry_run: req.dry_run,
            records,
        })
        .await
        .map_err(|e| store_err(&st, e))?;
    warnings.extend(res.warnings);
    if !req.dry_run {
        for (_, num, action) in &res.results {
            if let (Some(n), "created" | "updated") = (num, *action) {
                publish(
                    &st,
                    auth.org.id,
                    *n,
                    if *action == "created" {
                        "created"
                    } else {
                        "changed"
                    },
                    &[],
                );
            }
        }
    }
    Ok(Json(p::TaskImportResponse {
        results: res
            .results
            .into_iter()
            .map(|(id, num, action)| p::TaskImportOutcome {
                id,
                num,
                action: action.to_string(),
            })
            .collect(),
        warnings,
        dry_run: req.dry_run,
    }))
}

/// Tasks of the viewer's org linked to a thread (or whose discussion
/// thread it is), for thread answers.
pub async fn thread_refs(st: &AppState, org_id: i64, thread_id: i64) -> Vec<p::TaskRef> {
    match st.store.thread_tasks(org_id, thread_id).await {
        Ok(v) => v.iter().map(task_ref).collect(),
        Err(e) => {
            tracing::warn!("tasks of thread {thread_id}: {e}");
            Vec::new()
        }
    }
}

// --- mail into tasks ---------------------------------------------------------------

/// Task numbers named as `[t12]` in a subject (at most 10).
pub fn subject_tags(subject: &str) -> Vec<i64> {
    subject_tags_with(subject, 't')
}

/// Numbers named as `[<prefix>12]` in a subject (at most 10): tasks `t`,
/// documents `d`.
pub fn subject_tags_with(subject: &str, prefix: char) -> Vec<i64> {
    let open = format!("[{prefix}");
    let mut out = Vec::new();
    let mut rest = subject;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty()
            && rest[digits.len()..].starts_with(']')
            && let Ok(n) = digits.parse::<i64>()
            && n > 0
            && !out.contains(&n)
        {
            out.push(n);
            if out.len() == 10 {
                break;
            }
        }
    }
    out
}

/// After a message is stored: a reply in a task's discussion thread is a
/// comment of the task; a `[t12]` subject links the thread to task 12 of
/// the sender's org. Best effort: errors are logged.
pub async fn after_send(
    st: &AppState,
    org: &Org,
    sender: &Seat,
    written_by: Option<&str>,
    msg: &NewMessage,
    sent: &amail_store::Sent,
) {
    let w = |num: i64| TaskWrite {
        org_id: org.id,
        num,
        actor_id: Some(sender.id),
        written_by: written_by.map(str::to_string),
        quota: None,
        if_rev: None,
    };
    if msg.thread_id.is_some() {
        match st.store.thread_tasks(org.id, sent.thread_id).await {
            Ok(tasks) => {
                for t in tasks.iter().filter(|t| t.thread_id == Some(sent.thread_id)) {
                    match st
                        .store
                        .record_task_comment(&w(t.num), sent.message_id, sent.thread_id)
                        .await
                    {
                        Ok(_) => publish(st, org.id, t.num, "commented", &[]),
                        Err(e) => tracing::warn!("recording a comment of t{}: {e}", t.num),
                    }
                }
            }
            Err(e) => tracing::warn!("tasks of thread {}: {e}", sent.thread_id),
        }
    }
    for num in subject_tags(&msg.subject) {
        let note = format!("[t{num}] in the subject of message {}", sent.message_id);
        match st
            .store
            .add_task_link(&w(num), sent.thread_id, true, Some(&note))
            .await
        {
            Ok(true) => publish(st, org.id, num, "link", &[]),
            Ok(false) => {}
            Err(e) => tracing::warn!("linking thread {} to t{num}: {e}", sent.thread_id),
        }
    }
}

// --- reminders -----------------------------------------------------------------------

/// What a reminder is about.
#[derive(Debug, Clone)]
pub enum About {
    Task(TaskRec),
    Doc(amail_store::DocRec),
}

impl About {
    /// `t` or `d` and the record's id.
    fn key(&self) -> (char, i64) {
        match self {
            About::Task(t) => ('t', t.id),
            About::Doc(d) => ('d', d.id),
        }
    }
}

/// One reminder for one seat about one task or document.
#[derive(Debug, Clone)]
pub struct Reminder {
    pub(crate) seat_id: i64,
    pub(crate) seat: String,
    /// Tasks: `assigned`, `unblocked`, `waiting`, `closed`, `urgent`,
    /// `stale`, `due`, `steal`, `steal_declined`, `steal_expired`,
    /// `children_done`, `blocker_rejected`. Documents: `revised`,
    /// `supplemented`, `superseded`, `expired`, `dangling`.
    pub(crate) kind: &'static str,
    pub(crate) about: About,
    pub(crate) line: String,
    /// A copy (the team's lead, for stale tasks).
    pub(crate) cc: Option<(i64, String)>,
    /// More text for the body, after the lines.
    pub(crate) extra: Option<String>,
}

impl Reminder {
    fn new(seat_id: i64, seat: String, kind: &'static str, task: &TaskRec, line: String) -> Self {
        Reminder {
            seat_id,
            seat,
            kind,
            about: About::Task(task.clone()),
            line,
            cc: None,
            extra: None,
        }
    }

    /// A reminder about a document.
    pub fn doc(
        seat_id: i64,
        seat: String,
        kind: &'static str,
        doc: &amail_store::DocRec,
        line: String,
    ) -> Self {
        Reminder {
            seat_id,
            seat,
            kind,
            about: About::Doc(doc.clone()),
            line,
            cc: None,
            extra: None,
        }
    }
}

/// The reminders of one change.
fn change_reminders(ch: &TaskChanged) -> Vec<Reminder> {
    let (b, t) = (&ch.before, &ch.task);
    let mut out = Vec::new();
    if t.assignee_id != b.assignee_id
        && let (Some(id), Some(name)) = (t.assignee_id, &t.assignee)
    {
        out.push(Reminder::new(
            id,
            name.clone(),
            "assigned",
            t,
            format!("t{} assigned to you: {}", t.num, t.title),
        ));
    }
    if t.priority == TaskPriority::Urgent
        && b.priority != TaskPriority::Urgent
        && let (Some(id), Some(name)) = (t.assignee_id, &t.assignee)
        && !t.state.is_closed()
    {
        out.push(Reminder::new(
            id,
            name.clone(),
            "urgent",
            t,
            format!("t{} is now urgent: {}", t.num, t.title),
        ));
    }
    if t.state == TaskState::Blocked
        && (b.state != TaskState::Blocked || b.waiting_on_id != t.waiting_on_id)
        && let (Some(id), Some(name)) = (t.waiting_on_id, &t.waiting_on)
    {
        let why = t.blocked_reason.clone().unwrap_or_else(|| t.title.clone());
        out.push(Reminder::new(
            id,
            name.clone(),
            "waiting",
            t,
            format!("t{} waits for you: {why}", t.num),
        ));
    }
    out.extend(steal_ended(ch));
    let closed_now = t.state.is_closed() && !b.state.is_closed();
    out.extend(unblocked_reminders(ch, |d| match t.result {
        Some(r) if !closed_now => format!(
            "t{} is no longer blocked: t{} done with result {r}",
            d.num, t.num
        ),
        _ => format!("t{} is no longer blocked: t{} {}", d.num, t.num, t.state),
    }));
    for d in &ch.needs_ok_waiting {
        let (id, name) = d.responsible();
        let line = if t.result == Some(p::TaskResult::Rejected) {
            format!(
                "t{} is done with result rejected; t{} waits for an ok result",
                t.num, d.num
            )
        } else {
            format!(
                "t{}'s ok result was withdrawn; t{} waits for an ok result",
                t.num, d.num
            )
        };
        out.push(Reminder::new(
            id,
            name.to_string(),
            "blocker_rejected",
            d,
            line,
        ));
    }
    if let Some((parent, n)) = &ch.children_done {
        let (id, name) = parent.responsible();
        out.push(Reminder::new(
            id,
            name.to_string(),
            "children_done",
            parent,
            format!("t{}: all {n} child tasks are closed", parent.num),
        ));
    }
    if closed_now {
        // The reporter is told; assignees of tasks that still wait on other
        // blockers are not (they could not start anyway, and a reminder in
        // `To` wakes them).
        out.push(Reminder::new(
            t.reporter_id,
            t.reporter.clone(),
            "closed",
            t,
            format!("t{} {}: {}", t.num, t.state, t.title),
        ));
    }
    out
}

/// `unblocked` reminders for the tasks a write made startable, to their
/// assignees (else reporters).
fn unblocked_reminders(ch: &TaskChanged, line: impl Fn(&TaskRec) -> String) -> Vec<Reminder> {
    ch.unblocked
        .iter()
        .map(|d| {
            let (id, name) = d.responsible();
            Reminder::new(id, name.to_string(), "unblocked", d, line(d))
        })
        .collect()
}

/// Publish a change and send its reminders.
async fn after_change(
    st: &AppState,
    org: &Org,
    actor: Option<i64>,
    ch: &TaskChanged,
    change: &str,
) {
    if ch.fields.is_empty() {
        return;
    }
    publish(st, org.id, ch.task.num, change, &ch.fields);
    if ch.fields.iter().any(|f| f == "title") {
        crate::docs::link_task_title(st, org.id, actor, &ch.task).await;
    }
    crate::handover::task_changed(st, org, actor, ch).await;
    for d in &ch.unblocked {
        publish(st, org.id, d.num, "changed", &["state".to_string()]);
    }
    remind(st, org, actor, change_reminders(ch)).await;
}

/// The body of a reminder mail: its lines, then each task and document it
/// is about.
fn reminder_body(org: &Org, rs: &[&Reminder]) -> String {
    let mut body = String::new();
    for r in rs {
        body.push_str(&r.line);
        body.push('\n');
    }
    for r in rs {
        if let Some(x) = &r.extra {
            body.push_str(x);
        }
    }
    let mut seen = Vec::new();
    for r in rs {
        if seen.contains(&r.about.key()) {
            continue;
        }
        seen.push(r.about.key());
        let t = match &r.about {
            About::Task(t) => t,
            About::Doc(d) => {
                body.push_str(&crate::docs::reminder_block(d));
                continue;
            }
        };
        body.push_str(&format!(
            "\nt{}: {}\nState: {}  Priority: {}  Assignee: {}",
            t.num,
            t.title,
            t.shown_state(),
            t.priority,
            t.assignee.as_deref().unwrap_or("(none)")
        ));
        if !t.waits_on.is_empty() {
            let w: Vec<String> = t
                .waits_on
                .iter()
                .map(|n| {
                    if t.waits_on_ok.contains(n) {
                        format!("t{n} (ok needed)")
                    } else {
                        format!("t{n}")
                    }
                })
                .collect();
            body.push_str(&format!("  Waits on: {}", w.join(", ")));
        }
        if let Some(d) = t.due {
            body.push_str(&format!("  Due: {}", render::ts(d)));
        }
        if let Some(r) = t.result {
            body.push_str(&format!("  Result: {r}"));
        }
        body.push_str(&format!("\nShow it: amail task show t{}\n", t.num));
    }
    body.push_str(&format!(
        "\nThis is a reminder about the tasks and documents of org {}; the mail system sent it.\n",
        org.name
    ));
    body
}

/// Send reminders: never to `actor`, one mail per recipient listing all
/// of its reminders. Best effort: errors are logged.
pub async fn remind(st: &AppState, org: &Org, actor: Option<i64>, list: Vec<Reminder>) {
    let mut by_seat: Vec<(i64, Vec<Reminder>)> = Vec::new();
    for r in list {
        if Some(r.seat_id) == actor {
            continue;
        }
        match by_seat.iter_mut().find(|(s, _)| *s == r.seat_id) {
            Some((_, v)) => {
                if !v
                    .iter()
                    .any(|x| x.kind == r.kind && x.about.key() == r.about.key())
                {
                    v.push(r)
                }
            }
            None => by_seat.push((r.seat_id, vec![r])),
        }
    }
    if by_seat.is_empty() {
        return;
    }
    // Recipients must be live seats of the org (a seat may have been
    // deleted since it was named on the task).
    let mut names: Vec<String> = by_seat.iter().map(|(_, v)| v[0].seat.clone()).collect();
    names.extend(
        by_seat
            .iter()
            .flat_map(|(_, v)| v.iter().filter_map(|r| r.cc.as_ref().map(|c| c.1.clone()))),
    );
    let live: Vec<Seat> = match st.store.seats_by_names(org.id, &names).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("org {}: reminders not sent: {e}", org.id);
            return;
        }
    };
    let alive = |id: i64| {
        live.iter()
            .any(|s| s.id == id && s.kind != p::SeatKind::System)
    };
    let system = match st.store.system_seat(org.id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("org {}: reminders not sent: {e}", org.id);
            return;
        }
    };
    for (seat_id, rs) in by_seat {
        if !alive(seat_id) {
            continue;
        }
        let refs: Vec<&Reminder> = rs.iter().collect();
        let subject: String = rs[0].line.chars().take(MAX_SUBJECT_CHARS).collect();
        let subject = amail_proto::text::line(&subject).into_owned();
        let body = reminder_body(org, &refs);
        let seat = rs[0].seat.clone();
        let mut deliveries = vec![NewDelivery {
            seat_id,
            org_id: org.id,
            class: p::DeliveryClass::To,
            mode: p::DeliveryMode::Immediate,
        }];
        let mut cc_addrs = Vec::new();
        for (cid, cname) in rs.iter().filter_map(|r| r.cc.clone()) {
            if cid != seat_id
                && Some(cid) != actor
                && alive(cid)
                && !deliveries.iter().any(|d| d.seat_id == cid)
            {
                deliveries.push(NewDelivery {
                    seat_id: cid,
                    org_id: org.id,
                    class: p::DeliveryClass::Cc,
                    mode: p::DeliveryMode::Immediate,
                });
                cc_addrs.push(format!("{cname}@{}", org.name));
            }
        }
        let sent = st
            .store
            .send(&NewMessage {
                org_id: org.id,
                sender_seat_id: system.id,
                thread_id: None,
                parent_id: None,
                depth: 0,
                subject,
                body,
                to_addrs: vec![format!("{seat}@{}", org.name)],
                cc_addrs,
                priority: p::Priority::Normal,
                ack_required: false,
                meta: Default::default(),
                deliveries,
                ack_due: None,
                written_by: None,
                // Reminders from the mail system are never refused (as the
                // acknowledgement reminders): one per recipient per change or
                // per pass.
                storage_quota: None,
                notice: false,
                guards: Vec::new(),
            })
            .await;
        let sent = match sent {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("org {}: task reminder to {seat} not sent: {e}", org.id);
                continue;
            }
        };
        let events: Vec<(String, i64)> = sent
            .seqs
            .iter()
            .filter_map(|b| {
                live.iter()
                    .find(|s| s.id == b.seat_id)
                    .map(|s| (s.name.clone(), b.seq))
            })
            .collect();
        st.bus.publish(org.id, &events);
        let mut done: Vec<((char, i64), &str)> = Vec::new();
        for r in &rs {
            if done.contains(&(r.about.key(), r.kind)) {
                continue;
            }
            done.push((r.about.key(), r.kind));
            match &r.about {
                About::Task(t) => {
                    if let Err(e) = st
                        .store
                        .record_task_reminder(org.id, t.id, r.kind, &seat, sent.message_id)
                        .await
                    {
                        tracing::warn!("recording a reminder of t{}: {e}", t.num);
                    }
                    publish(st, org.id, t.num, "reminder", &[]);
                }
                About::Doc(d) => {
                    if let Err(e) = st
                        .store
                        .record_doc_reminder(org.id, d.id, r.kind, &seat, sent.message_id)
                        .await
                    {
                        tracing::warn!("recording a reminder of d{}: {e}", d.num);
                    }
                    crate::docs::publish(st, org.id, d.num, "reminder", &[]);
                }
            }
        }
    }
}

/// The background pass: due times that passed (each task once), the stale
/// pass (at most once an hour per org; each task once per stale period,
/// with a copy to its team's lead, the team's root seat), expired steal
/// requests, and the document reminders (expiry once, dangling links once
/// a day per document). Each seat gets one mail per pass listing
/// everything due for it.
pub async fn run(st: AppState) {
    loop {
        tokio::time::sleep(st.attention.task_scan_every).await;
        if let Err(e) = scan(&st).await {
            tracing::warn!("task reminders: {e}");
        }
    }
}

pub async fn scan(st: &AppState) -> amail_store::Result<()> {
    let now = amail_store::now_ms();
    let due = st.store.claim_due_tasks(now, 500).await?;
    let stale = st.store.claim_stale_tasks(now, 500).await?;
    let expired = st
        .store
        .expire_task_steals(now - STEAL_REQUEST_HOURS * 3_600_000, 500)
        .await?;
    let mut by_org: HashMap<i64, Vec<Reminder>> = HashMap::new();
    for x in &expired {
        let t = &x.task;
        publish(st, t.org_id, t.num, "steal", &["steal".to_string()]);
        by_org.entry(t.org_id).or_default().push(Reminder::new(
            x.requester_id,
            x.requester.clone(),
            "steal_expired",
            t,
            format!(
                "t{}: your request to take it over expired ({} did not answer in {STEAL_REQUEST_HOURS} hours)",
                t.num,
                t.assignee.as_deref().unwrap_or("the assignee")
            ),
        ));
    }
    for t in &due {
        let (id, name) = t.responsible();
        by_org.entry(t.org_id).or_default().push(Reminder::new(
            id,
            name.to_string(),
            "due",
            t,
            format!(
                "t{} is past its due time ({}): {}",
                t.num,
                render::ts(t.due.unwrap_or_default()),
                t.title
            ),
        ));
    }
    let mut leads: HashMap<i64, HashMap<i64, (i64, String)>> = HashMap::new();
    for t in &stale {
        if let std::collections::hash_map::Entry::Vacant(e) = leads.entry(t.org_id) {
            let m = st
                .store
                .teams(t.org_id)
                .await?
                .into_iter()
                .filter_map(|tm| Some((tm.id, (tm.root_seat_id?, tm.root?))))
                .collect();
            e.insert(m);
        }
        let (id, name) = t.responsible();
        let mut r = Reminder::new(
            id,
            name.to_string(),
            "stale",
            t,
            format!("t{} has not changed for a while: {}", t.num, t.title),
        );
        r.cc = t
            .team_id
            .and_then(|tm| leads.get(&t.org_id).and_then(|m| m.get(&tm)).cloned())
            .filter(|(lead, _)| *lead != id);
        by_org.entry(t.org_id).or_default().push(r);
    }
    for (org_id, r) in crate::docs::due_reminders(st, now).await? {
        by_org.entry(org_id).or_default().push(r);
    }
    for (org_id, rs) in by_org {
        let Some(org) = st.store.org_by_id(org_id).await? else {
            continue;
        };
        remind(st, &org, None, rs).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_in_subjects() {
        assert_eq!(subject_tags("[t12] fix it"), vec![12]);
        assert_eq!(subject_tags("Re: [t3] and [t4] [t3]"), vec![3, 4]);
        assert!(subject_tags("t12 [t] [tx1] [t0] [t12").is_empty());
        assert_eq!(parse_num("t7").unwrap(), 7);
        assert!(parse_num("tt7").is_err() && parse_num("0").is_err());
    }
}
