//! Mail endpoints: send, inbox, read, ack, status, threads, claim, changes,
//! events, archive.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json, Path, Query};
pub use crate::limits::Pool;
use crate::render;
use amail_proto::limits::{MAX_PAGE_BYTES, MAX_THREAD_BODY_BYTES, MAX_THREAD_MESSAGES};
use amail_proto::{self as p, Address, Priority, TokenScope};
use amail_store::{ArchiveQuery, ClaimFilter, InboxQuery, MessageRec, SeqBump};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::{Stream, StreamExt, stream};
use serde::Deserialize;
use std::convert::Infallible;

/// How long one search or archive request looks at further messages (see
/// [`amail_proto::limits::SCAN_TIME_LIMIT_MS`]).
pub const SCAN_TIME_LIMIT: std::time::Duration =
    std::time::Duration::from_millis(amail_proto::limits::SCAN_TIME_LIMIT_MS);
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

/// `POST /v1/seats/{name}/messages`: see [`crate::send::send_as`].
pub async fn send_as(
    State(st): State<AppState>,
    auth: Auth,
    Path(name): Path<String>,
    Json(req): Json<p::SendRequest>,
) -> ApiResult<Json<p::SendResponse>> {
    Ok(Json(crate::send::send_as(&st, &auth, &name, req).await?))
}

