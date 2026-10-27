//! Seat handover (M9): the handover note (`/v1/seats/{name}/handover…`)
//! and the worklog (`/v1/seats/{name}/worklog`, `POST /v1/worklog`).
//!
//! The lead of a seat is the root seat of the seat's team (the team
//! records' `root`, as the task reminders use it); a seat without a team
//! has no lead.
//!
//! | call | who |
//! |---|---|
//! | `GET /seats/{name}/handover` | every seat of the org, the user, node tokens; a private note only the seat, its lead, the user and node tokens |
//! | `PUT /seats/{name}/handover` | the seat itself, the user; node tokens with `source: compaction` only |
//! | `POST /seats/{name}/handover/request` | the seat's lead, the user |
//! | `POST /seats/{name}/handover/read` | node tokens, the seat itself |
//! | `GET /seats/{name}/worklog` | the seat itself, seats of its team, its lead, the user |
//! | `POST /worklog` | node tokens, for the org's agent seats |
//!
//! The server writes worklog entries from its own events (mail sent, read
//! or acknowledged; task changes; focus on and off; document events, see
//! [`doc_event`]). They are best effort: a failure is logged and the
//! request it belongs to goes on.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path, Query};
use crate::limits::Pool;
use crate::render::{parse_ts, ts};
use amail_proto::limits::MAX_SUBJECT_CHARS;
use amail_proto::{self as p, HandoverSource, SeatKind, TokenScope, WorklogSource};
use amail_store::{
    NewDelivery, NewMessage, NewNoteRec, NewWorklogEntry, NoteRec, Org, Seat, StoreError,
    TaskChanged, TaskRec, WorklogQuery, WorklogRec,
};
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;

// --- who may do what -------------------------------------------------------------

/// A seat of the org by name (not the hidden system seat).
async fn target(st: &AppState, org: &Org, name: &str) -> ApiResult<Seat> {
    match st.store.seat_by_name(org.id, name.trim()).await? {
        Some(s) if s.kind != SeatKind::System => Ok(s),
        _ => Err(ApiError::not_found(format!("no seat {name:?}"))),
    }
}

/// The lead of `seat`: its team's root seat.
async fn lead_of(st: &AppState, seat: &Seat) -> ApiResult<Option<i64>> {
    let Some(team) = seat.team_id else {
        return Ok(None);
    };
    Ok(st
        .store
        .teams(seat.org_id)
        .await?
        .into_iter()
        .find(|t| t.id == team)
        .and_then(|t| t.root_seat_id))
}

/// The caller is the user, the seat itself or its lead (they always read
/// the seat's notes and worklog).
async fn is_close(st: &AppState, auth: &Auth, seat: &Seat) -> ApiResult<bool> {
    match auth.scope {
        TokenScope::User => Ok(true),
        TokenScope::Seat => {
            let me = auth.mail_seat()?;
            Ok(me.id == seat.id || lead_of(st, seat).await? == Some(me.id))
        }
        _ => Ok(false),
    }
}

// --- rendering -------------------------------------------------------------------------

fn note_out(n: &NoteRec) -> p::HandoverNote {
    let text = n.fields.render();
    p::HandoverNote {
        seat: n.seat.clone(),
        rev: n.rev,
        chars: text.chars().count(),
        text,
        fields: n.fields.clone(),
        pointers_filled: n.pointers_filled,
        private: n.private,
        source: n.source,
        draft: n.source == HandoverSource::Compaction,
        session: n.session.clone(),
        written_by: n.written_by.clone(),
        written_at: ts(n.written_at),
        read_by_session: n.read_by_session.clone(),
        read_at: n.read_at.map(ts),
    }
}

fn revision_out(n: &NoteRec) -> p::HandoverRevision {
    p::HandoverRevision {
        rev: n.rev,
        written_at: ts(n.written_at),
        source: n.source,
        private: n.private,
        chars: n.fields.chars(),
        session: n.session.clone(),
        written_by: n.written_by.clone(),
        read_by_session: n.read_by_session.clone(),
        read_at: n.read_at.map(ts),
    }
}

