//! Endpoints the web interface needs: overview, mailbox counts, the
//! "needs you" queue, a seat's sent mail, search, and seat commands.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path, Query};
use crate::mail::Pool;
use crate::render;
use amail_proto::limits::{MAX_PAGE_BYTES, MAX_SCAN_MESSAGES, SHORT_TERM_SCAN_MESSAGES};
use amail_proto::{
    self as p, AttentionItem, AttentionKind, NodeCommand, Presence, Priority, SeatKind, SeatState,
    TokenScope,
};
use amail_store::{ArchiveQuery, SearchHit, SearchQuery};
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use std::collections::HashMap;

const MAX_TERMS: usize = 10;
const ATTENTION_LIMIT: i64 = 500;
/// Characters of a message shown in the "needs you" queue.
const PREVIEW_CHARS: usize = 200;

/// Presence of every seat of the org that a node reports, with
/// `node_connected` filled in.
pub async fn presences(st: &AppState, org_id: i64) -> ApiResult<HashMap<String, Presence>> {
    let connected = st.nodes.connected(org_id) > 0;
    Ok(st
        .presence
        .snapshot(org_id)
        .await?
        .into_iter()
        .map(|(n, mut p)| {
            p.node_connected = connected;
            (n, p)
        })
        .collect())
}

/// Seats that need a person, as attention items.
fn seat_items(presence: &HashMap<String, Presence>) -> Vec<AttentionItem> {
    presence
        .iter()
        .filter(|(_, p)| p.attention.is_some() || p.wake_failed || p.turn_failed.is_some())
        .map(|(name, p)| AttentionItem {
            kind: AttentionKind::Seat,
            since: p.since.clone(),
            seat: Some(name.clone()),
            message: None,
            delivery: None,
            state: Some(p.state),
            attention: p
                .attention
                .clone()
                .or_else(|| p.turn_failed.as_ref().map(|_| "turn-failed".to_string()))
                .or_else(|| p.wake_failed.then(|| "wake-failed".to_string())),
            hint: p
                .attention_hint
                .clone()
                .or_else(|| p.turn_failed.as_ref().map(|t| t.reason.clone())),
            wake_failed: p.wake_failed,
            turn_failed: p.turn_failed.clone(),
            preview: None,
        })
        .collect()
}

async fn attention_items(st: &AppState, auth: &Auth) -> ApiResult<Vec<AttentionItem>> {
    let user = auth.mail_seat()?;
    let entries = st.store.attention(user.id, ATTENTION_LIMIT).await?;
    let mut urgent = Vec::new();
    let mut normal = Vec::new();
    for e in &entries {
        let header = render::header(&e.message, &auth.org);
        // Mail from the server itself (settings changed, acknowledgements
        // overdue) asks for no answer: a notice, until it is read.
        let kind = if e.message.ack_required && e.delivery.acked_at.is_none() {
            AttentionKind::Ack
        } else if header.source == p::Source::System {
            AttentionKind::Notice
        } else {
            AttentionKind::Reply
        };
        let item = AttentionItem {
            kind,
            since: header.created_at.clone(),
            seat: Some(header.from.clone()),
            message: Some(header),
            delivery: Some(render::delivery(&e.delivery, &auth.org)),
            state: None,
            attention: None,
            hint: None,
            wake_failed: false,
            turn_failed: None,
            preview: Some(render::lead(&e.message.body, PREVIEW_CHARS)),
        };
        if e.message.priority == Priority::Urgent {
            urgent.push(item);
        } else {
            normal.push(item);
        }
    }
    let mut seats = seat_items(&presences(st, auth.org.id).await?);
    // Timestamps are RFC 3339 in UTC with a fixed format, so they sort as
    // strings.
    seats.sort_by(|a, b| (&a.since, &a.seat).cmp(&(&b.since, &b.seat)));
    let mut items = urgent;
    items.append(&mut seats);
    items.append(&mut normal);
    Ok(items)
}