/// `POST /v1/messages/preview`: see [`crate::send::preview`].
pub async fn preview(
    State(st): State<AppState>,
    auth: Auth,
    Json(req): Json<p::SendRequest>,
) -> ApiResult<Json<p::SendPreview>> {
    Ok(Json(crate::send::preview(&st, &auth, req).await?))
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
    let entries = if query.limit > p::limits::SMALL_PAGE {
        let (store, seat_id, query) = (st.store.clone(), seat.id, query.clone());
        limited(&st, &auth, Pool::Other, async move {
            Ok(store.inbox(seat_id, &query).await?)
        })
        .await?
    } else {
        st.store.inbox(seat.id, &query).await?
    };
    let mut entries: Vec<p::InboxEntry> = entries
        .iter()
        .map(|e| render::entry(e, &auth.org))
        .collect();
    // Paging backwards, the entries next to `before_seq` (the end) stay.
    let truncated = render::fit(&mut entries, MAX_PAGE_BYTES, query.before_seq.is_some());
    Ok(Json(p::InboxResponse {
        seat: seat.name,
        entries,
        truncated,
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
                crate::handover::mail_read(&st, seat, &m, "mail_read").await;
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
    let m = readable_message(&st, &auth, id).await?;
    let seat = auth.mail_seat()?;
    let (d, bump) = st.store.ack(id, seat.id).await?.ok_or_else(|| {
        ApiError::not_found(format!("message {id} was not delivered to {}", seat.name))
    })?;
    publish(&st, &seat.name, bump);
    crate::handover::mail_read(&st, seat, &m, "mail_acked").await;
    Ok(Json(p::AckResponse {
        delivery: render::delivery(&d, &auth.org),
    }))
}

/// `POST /v1/messages/{id}/deliveries/{seat}/withdraw`: the sender of a
/// message, or its org's user, withdraws the delivery to a seat of the
/// org that has not been notified, read or acknowledged. The delivery
/// stays (the recipient can still read the message by id and in its
/// threads) but leaves the recipient's unread mail and pending counts, so
/// it is never injected and wakes nobody. Withdrawing a withdrawn delivery
/// again answers `changed: false`.
pub async fn withdraw(
    State(st): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(i64, String)>,
) -> ApiResult<Json<p::WithdrawResponse>> {
    let m = readable_message(&st, &auth, id).await?;
    let seat = auth.mail_seat()?;
    let allowed = m.org_id == auth.org.id && (m.sender_seat_id == seat.id || auth.is_user());
    if !allowed {
        return Err(ApiError::forbidden(
            "only the sender and the sender org's user can withdraw a delivery",
        ));
    }
    if name.contains('@') {
        return Err(ApiError::unprocessable(
            "not_local",
            "only deliveries to seats of your own org can be withdrawn",
        ));
    }
    let recipient = st
        .store
        .seat_by_name(auth.org.id, &name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    let (d, bump) =
        st.store.withdraw(id, recipient.id).await?.ok_or_else(|| {
            ApiError::not_found(format!("message {id} was not delivered to {name}"))
        })?;
    if d.withdrawn_at.is_none() {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "delivery_seen",
            format!("{name} was already shown, has read or has acknowledged message {id}"),
        ));
    }
    let changed = bump.is_some();
    publish(&st, &recipient.name, bump);
    if changed {
        tracing::info!(
            org_id = auth.org.id,
            token_id = auth.token_id,
            message_id = id,
            seat = %recipient.name,
            "delivery withdrawn"
        );
    }
    Ok(Json(p::WithdrawResponse {
        delivery: render::delivery(&d, &auth.org),
        changed,
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
    let store = st.store.clone();
    let rows = limited(&st, &auth, Pool::Other, async move {
        Ok(store.recipients(id).await?)
    })
    .await?;
    let recipients = rows
        .iter()
        .map(|d| {
            let mut delivery = render::delivery(d, &auth.org);
            let state = p::delivery_state(&delivery).into();
            // A seat of another org: its state and the read and ack times
            // only; its mailbox counters, how and when it was notified are
            // that org's business.
            if d.org_id != auth.org.id {
                delivery.seq = 0;
                delivery.mod_seq = 0;
                delivery.notified_at = None;
                delivery.notified_via = None;
            }
            p::RecipientStatus {
                address: delivery.seat.clone(),
                state,
                delivery,
                members: None,
            }
        })
        .collect();
    Ok(Json(p::StatusResponse {
        message_id: id,
        written_by: m.written_by.clone(),
        recipients,
    }))
}

pub async fn thread(
    State(st): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<p::ThreadResponse>> {
    let seat = auth.mail_seat()?;
    // Whether the caller sees the thread is a few index lookups, so a
    // thread it cannot see costs what a missing one does; only then is the
    // thread loaded.
    let visible = match auth.scope {
        TokenScope::Seat => st.store.seat_in_thread(seat.id, id).await?,
        _ => st.store.org_sees_thread(auth.org.id, id).await?,
    };
    if !visible {
        return Err(ApiError::not_found(format!("no thread {id}")));
    }
    let store = st.store.clone();
    let org_id = auth.org.id;
    let mut found = limited(&st, &auth, Pool::Other, async move {
        Ok(store
            .thread(id, org_id, MAX_THREAD_MESSAGES + 1, MAX_THREAD_BODY_BYTES)
            .await?)
    })
    .await?;
    if found.is_empty() {
        return Err(ApiError::not_found(format!("no thread {id}")));
    }
    let mut truncated = found.len() as i64 > MAX_THREAD_MESSAGES;
    if truncated {
        found.remove(0);
    }
    let participants = crate::boundary::participants(&st, &auth.org, id).await?;
    let mut messages: Vec<p::Message> = found
        .iter()
        .map(|t| {
            let mut m = render::message(&t.message, &auth.org);
            m.body_omitted = t.body_omitted;
            m
        })
        .collect();
    // Bodies, newest first, within MAX_THREAD_BODY_BYTES as JSON (the
    // store kept their raw size within it); headers, newest first, within
    // MAX_PAGE_BYTES.
    let (mut bodies, mut headers) = (0usize, 0usize);
    let mut keep = 0;
    for m in messages.iter_mut().rev() {
        if !m.body.is_empty() {
            bodies += render::json_len(&m.body);
            if bodies > MAX_THREAD_BODY_BYTES {
                m.body = String::new();
                m.body_omitted = true;
            }
        }
        headers += render::json_len(&m.header) + 1;
        if headers > MAX_PAGE_BYTES && keep > 0 {
            break;
        }
        keep += 1;
    }
    if keep < messages.len() {
        messages.drain(..messages.len() - keep);
        truncated = true;
    }
    let tasks = crate::tasks::thread_refs(&st, auth.org.id, id).await;
    let (docs, doc_comments) = crate::docs::thread_refs(&st, auth.org.id, id).await;
    Ok(Json(p::ThreadResponse {
        thread_id: id,
        truncated,
        messages,
        participants,
        tasks,
        docs,
        doc_comments,
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
    // Claims load at most this much of each body (see the store).
    let inline_max = req
        .inline_body_max
        .map(|m| (m as usize).min(p::limits::MAX_CLAIM_BODY_CHARS));
    let preview_chars = req
        .preview_chars
        .map(|n| (n as usize).min(p::limits::MAX_CLAIM_BODY_CHARS));
    let filter = ClaimFilter {
        classes: req.classes,
        urgent_only: req.min_priority == Some(Priority::Urgent),
        include_digest: req.include_digest,
        via: req.via,
        limit: req.limit.unwrap_or(50).clamp(1, 500) as i64,
        read_body_max: match inline_max {
            Some(max) if req.mark_inlined_read && max > 0 => Some(max as i64),
            _ => None,
        },
        digest_only: req.digest_only,
        key: req.key,
    };
    let (entries, bump) = st.store.claim(seat.id, &filter).await?;
    publish(&st, &seat.name, bump);
    let max = inline_max.unwrap_or(0);
    let entries = entries
        .iter()
        .map(|e| {
            let mut out = render::entry(e, &auth.org);
            if max > 0 && e.message.body.chars().count() <= max {
                out.body = Some(e.message.body.clone());
            }
            if let Some(n) = preview_chars {
                out.preview = Some(render::preview(&e.message.body, n));
            }
            out
        })
        .collect();
    Ok(Json(p::ClaimResponse {
        seat: seat.name,
        entries,
    }))
}

/// `POST /v1/claim/release` (node tokens): see [`p::ReleaseRequest`].
pub async fn release(
    State(st): State<AppState>,
    auth: Auth,
    Query(q): Query<SeatParam>,
    Json(req): Json<p::ReleaseRequest>,
) -> ApiResult<Json<p::ReleaseResponse>> {
    if auth.scope != p::TokenScope::Node {
        return Err(ApiError::forbidden("requires a node token"));
    }
    let seat = auth.mailbox(&st, q.seat.as_deref()).await?;
    if req.key.is_empty() || req.key.len() > 100 {
        return Err(ApiError::bad_request("key must be 1..=100 characters"));
    }
    if req.messages.len() > 500 {
        return Err(ApiError::bad_request("at most 500 messages"));
    }
    let (released, bump) = st
        .store
        .release_claim(seat.id, &req.key, &req.messages)
        .await?;
    publish(&st, &seat.name, bump);
    Ok(Json(p::ReleaseResponse {
        seat: seat.name,
        released,
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
    let store = st.store.clone();
    let seat_id = seat.id;
    let entries = limited(&st, &auth, Pool::Other, async move {
        Ok(store.changes(seat_id, since, limit).await?)
    })
    .await?;
    let full = entries.len() as i64 == limit;
    let mut entries: Vec<p::InboxEntry> = entries
        .iter()
        .map(|e| render::entry(e, &auth.org))
        .collect();
    let cut = render::fit(&mut entries, MAX_PAGE_BYTES, false);
    let next = entries.last().map(|e| e.delivery.mod_seq).unwrap_or(since);
    Ok(Json(p::ChangesResponse {
        seat: seat.name,
        more: full || cut,
        next,
        entries,
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
    // The stream ends when its token (or browser session) is revoked.
    let (cred, ended) = st.revocations.open(&auth);
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
    // receiver just skips what it missed. `node_connected` is filled in as
    // in `GET /v1/seats`.
    let (nodes, org_id) = (st.nodes.clone(), auth.org.id);
    let presence = BroadcastStream::new(presence_rx).filter_map(move |r| {
        let out = match r {
            Ok(mut e) if only2.as_deref().is_none_or(|o| o == e.seat) => {
                e.presence.node_connected = nodes.connected(org_id) > 0;
                Some(
                    Event::default()
                        .event("presence")
                        .data(serde_json::to_string(&e).unwrap_or_default()),
                )
            }
            _ => None,
        };
        std::future::ready(out)
    });
    // User tokens also get `directory` events (seats and lists changed) and
    // `nodes` events (the number of connected nodes, now and on change).
    let user_events = if auth.scope == TokenScope::User {
        let dir = BroadcastStream::new(st.directory.subscribe(auth.org.id)).filter_map(|r| {
            std::future::ready(r.ok().map(|d| {
                Event::default()
                    .event("directory")
                    .data(serde_json::to_string(&d).unwrap_or_default())
            }))
        });
        let nodes =
            tokio_stream::wrappers::WatchStream::new(st.nodes.watch(auth.org.id)).map(|n| {
                Event::default().event("nodes").data(
                    serde_json::to_string(&p::NodesEvent {
                        connected: n as u32,
                    })
                    .unwrap_or_default(),
                )
            });
        stream::select(dir, nodes).boxed()
    } else {
        stream::empty().boxed()
    };
    // User and seat tokens also get `task` events (a task of the org
    // changed).
    let task_events = if matches!(auth.scope, TokenScope::User | TokenScope::Seat) {
        BroadcastStream::new(st.tasks.subscribe(auth.org.id))
            .filter_map(|r| {
                std::future::ready(r.ok().map(|t| {
                    Event::default()
                        .event("task")
                        .data(serde_json::to_string(&t).unwrap_or_default())
                }))
            })
            .boxed()
    } else {
        stream::empty().boxed()
    };
    // And `doc` events (a document of the org changed).
    let doc_events = if matches!(auth.scope, TokenScope::User | TokenScope::Seat) {
        BroadcastStream::new(st.docs.subscribe(auth.org.id))
            .filter_map(|r| {
                std::future::ready(r.ok().map(|t| {
                    Event::default()
                        .event("doc")
                        .data(serde_json::to_string(&t).unwrap_or_default())
                }))
            })
            .boxed()
    } else {
        stream::empty().boxed()
    };
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
    let others = stream::select(
        stream::select(presence, commands),
        stream::select(user_events, stream::select(task_events, doc_events)),
    )
    .map(Some);
    let stream = stream::select(mailbox, others)
        .take_while(|e| std::future::ready(e.is_some()))
        .filter_map(std::future::ready)
        .take_until(ended)
        .map(move |e| {
            let _open = (&guard, &cred);
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
        time_limit: Some(SCAN_TIME_LIMIT),
    };
    let store = st.store.clone();
    let org_id = auth.org.id;
    let page = limited(&st, &auth, Pool::Scan, async move {
        Ok(store.archive(org_id, &query).await?)
    })
    .await?;
    Ok(Json(archive_response(page, &auth.org)))
}

/// An archive page as sent: within [`MAX_PAGE_BYTES`], continuing after
/// the last message kept when it was cut.
pub fn archive_response(
    page: amail_store::ArchivePage,
    org: &amail_store::Org,
) -> p::ArchiveResponse {
    let mut messages: Vec<p::MessageHeader> = page
        .messages
        .iter()
        .map(|m| render::header(m, org))
        .collect();
    let next_before_id = if render::fit(&mut messages, MAX_PAGE_BYTES, false) {
        messages.last().map(|m| m.id)
    } else {
        page.next_before_id
    };
    p::ArchiveResponse {
        messages,
        next_before_id,
        delivery: Vec::new(),
    }
}

/// Run `f`, database work whose cost depends on the data, in one of the
/// org's query slots of `pool` ([`crate::config::Limits::org_queries`] per
/// org, [`crate::config::Limits::queries`] in all except for
/// [`Pool::Light`]). Without a free slot after a short wait the request
/// gets 429 `too_many_queries`. The work runs in its own task, which holds
/// the slot until the work ends: a client that drops the request does not
/// free the slot while the database is still busy.
pub async fn limited<T, F>(st: &AppState, auth: &Auth, pool: Pool, f: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = ApiResult<T>> + Send + 'static,
{
    use crate::limits::SlotRefused;
    let slot = st
        .limiters
        .queries
        .acquire(pool, auth.org.id)
        .await
        .map_err(|r| match r {
            SlotRefused::Org => ApiError::too_many(
                "too_many_queries",
                "this org has too many search, archive and similar requests in progress",
                1000,
            ),
            SlotRefused::Total => ApiError::too_many(
                "too_many_queries",
                "the server has too many search, archive and similar requests in progress",
                1000,
            ),
        })?;
    tokio::spawn(async move {
        let out = f.await;
        drop(slot);
        out
    })
    .await
    .unwrap_or_else(|e| {
        Err(ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("query task failed: {e}"),
        ))
    })
}