fn entry_out(e: &WorklogRec) -> p::WorklogEntry {
    p::WorklogEntry {
        id: e.id,
        seat: e.seat.clone(),
        session: e.session.clone(),
        at: ts(e.at),
        kind: e.kind.clone(),
        text: e.text.clone(),
        task: e.task,
        doc: e.doc,
        source: e.source,
    }
}

/// Store errors of note writes: refusals are 422 with their code, the
/// storage quota 507.
fn store_err(st: &AppState, e: StoreError) -> ApiError {
    match e {
        StoreError::Refused(code, m) => ApiError::unprocessable(code, m),
        StoreError::StorageQuota(stored) => {
            let mut e = ApiError::storage_quota(stored, st.config.limits.org_storage_bytes);
            e.message = e.message.replace("this message", "this note");
            e
        }
        e => e.into(),
    }
}

fn check_session(s: Option<&str>) -> ApiResult<Option<String>> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s)
            if s.chars().count() > p::MAX_SESSION_ID_CHARS || s.chars().any(char::is_control) =>
        {
            Err(ApiError::unprocessable(
                "invalid_session",
                format!(
                    "a session id is 1 to {} characters without control characters",
                    p::MAX_SESSION_ID_CHARS
                ),
            ))
        }
        Some(s) => Ok(Some(s.to_string())),
    }
}

// --- the note ----------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct NoteParams {
    rev: Option<i64>,
}

async fn note_answer(
    st: &AppState,
    auth: &Auth,
    seat: &Seat,
    rev: Option<i64>,
) -> ApiResult<p::HandoverResponse> {
    let org = &auth.org;
    let state = st.store.handover_seat(org.id, seat.id).await?;
    let current = st.store.handover_note(org.id, seat.id, None).await?;
    let note = match rev {
        Some(r) => Some(
            st.store
                .handover_note(org.id, seat.id, Some(r))
                .await?
                .ok_or_else(|| {
                    ApiError::not_found(format!("{} has no note revision {r}", seat.name))
                })?,
        ),
        None => current.clone(),
    };
    let private = state.private
        || current.as_ref().is_some_and(|n| n.private)
        || note.as_ref().is_some_and(|n| n.private);
    if private && auth.scope != TokenScope::Node && !is_close(st, auth, seat).await? {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "private",
            format!(
                "the handover note of {} is private: only the seat, its lead and the user read it",
                seat.name
            ),
        ));
    }
    let revisions = st
        .store
        .handover_notes(org.id, seat.id, p::HANDOVER_REVISIONS_SHOWN)
        .await?;
    let requested = state
        .requested_at
        .filter(|at| current.as_ref().is_none_or(|n| n.written_at < *at));
    Ok(p::HandoverResponse {
        seat: seat.name.clone(),
        note: note.as_ref().map(note_out),
        current_rev: state.note_rev,
        revisions: revisions.iter().map(revision_out).collect(),
        private,
        requested_at: requested.map(ts),
        requested_by: requested.and(state.requested_by),
    })
}

/// `GET /v1/seats/{name}/handover[?rev=N]`.
pub async fn get(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Query(q): Query<NoteParams>,
) -> ApiResult<Json<p::HandoverResponse>> {
    if auth.scope == TokenScope::Operator {
        return Err(ApiError::forbidden("operator tokens cannot read notes"));
    }
    let seat = target(&st, &auth.org, &name).await?;
    Ok(Json(note_answer(&st, &auth, &seat, q.rev).await?))
}

