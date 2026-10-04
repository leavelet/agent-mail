//! Mail endpoints: send, inbox, read, ack, status, threads, claim, changes,
//! events, archive.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path, Query};
use crate::render;
use amail_proto::{self as p, Address, Priority, TokenScope};
use amail_store::{ArchiveQuery, ClaimFilter, InboxQuery, MessageRec, SeqBump};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt, stream};
use serde::Deserialize;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;

/// Whether the caller may read a message: user tokens see everything sent
/// by or delivered to their org; seat tokens see messages their org can see
/// in threads the seat takes part in.
pub async fn can_read(st: &AppState, auth: &Auth, m: &MessageRec) -> ApiResult<bool> {
    match auth.scope {
        TokenScope::Node | TokenScope::Operator => Ok(false),
        TokenScope::User => Ok(st.store.org_sees_message(auth.org.id, m.id).await?),
        TokenScope::Seat => {
            let seat = auth.mail_seat()?;
            if m.sender_seat_id == seat.id {
                return Ok(true);
            }
            Ok(st.store.org_sees_message(auth.org.id, m.id).await?
                && st.store.seat_in_thread(seat.id, m.thread_id).await?)
        }
    }
}

async fn readable_message(st: &AppState, auth: &Auth, id: i64) -> ApiResult<MessageRec> {
    auth.mail_seat()?;
    match st.store.message(id).await? {
        Some(m) if can_read(st, auth, &m).await? => Ok(m),
        _ => Err(ApiError::not_found(format!("no message {id}"))),
    }
}

fn publish(st: &AppState, seat: &str, bump: Option<SeqBump>) {
    if let Some(b) = bump {
        st.bus.publish(b.org_id, &[(seat.to_string(), b.seq)]);
    }
}

pub async fn send(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::SendRequest>,
) -> ApiResult<Json<p::SendResponse>> {
    Ok(Json(crate::send::send(&st, &auth, req).await?))
}

#[derive(Deserialize)]
pub struct InboxParams {
    seat: Option<String>,
    #[serde(default)]
    unread: Option<String>,
    after_seq: Option<i64>,
    before_seq: Option<i64>,
    limit: Option<i64>,
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true" | "yes" | ""))
}