/// `GET /v1/attention`: what waits for the user (see the README).
pub async fn attention(
    State(st): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<p::AttentionResponse>> {
    auth.require_user()?;
    let (st2, auth2) = (st.clone(), auth.clone());
    let mut items = crate::mail::limited(&st, &auth, Pool::Light, async move {
        attention_items(&st2, &auth2).await
    })
    .await?;
    render::fit(&mut items, MAX_PAGE_BYTES, false);
    Ok(Json(p::AttentionResponse { items }))
}

/// `GET /v1/overview`: seats per state and recent mail volume.
pub async fn overview(State(st): State<AppState>, auth: Auth) -> ApiResult<Json<p::Overview>> {
    auth.require_user()?;
    let (st2, auth2) = (st.clone(), auth.clone());
    let out = crate::mail::limited(&st, &auth, Pool::Light, async move {
        overview_of(&st2, &auth2).await
    })
    .await?;
    Ok(Json(out))
}

/// The overview: the seats and their presence, the counters of recent mail
/// and of the user seat, and the attention items (see
/// [`amail_store::Store::attention`]).
async fn overview_of(st: &AppState, auth: &Auth) -> ApiResult<p::Overview> {
    let seats = st.store.seats(auth.org.id).await?;
    let presence = presences(st, auth.org.id).await?;
    let mut out = p::Overview::default();
    for s in [
        SeatState::Busy,
        SeatState::Idle,
        SeatState::Awaiting,
        SeatState::Dormant,
        SeatState::Offline,
    ] {
        out.states.insert(s.as_str().to_string(), 0);
    }
    for s in seats.iter().filter(|s| s.kind == SeatKind::Agent) {
        out.seats += 1;
        let pr = presence.get(&s.name);
        let state = pr.map_or(SeatState::Offline, |p| p.state);
        *out.states.entry(state.as_str().to_string()).or_default() += 1;
        if pr.is_some_and(|p| p.attention.is_some()) {
            out.needs_person += 1;
        }
        if pr.is_some_and(|p| p.wake_failed) {
            out.wake_failed += 1;
        }
        if pr.is_some_and(|p| p.turn_failed.is_some()) {
            out.turn_failed += 1;
        }
    }
    let now = amail_store::now_ms();
    out.mail_last_hour = st
        .activity
        .count(&*st.store, auth.org.id, now - 3_600_000)
        .await?;
    out.mail_last_day = st
        .activity
        .count(&*st.store, auth.org.id, now - 86_400_000)
        .await?;
    let user = auth.mail_seat()?;
    out.user_unread = st
        .store
        .counts(auth.org.id, Some(user.id))
        .await?
        .first()
        .map_or(0, |c| c.unread);
    out.attention = attention_items(st, auth).await?.len() as i64;
    Ok(out)
}

#[derive(Deserialize)]
pub struct SeatParam {
    seat: Option<String>,
}

/// `GET /v1/counts`: unread and pending deliveries per seat (user tokens).
pub async fn counts(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SeatParam>,
) -> ApiResult<Json<p::CountsResponse>> {
    auth.require_user()?;
    let seat_id = match &q.seat {
        Some(name) => Some(auth.mailbox(&st, Some(name)).await?.id),
        None => None,
    };
    let rows = st.store.counts(auth.org.id, seat_id).await?;
    Ok(Json(p::CountsResponse {
        seats: rows
            .into_iter()
            .map(|r| p::SeatCounts {
                seat: r.seat_name,
                unread: r.unread,
                pending: r.pending,
                seq: r.last_seq,
                focus_until: r
                    .focus_until
                    .filter(|t| *t > amail_store::now_ms())
                    .map(render::ts),
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct SentParams {
    seat: Option<String>,
    before_id: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/sent`: messages a seat sent, newest first. User tokens may name
/// any seat; seat tokens see their own.
pub async fn sent(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SentParams>,
) -> ApiResult<Json<p::ArchiveResponse>> {
    if auth.scope == TokenScope::Node {
        return Err(ApiError::forbidden("node tokens cannot read messages"));
    }
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = ArchiveQuery {
        from: Some((auth.org.name.clone(), seat.name.clone())),
        before_id: q.before_id,
        limit,
        ..Default::default()
    };
    let page = if limit > p::limits::SMALL_PAGE {
        let (store, org_id) = (st.store.clone(), auth.org.id);
        crate::mail::limited(&st, &auth, Pool::Other, async move {
            Ok(store.archive(org_id, &query).await?)
        })
        .await?
    } else {
        st.store.archive(auth.org.id, &query).await?
    };
    Ok(Json(crate::mail::archive_response(page, &auth.org)))
}

#[derive(Deserialize)]
pub struct SearchParams {
    q: String,
    before_id: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/archive/search`: substring search over subjects and bodies of
/// the org's most recent messages (see [`SearchQuery::scan`]).
pub async fn search(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SearchParams>,
) -> ApiResult<Json<p::SearchResponse>> {
    auth.require_user()?;
    let terms: Vec<String> = q.q.split_whitespace().map(str::to_string).collect();
    if terms.is_empty() {
        return Err(ApiError::bad_request("q must contain a search term"));
    }
    if terms.len() > MAX_TERMS || terms.iter().any(|t| t.chars().count() > 200) {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_TERMS} terms of up to 200 characters"
        )));
    }
    let short = terms.iter().any(|t| t.chars().count() < 3);
    let scan = if short {
        SHORT_TERM_SCAN_MESSAGES
    } else {
        MAX_SCAN_MESSAGES
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let query = SearchQuery {
        terms,
        before_id: q.before_id,
        limit,
        scan,
        time_limit: Some(crate::mail::SCAN_TIME_LIMIT),
    };
    let (store, org_id) = (st.store.clone(), auth.org.id);
    let r = crate::mail::limited(&st, &auth, Pool::Scan, async move {
        Ok(store.search(org_id, &query).await?)
    })
    .await?;
    let mut messages: Vec<p::SearchHit> = r
        .hits
        .iter()
        .map(|h| p::SearchHit {
            header: render::header(&h.message, &auth.org),
            snippet: snippet(h),
        })
        .collect();
    let next_before_id = if render::fit(&mut messages, MAX_PAGE_BYTES, false) {
        messages.last().map(|m| m.header.id)
    } else {
        r.next_before_id
    };
    Ok(Json(p::SearchResponse {
        messages,
        next_before_id,
        mode: p::SearchMode::Scan,
        scan_limit: Some(scan),
    }))
}

/// The excerpt of a hit on one line, with `…` where the body goes on.
pub fn snippet(h: &SearchHit) -> String {
    let mut s = h
        .excerpt
        .split(char::is_whitespace)
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if h.cut_before {
        s.insert(0, '…');
    }
    if h.cut_after {
        s.push('…');
    }
    s
}

/// `POST /v1/messages/{id}/read`: mark the caller's delivery read.
pub async fn mark_read(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::AckResponse>> {
    let seat = auth.mail_seat()?.clone();
    let m = st
        .store
        .message(id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no message {id}")))?;
    if !crate::mail::can_read(&st, &auth, &m).await? {
        return Err(ApiError::not_found(format!("no message {id}")));
    }
    let (d, bump) = st.store.mark_read(id, seat.id).await?.ok_or_else(|| {
        ApiError::not_found(format!("message {id} was not delivered to {}", seat.name))
    })?;
    if let Some(b) = bump {
        st.bus.publish(b.org_id, &[(seat.name.clone(), b.seq)]);
    }
    Ok(Json(p::AckResponse {
        delivery: render::delivery(&d, &auth.org),
    }))
}

/// `POST /v1/messages/{id}/unread` (user tokens): mark the user's delivery
/// not read again, as a person does in a mail program. An acknowledged
/// delivery stays read.
pub async fn mark_unread(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::AckResponse>> {
    auth.require_user()?;
    let seat = auth.mail_seat()?.clone();
    let m = st
        .store
        .message(id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no message {id}")))?;
    if !crate::mail::can_read(&st, &auth, &m).await? {
        return Err(ApiError::not_found(format!("no message {id}")));
    }
    let (d, bump) = st.store.mark_unread(id, seat.id).await?.ok_or_else(|| {
        ApiError::not_found(format!("message {id} was not delivered to {}", seat.name))
    })?;
    if let Some(b) = bump {
        st.bus.publish(b.org_id, &[(seat.name.clone(), b.seq)]);
    }
    Ok(Json(p::AckResponse {
        delivery: render::delivery(&d, &auth.org),
    }))
}

async fn seat_op(
    st: &AppState,
    auth: &Auth,
    name: &str,
    op: &str,
) -> ApiResult<(StatusCode, Json<p::SeatOpResponse>)> {
    auth.require_user()?;
    let seat = st
        .store
        .seat_by_name(auth.org.id, name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if seat.kind != SeatKind::Agent {
        return Err(ApiError::unprocessable(
            "not_an_agent",
            "only agent seats have sessions",
        ));
    }
    let cmd = NodeCommand {
        op: op.to_string(),
        seat: seat.name.clone(),
        lease_ms: None,
        backfill: None,
        node: st
            .presence
            .get(auth.org.id, &seat.name)
            .await?
            .and_then(|p| p.node),
    };
    let nodes = st.nodes.send(auth.org.id, cmd);
    if nodes == 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "node_offline",
            "no node of this org is connected",
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(p::SeatOpResponse {
            seat: seat.name,
            op: op.to_string(),
            nodes: nodes as u32,
        }),
    ))
}

/// `POST /v1/seats/{name}/sleep`: ask the hosting node to put the seat to
/// sleep.
pub async fn sleep(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<(StatusCode, Json<p::SeatOpResponse>)> {
    seat_op(&st, &auth, &name, "sleep").await
}

/// `POST /v1/seats/{name}/wake`: ask the hosting node to start the seat.
pub async fn wake(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
) -> ApiResult<(StatusCode, Json<p::SeatOpResponse>)> {
    seat_op(&st, &auth, &name, "wake").await
}