/// `PUT /v1/seats/{name}/handover`: write the next revision.
pub async fn put(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(req): Json<p::HandoverWrite>,
) -> ApiResult<Json<p::HandoverResponse>> {
    let seat = target(&st, &auth.org, &name).await?;
    let source = req.source.unwrap_or(HandoverSource::Agent);
    let written_by = match auth.scope {
        TokenScope::User => Some(auth.mail_seat()?.name.clone()),
        TokenScope::Seat if auth.mail_seat()?.id == seat.id => None,
        TokenScope::Node if source == HandoverSource::Compaction => None,
        TokenScope::Node => {
            return Err(ApiError::forbidden(
                "node tokens write only compaction drafts (source: compaction)",
            ));
        }
        _ => {
            return Err(ApiError::forbidden(
                "only the seat itself and the user write a seat's handover note",
            ));
        }
    };
    if source == HandoverSource::Server {
        return Err(ApiError::bad_request(
            "source is agent or compaction; the server writes its own notes",
        ));
    }
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats have handover notes",
        ));
    }
    let session = check_session(req.session.as_deref())?;
    let mut fields = match (req.text, req.fields) {
        (Some(t), None) => p::parse_handover(&t).trimmed(),
        (None, Some(f)) => f.trimmed(),
        (Some(_), Some(_)) => return Err(ApiError::bad_request("give text or fields, not both")),
        (None, None) => return Err(ApiError::bad_request("give the note as text or fields")),
    };
    if source == HandoverSource::Compaction {
        if fields.values().iter().all(|v| v.is_empty()) {
            return Err(ApiError::bad_request("the compaction summary is empty"));
        }
        if fields.state.is_empty() {
            fields.state = "draft: from compaction summary".into();
        }
    } else {
        let missing = fields.missing();
        if !missing.is_empty() {
            let mut e = ApiError::unprocessable(
                "note_fields_missing",
                format!(
                    "the note lacks {}; the fields are, in order: {} (amail handover template)",
                    missing.join(", "),
                    p::HANDOVER_FIELDS.join(", ")
                ),
            );
            let mut extra = serde_json::Map::new();
            extra.insert("missing".into(), serde_json::json!(missing));
            e.extra = Some(extra);
            return Err(e);
        }
    }
    let chars = fields.chars();
    if chars > p::MAX_HANDOVER_NOTE_CHARS {
        let mut e = ApiError::unprocessable(
            "note_too_large",
            format!(
                "the note has {chars} characters; the limit is {}: keep State, Summary and Next short",
                p::MAX_HANDOVER_NOTE_CHARS
            ),
        );
        let mut extra = serde_json::Map::new();
        extra.insert("chars".into(), serde_json::json!(chars));
        extra.insert("max".into(), serde_json::json!(p::MAX_HANDOVER_NOTE_CHARS));
        e.extra = Some(extra);
        return Err(e);
    }
    let n = st
        .store
        .write_handover_note(&NewNoteRec {
            org_id: auth.org.id,
            seat_id: seat.id,
            fields,
            private: req.private,
            source,
            session,
            written_by,
            quota: st.config.limits.org_storage_bytes,
        })
        .await
        .map_err(|e| store_err(&st, e))?;
    log(
        &st,
        auth.org.id,
        seat.id,
        "handover",
        format!("handover note rev {} written ({})", n.rev, n.source),
        None,
    )
    .await;
    Ok(Json(note_answer(&st, &auth, &seat, None).await?))
}