pub async fn inbox(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<InboxParams>,
) -> ApiResult<Json<p::InboxResponse>> {
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    let query = InboxQuery {
        unread_only: flag(&q.unread),
        after_seq: q.after_seq,
        before_seq: q.before_seq,
        limit: q.limit.unwrap_or(50).clamp(1, 1000),
    };
    let entries = st.store.inbox(seat.id, &query).await?;
    Ok(Json(p::InboxResponse {
        seat: seat.name,
        entries: entries
            .iter()
            .map(|e| render::entry(e, &auth.org))
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct ReadParams {
    peek: Option<String>,
}

pub async fn read(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Query(q): Query<ReadParams>,
) -> ApiResult<Json<p::MessageResponse>> {
    let m = readable_message(&st, &auth, id).await?;
    let seat = auth.mail_seat()?;
    let delivery = if flag(&q.peek) {
        st.store.delivery(id, seat.id).await?
    } else {
        match st.store.mark_read(id, seat.id).await? {
            Some((d, bump)) => {
                publish(&st, &seat.name, bump);
                Some(d)
            }
            None => None,
        }
    };
    Ok(Json(p::MessageResponse {
        message: render::message(&m, &auth.org),
        delivery: delivery.map(|d| render::delivery(&d, &auth.org)),
    }))
}

pub async fn ack(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::AckResponse>> {
    readable_message(&st, &auth, id).await?;
    let seat = auth.mail_seat()?;
    let (d, bump) = st.store.ack(id, seat.id).await?.ok_or_else(|| {
        ApiError::not_found(format!("message {id} was not delivered to {}", seat.name))
    })?;
    publish(&st, &seat.name, bump);
    Ok(Json(p::AckResponse {
        delivery: render::delivery(&d, &auth.org),
    }))
}

pub async fn status(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::StatusResponse>> {
    let m = readable_message(&st, &auth, id).await?;
    let seat = auth.mail_seat()?;
    let allowed = m.sender_seat_id == seat.id || (auth.is_user() && m.org_id == auth.org.id);
    if !allowed {
        return Err(ApiError::forbidden(
            "only the sender and the sender org's user can see delivery status",
        ));
    }
    let recipients = st
        .store
        .recipients(id)
        .await?
        .iter()
        .map(|d| {
            let delivery = render::delivery(d, &auth.org);
            p::RecipientStatus {
                address: delivery.seat.clone(),
                state: p::delivery_state(&delivery).into(),
                delivery,
            }
        })
        .collect();
    Ok(Json(p::StatusResponse {
        message_id: id,
        recipients,
    }))
}

pub async fn thread(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::ThreadResponse>> {
    let seat = auth.mail_seat()?;
    if auth.scope == TokenScope::Seat && !st.store.seat_in_thread(seat.id, id).await? {
        return Err(ApiError::not_found(format!("no thread {id}")));
    }
    let messages = st.store.thread(id, auth.org.id).await?;
    if messages.is_empty() {
        return Err(ApiError::not_found(format!("no thread {id}")));
    }
    Ok(Json(p::ThreadResponse {
        thread_id: id,
        messages: messages
            .iter()
            .map(|m| render::message(m, &auth.org))
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct SeatParam {
    seat: Option<String>,
}

pub async fn claim(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SeatParam>,
    Json(req): Json<p::ClaimRequest>,
) -> ApiResult<Json<p::ClaimResponse>> {
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    if req.via.is_empty() || req.via.len() > 100 {
        return Err(ApiError::bad_request("via must be 1..=100 characters"));
    }
    if req
        .key
        .as_ref()
        .is_some_and(|k| k.is_empty() || k.len() > 100)
    {
        return Err(ApiError::bad_request("key must be 1..=100 characters"));
    }
    let filter = ClaimFilter {
        classes: req.classes,
        urgent_only: req.min_priority == Some(Priority::Urgent),
        include_digest: req.include_digest,
        via: req.via,
        limit: req.limit.unwrap_or(50).clamp(1, 500) as i64,
        read_body_max: match req.inline_body_max {
            Some(max) if req.mark_inlined_read && max > 0 => Some(max as i64),
            _ => None,
        },
        digest_only: req.digest_only,
        key: req.key,
    };
    let (entries, bump) = st.store.claim(seat.id, &filter).await?;
    publish(&st, &seat.name, bump);
    let max = req.inline_body_max.unwrap_or(0) as usize;
    let entries = entries
        .iter()
        .map(|e| {
            let mut out = render::entry(e, &auth.org);
            if max > 0 && e.message.body.chars().count() <= max {
                out.body = Some(e.message.body.clone());
            }
            if let Some(n) = req.preview_chars {
                out.preview = Some(render::preview(&e.message.body, n as usize));
            }
            out
        })
        .collect();
    Ok(Json(p::ClaimResponse {
        seat: seat.name,
        entries,
    }))
}

#[derive(Deserialize)]
pub struct ChangesParams {
    seat: Option<String>,
    since: Option<i64>,
    limit: Option<i64>,
}

pub async fn changes(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ChangesParams>,
) -> ApiResult<Json<p::ChangesResponse>> {
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    let since = q.since.unwrap_or(0);
    let limit = q.limit.unwrap_or(500).clamp(1, 5000);
    let entries = st.store.changes(seat.id, since, limit).await?;
    let next = entries.last().map(|e| e.delivery.mod_seq).unwrap_or(since);
    Ok(Json(p::ChangesResponse {
        seat: seat.name,
        more: entries.len() as i64 == limit,
        next,
        entries: entries
            .iter()
            .map(|e| render::entry(e, &auth.org))
            .collect(),
    }))
}

/// `GET /v1/events`: SSE stream of `mailbox` events (`{"seat","seq"}`).
/// Seat tokens get their own seat; user and node tokens get every seat of the
/// org, or one seat with `seat=`. With `Last-Event-ID`, missed events are
/// replayed; if they are no longer available, the current counter of every
/// seat in scope is sent instead.
pub async fn events(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SeatParam>,
    headers: HeaderMap,
) -> ApiResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    let only: Option<String> = match auth.scope {
        TokenScope::Seat => Some(auth.mailbox(&st, q.seat.as_deref()).await?.name),
        _ => match q.seat {
            Some(name) => Some(auth.mailbox(&st, Some(&name)).await?.name),
            None => None,
        },
    };
    let guard = st.limiters.streams.open(auth.org.id).ok_or_else(|| {
        ApiError::too_many(
            "too_many_streams",
            "this org has too many open event streams",
            5000,
        )
    })?;
    let last = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let sub = st.bus.subscribe(auth.org.id, last.as_deref());
    let presence_rx = st.presence.subscribe(auth.org.id);
    let only2 = only.clone();
    let keep = move |seat: &str| only.as_deref().is_none_or(|o| o == seat);

    let initial: Vec<Event> = match (&last, sub.replay) {
        (Some(_), None) => {
            // Snapshot: current counters, tagged with the latest event id.
            let id = sub.latest_id.clone().unwrap_or_default();
            st.store
                .seat_seqs(auth.org.id)
                .await?
                .into_iter()
                .filter(|(_, name, _)| keep(name))
                .map(|(_, seat, seq)| sse_event(&id, &p::MailboxEvent { seat, seq }))
                .collect()
        }
        (_, Some(replay)) => replay
            .into_iter()
            .filter(|e| keep(&e.seat))
            .map(|e| {
                sse_event(
                    &e.id,
                    &p::MailboxEvent {
                        seat: e.seat,
                        seq: e.seq,
                    },
                )
            })
            .collect(),
        (None, None) => Vec::new(),
    };
    // A lagging subscriber's stream ends; the client reconnects with
    // Last-Event-ID and gets a replay or a snapshot.
    let live = BroadcastStream::new(sub.rx)
        .take_while(|r| std::future::ready(r.is_ok()))
        .filter_map(move |r| {
            let out = match r {
                Ok(e) if keep(&e.seat) => Some(sse_event(
                    &e.id,
                    &p::MailboxEvent {
                        seat: e.seat,
                        seq: e.seq,
                    },
                )),
                _ => None,
            };
            std::future::ready(out)
        });
    // Presence events have no id and are not replayed; a lagging presence
    // receiver just skips what it missed.
    let presence = BroadcastStream::new(presence_rx).filter_map(move |r| {
        let out = match r {
            Ok(e) if only2.as_deref().is_none_or(|o| o == e.seat) => Some(
                Event::default()
                    .event("presence")
                    .data(serde_json::to_string(&e).unwrap_or_default()),
            ),
            _ => None,
        };
        std::future::ready(out)
    });
    // Node tokens also get `command` events (sleep, wake, transcript leases);
    // while the stream is open the node counts as connected.
    let commands = if auth.scope == TokenScope::Node {
        let (rx, conn) = st.nodes.connect(auth.org.id);
        BroadcastStream::new(rx)
            .filter_map(move |r| {
                let _conn = &conn;
                std::future::ready(r.ok().map(|c| {
                    Event::default()
                        .event("command")
                        .data(serde_json::to_string(&c).unwrap_or_default())
                }))
            })
            .boxed()
    } else {
        stream::empty().boxed()
    };
    // The response ends when the mailbox stream ends (lag), even though the
    // presence stream would go on.
    let mailbox = stream::iter(initial)
        .chain(live)
        .map(Some)
        .chain(stream::once(std::future::ready(None)));
    let others = stream::select(presence, commands).map(Some);
    let stream = stream::select(mailbox, others)
        .take_while(|e| std::future::ready(e.is_some()))
        .filter_map(std::future::ready)
        .map(move |e| {
            let _open = &guard;
            Ok(e)
        });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

fn sse_event(id: &str, ev: &p::MailboxEvent) -> Event {
    let e = Event::default()
        .event("mailbox")
        .data(serde_json::to_string(ev).unwrap_or_default());
    if id.is_empty() { e } else { e.id(id) }
}

#[derive(Deserialize)]
pub struct ArchiveParams {
    since: Option<String>,
    until: Option<String>,
    from: Option<String>,
    to: Option<String>,
    thread: Option<i64>,
    before_id: Option<i64>,
    limit: Option<i64>,
}

pub async fn archive(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<ArchiveParams>,
) -> ApiResult<Json<p::ArchiveResponse>> {
    auth.require_user()?;
    let time = |v: &Option<String>, name: &str| -> ApiResult<Option<i64>> {
        match v {
            None => Ok(None),
            Some(s) => render::parse_ts(s).map(Some).ok_or_else(|| {
                ApiError::bad_request(format!("{name} must be an RFC 3339 timestamp"))
            }),
        }
    };
    let from = match &q.from {
        None => None,
        Some(a) => {
            let a: Address = a
                .parse()
                .map_err(|e| ApiError::bad_request(format!("from: {e}")))?;
            Some((a.org_or(&auth.org.name).to_string(), a.name))
        }
    };
    let to = match &q.to {
        None => None,
        Some(a) => {
            let a: Address = a
                .parse()
                .map_err(|e| ApiError::bad_request(format!("to: {e}")))?;
            Some((a.org_or(&auth.org.name).to_string(), a.name))
        }
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = ArchiveQuery {
        to,
        since: time(&q.since, "since")?,
        until: time(&q.until, "until")?,
        from,
        thread_id: q.thread,
        before_id: q.before_id,
        limit,
    };
    let msgs = st.store.archive(auth.org.id, &query).await?;
    let next_before_id = if msgs.len() as i64 == limit {
        msgs.last().map(|m| m.id)
    } else {
        None
    };
    Ok(Json(p::ArchiveResponse {
        messages: msgs.iter().map(|m| render::header(m, &auth.org)).collect(),
        next_before_id,
    }))
}