/// `POST /v1/seats/{name}/handover/request`: the lead or the user asks
/// the seat for its note; the seat gets system mail in `To`.
pub async fn request(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<Json<p::HandoverRequested>> {
    let seat = target(&st, &auth.org, &name).await?;
    let me = match auth.scope {
        TokenScope::User | TokenScope::Seat => auth.mail_seat()?.clone(),
        _ => return Err(ApiError::forbidden("requires a user or seat token")),
    };
    if !auth.is_user() && lead_of(&st, &seat).await? != Some(me.id) {
        return Err(ApiError::forbidden(format!(
            "only the lead of {} (its team's root seat) and the user ask for its note",
            seat.name
        )));
    }
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats have handover notes",
        ));
    }
    let now = amail_store::now_ms();
    st.store
        .request_handover(auth.org.id, seat.id, me.id, now)
        .await?;
    let body = format!(
        "{by} asks you to write your handover note now.\n\nRun:\n    amail handover write\n\
         with the fields State, Summary, Next, Waiting on, Traps, Verify and Pointers \
         (amail handover template prints them). Pointers may stay empty: the server fills it in \
         from your worklog.\n\nThis mail was sent by the mail system of org {org} for {by}.\n",
        by = me.name,
        org = auth.org.name
    );
    let subject = format!("Handover note requested by {}", me.name);
    let message_id = system_mail(&st, &auth.org, &seat, subject, body).await;
    Ok(Json(p::HandoverRequested {
        seat: seat.name,
        requested_at: ts(now),
        message_id,
    }))
}

/// System mail to `seat` in `To` (it wakes an idle agent). Best effort.
async fn system_mail(
    st: &AppState,
    org: &Org,
    seat: &Seat,
    subject: String,
    body: String,
) -> Option<i64> {
    let system = match st.store.system_seat(org.id).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("org {}: handover mail not sent: {e}", org.id);
            return None;
        }
    };
    let subject = amail_proto::text::line(&p::cut_chars(&subject, MAX_SUBJECT_CHARS)).into_owned();
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
            to_addrs: vec![format!("{}@{}", seat.name, org.name)],
            cc_addrs: Vec::new(),
            priority: p::Priority::Normal,
            ack_required: false,
            meta: Default::default(),
            deliveries: vec![NewDelivery {
                seat_id: seat.id,
                org_id: org.id,
                class: p::DeliveryClass::To,
                mode: p::DeliveryMode::Immediate,
            }],
            ack_due: None,
            written_by: None,
            storage_quota: None,
            notice: false,
            guards: Vec::new(),
        })
        .await;
    match sent {
        Ok(s) => {
            let events: Vec<(String, i64)> =
                s.seqs.iter().map(|b| (seat.name.clone(), b.seq)).collect();
            st.bus.publish(org.id, &events);
            Some(s.message_id)
        }
        Err(e) => {
            tracing::warn!(
                "org {}: handover mail to {} not sent: {e}",
                org.id,
                seat.name
            );
            None
        }
    }
}

/// `POST /v1/seats/{name}/handover/read`: the node showed the note to a
/// session (the acknowledgement `show` prints).
pub async fn read(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(req): Json<p::HandoverRead>,
) -> ApiResult<Json<p::HandoverReadResponse>> {
    let seat = target(&st, &auth.org, &name).await?;
    match auth.scope {
        TokenScope::Node => {}
        TokenScope::Seat if auth.mail_seat()?.id == seat.id => {}
        _ => {
            return Err(ApiError::forbidden(
                "requires a node token or the seat's own token",
            ));
        }
    }
    let session = check_session(Some(&req.session))?
        .ok_or_else(|| ApiError::bad_request("session is required"))?;
    let n = st
        .store
        .record_handover_read(auth.org.id, seat.id, req.rev, &session)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("{} has no such note", seat.name)))?;
    Ok(Json(p::HandoverReadResponse {
        seat: seat.name,
        rev: n.rev,
        read_by_session: session,
        read_at: ts(n.read_at.unwrap_or_default()),
    }))
}

// --- the worklog -------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct WorklogParams {
    since: Option<String>,
    task: Option<String>,
    kind: Option<String>,
    limit: Option<i64>,
    before: Option<i64>,
    summary: Option<String>,
}

/// `2h`, `30m`, `45s`, `7d` before now, or an RFC 3339 time.
fn parse_since(s: &str, now: i64) -> ApiResult<i64> {
    let s = s.trim();
    if let Some(t) = parse_ts(s) {
        return Ok(t);
    }
    let unit = match s.chars().last() {
        Some('s') => 1_000,
        Some('m') => 60_000,
        Some('h') => 3_600_000,
        Some('d') => 86_400_000,
        _ => 0,
    };
    s[..s.len().saturating_sub(1)]
        .parse::<i64>()
        .ok()
        .filter(|n| unit > 0 && *n >= 0)
        .map(|n| now.saturating_sub(n.saturating_mul(unit)))
        .ok_or_else(|| {
            ApiError::bad_request("since is a duration (30m, 2h, 7d) or an RFC 3339 time")
        })
}

/// `GET /v1/seats/{name}/worklog`: entries newest first, or with
/// `summary=1` counts per kind, the tasks touched and the last ten.
pub async fn worklog(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Query(q): Query<WorklogParams>,
) -> ApiResult<axum::response::Response> {
    use axum::response::IntoResponse;
    let seat = target(&st, &auth.org, &name).await?;
    let allowed = is_close(&st, &auth, &seat).await?
        || (auth.scope == TokenScope::Seat
            && seat.team_id.is_some()
            && auth.mail_seat()?.team_id == seat.team_id);
    if !allowed {
        return Err(ApiError::forbidden(format!(
            "the worklog of {} is read by the seat, its team, its lead and the user",
            seat.name
        )));
    }
    let now = amail_store::now_ms();
    let task = match q.task.as_deref() {
        Some(t) => Some(
            t.trim()
                .trim_start_matches('t')
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| ApiError::bad_request(format!("{t:?} is not a task number")))?,
        ),
        None => None,
    };
    let kind = q
        .kind
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    if kind.as_deref().is_some_and(|k| !p::valid_worklog_kind(k)) {
        return Err(ApiError::bad_request("kind is a-z, 0-9, _, - and ."));
    }
    let query = WorklogQuery {
        org_id: auth.org.id,
        seat_id: seat.id,
        since: q
            .since
            .as_deref()
            .map(|s| parse_since(s, now))
            .transpose()?,
        task,
        kind,
        before: q.before,
        limit: q.limit.unwrap_or(100).clamp(1, p::MAX_WORKLOG_PAGE),
    };
    let summary = matches!(q.summary.as_deref(), Some("1" | "true" | "yes"));
    let store = st.store.clone();
    let name = seat.name.clone();
    crate::mail::limited(&st, &auth, Pool::Other, async move {
        if summary {
            let s = store.worklog_summary(&query).await?;
            return Ok(Json(p::WorklogSummary {
                seat: name,
                total: s.total,
                kinds: s
                    .kinds
                    .into_iter()
                    .map(|(kind, count)| p::WorklogKindCount { kind, count })
                    .collect(),
                tasks: s
                    .tasks
                    .into_iter()
                    .map(|(task, count, at)| p::WorklogTaskCount {
                        task,
                        count,
                        last_at: ts(at),
                    })
                    .collect(),
                last: s.last.iter().map(entry_out).collect(),
            })
            .into_response());
        }
        let mut entries = store
            .worklog(&WorklogQuery {
                limit: query.limit + 1,
                ..query.clone()
            })
            .await?;
        let more = entries.len() as i64 > query.limit;
        entries.truncate(query.limit as usize);
        Ok(Json(p::WorklogPage {
            seat: name,
            next_before: if more {
                entries.last().map(|e| e.id)
            } else {
                None
            },
            entries: entries.iter().map(entry_out).collect(),
        })
        .into_response())
    })
    .await
}

/// `POST /v1/worklog` (node tokens): entries for agent seats of the org.
/// Entries of other seats, and entries whose kind or time cannot be read,
/// are left out and counted in `skipped`.
pub async fn post(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::WorklogPost>,
) -> ApiResult<Json<p::WorklogPosted>> {
    if auth.scope != TokenScope::Node {
        return Err(ApiError::forbidden("requires a node token"));
    }
    if let Some(max) = st.config.limits.worklog_batch_entries
        && req.entries.len() > max
    {
        return Err(ApiError::unprocessable(
            "too_many_entries",
            format!("at most {max} worklog entries per request"),
        ));
    }
    let seats: std::collections::HashMap<String, Seat> = st
        .store
        .seats(auth.org.id)
        .await?
        .into_iter()
        .filter(|s| s.kind == SeatKind::Agent)
        .map(|s| (s.name.clone(), s))
        .collect();
    let now = amail_store::now_ms();
    let mut skipped = 0;
    let mut entries = Vec::with_capacity(req.entries.len());
    for e in req.entries {
        let kind = e.kind.trim();
        let at = match e.at.as_deref() {
            None => Some(now),
            Some(t) => parse_ts(t.trim()).map(|t| t.min(now)),
        };
        let (Some(seat), true, Some(at)) =
            (seats.get(e.seat.trim()), p::valid_worklog_kind(kind), at)
        else {
            skipped += 1;
            continue;
        };
        entries.push(NewWorklogEntry {
            seat_id: seat.id,
            session: e
                .session
                .as_deref()
                .map(|s| p::cut_chars(s.trim(), p::MAX_SESSION_ID_CHARS))
                .filter(|s| !s.is_empty()),
            at,
            kind: kind.to_string(),
            text: p::cut_chars(&e.text, p::MAX_WORKLOG_TEXT_CHARS),
            task: e.task.filter(|n| *n > 0),
            doc: e.doc.filter(|n| *n > 0),
            source: WorklogSource::Node,
        });
    }
    let accepted = st
        .store
        .append_worklog(auth.org.id, &entries, st.config.limits.org_storage_bytes)
        .await
        .map_err(|e| store_err(&st, e))?;
    Ok(Json(p::WorklogPosted {
        accepted,
        skipped: skipped + entries.len() - accepted,
    }))
}

// --- entries from the server's own events ----------------------------------------------

/// Append one server entry for `seat_id` (agent seats only; the store
/// leaves others out). Best effort.
pub async fn log(
    st: &AppState,
    org_id: i64,
    seat_id: i64,
    kind: &str,
    text: String,
    task: Option<i64>,
) {
    log_doc(st, org_id, seat_id, kind, text, task, None).await
}

async fn log_doc(
    st: &AppState,
    org_id: i64,
    seat_id: i64,
    kind: &str,
    text: String,
    task: Option<i64>,
    doc: Option<i64>,
) {
    let e = NewWorklogEntry {
        seat_id,
        session: None,
        at: amail_store::now_ms(),
        kind: kind.to_string(),
        text,
        task,
        doc,
        source: WorklogSource::Server,
    };
    if let Err(e) = st
        .store
        .append_worklog(org_id, &[e], st.config.limits.org_storage_bytes)
        .await
    {
        tracing::warn!("org {org_id}: worklog entry for seat {seat_id} not written: {e}");
    }
}

/// A message the seat sent (`mail_sent`).
pub async fn mail_sent(
    st: &AppState,
    org: &Org,
    sender: &Seat,
    msg: &NewMessage,
    sent: &amail_store::Sent,
) {
    if sender.kind != SeatKind::Agent {
        return;
    }
    let mut to = msg.to_addrs.clone();
    to.extend(msg.cc_addrs.iter().cloned());
    let suffix = format!("@{}", org.name);
    let to: Vec<&str> = to
        .iter()
        .map(|a| a.strip_suffix(&suffix).unwrap_or(a))
        .collect();
    let text = format!(
        "m{} {:?} to {}",
        sent.message_id,
        msg.subject,
        to.join(", ")
    );
    let task = crate::tasks::subject_tags(&msg.subject).first().copied();
    log(st, org.id, sender.id, "mail_sent", text, task).await;
}

/// A message the seat read (`mail_read`) or acknowledged (`mail_acked`).
pub async fn mail_read(st: &AppState, seat: &Seat, m: &amail_store::MessageRec, kind: &str) {
    if seat.kind != SeatKind::Agent {
        return;
    }
    let text = format!("m{} {:?} from {}", m.id, m.subject, m.sender_name);
    let task = crate::tasks::subject_tags(&m.subject).first().copied();
    log(st, seat.org_id, seat.id, kind, text, task).await;
}

/// A task the actor created (`task`).
pub async fn task_created(st: &AppState, org: &Org, actor: &Seat, t: &TaskRec) {
    if actor.kind != SeatKind::Agent {
        return;
    }
    let text = format!("t{}: created ({}): {}", t.num, t.state, t.title);
    log(st, org.id, actor.id, "task", text, Some(t.num)).await;
}

/// A change of a task: the actor's entry (`task`, `t12: open → doing`),
/// and when the change closed the task, the server's idle note for its
/// assignee when the assignee holds no other unfinished task.
pub async fn task_changed(st: &AppState, org: &Org, actor: Option<i64>, ch: &TaskChanged) {
    let (b, t) = (&ch.before, &ch.task);
    if let Some(a) = actor {
        let text = if b.state != t.state {
            format!("t{}: {} → {}", t.num, b.state, t.state)
        } else {
            format!("t{}: changed {}", t.num, ch.fields.join(", "))
        };
        log(st, org.id, a, "task", text, Some(t.num)).await;
    }
    let closed = !b.state.is_closed() && t.state.is_closed();
    if let (true, Some(seat)) = (closed, b.assignee_id.or(t.assignee_id)) {
        match st
            .store
            .write_idle_note(org.id, seat, t.num, st.config.limits.org_storage_bytes)
            .await
        {
            Ok(Some(n)) => {
                tracing::debug!("org {}: idle note rev {} for {}", org.id, n.rev, n.seat)
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("org {}: idle note for seat {seat} not written: {e}", org.id),
        }
    }
}

/// Focus on (`until`) or off (`focus`).
pub async fn focus(st: &AppState, seat: &Seat, until: Option<i64>) {
    let text = match until {
        Some(u) => format!("focus on until {}", ts(u)),
        None => "focus off".to_string(),
    };
    log(st, seat.org_id, seat.id, "focus", text, None).await;
}

/// The hook for document events (the documents feature calls it): an
/// entry of kind `doc` for the seat that changed or read document `doc`.
pub async fn doc_event(st: &AppState, org_id: i64, seat_id: i64, doc: i64, text: String) {
    log_doc(st, org_id, seat_id, "doc", text, None, Some(doc)).await
}

// --- retention ---------------------------------------------------------------------------

/// Delete worklog entries older than `limits.worklog_days` (public
/// instances; nothing runs without it), every ten minutes.
pub async fn retention(st: AppState) {
    let Some(days) = st.config.limits.worklog_days else {
        return;
    };
    loop {
        if let Err(e) = expire(&st, days).await {
            tracing::warn!("worklog retention: {e}");
        }
        tokio::time::sleep(st.config.retention_every).await;
    }
}

pub async fn expire(st: &AppState, days: u32) -> amail_store::Result<u64> {
    let cutoff = amail_store::now_ms() - i64::from(days) * 86_400_000;
    let mut total = 0;
    loop {
        let n = st.store.expire_worklog(cutoff, 5_000).await?;
        total += n;
        if n < 5_000 {
            return Ok(total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since() {
        let now = 10 * 86_400_000;
        assert_eq!(parse_since("2h", now).unwrap(), now - 7_200_000);
        assert_eq!(parse_since("7d", now).unwrap(), now - 7 * 86_400_000);
        assert_eq!(parse_since("1970-01-01T00:00:01Z", now).unwrap(), 1_000);
        assert!(parse_since("2x", now).is_err());
        assert!(parse_since("h", now).is_err());
        assert!(parse_since("", now).is_err());
    }
}
