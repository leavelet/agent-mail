//! The send pipeline: authorization, attention protection, recipient
//! resolution and storage.
//!
//! Resolution rules:
//! - `name` resolves in the sender's org to a live seat (class `to`/`cc`,
//!   mode `immediate`) or a list. The delivery class follows the header
//!   field, not whether a list was involved: members of a list in `To` get
//!   class `to`, of a list in `Cc` class `cc`, each with their member mode
//!   (a `digest` or `archive` member is not notified at once). The sender
//!   never receives its own list mail. Class `list` is no longer produced;
//!   older deliveries may still carry it.
//! - `name@org` for another org requires an active link (else
//!   `not_linked`, also when there is no such org or it is hidden) and
//!   resolves only to a seat that the reachability rule
//!   ([`crate::boundary::Reach`]) accepts: a contact of that org for the
//!   sender's org, or a participant of the thread. Anything else (a seat
//!   that is neither, a list, a name that does not exist there) is
//!   rejected alike with `unreachable`, after the same lookups. When the
//!   thread crossed between the two orgs before and one of them has no
//!   contact left on it, the recipients there get `thread_closed`.
//! - A message that reaches another org names no list (422
//!   `list_on_cross_org`), and carries a contact of the sender's org for
//!   each other org: when none is on it, the server adds the org's primary
//!   contact for that org in Cc (422 `no_contact` when there is none).
//! - List members are seats of the list's own org. Expansion is one level
//!   deep; there are no nested lists.
//! - A new thread that reaches another org takes a token from the sending
//!   seat's budget of new threads to other orgs (the org's
//!   `new_threads_per_hour`; the user seat is exempt).
//! - A seat reached through several addresses gets one delivery with the
//!   highest class (`to` > `cc`) and the most immediate mode.

use crate::AppState;
use crate::auth::Auth;
use crate::boundary::{Reach, Verdict};
use crate::error::{ApiError, ApiResult};
use amail_proto::{
    self as p, Address, AddressResult, DeliveryClass, DeliveryMode, PostPolicy, Priority,
    RecipientNote, Rejected, SeatKind, SeatState, SendRequest, SendResponse,
};
use amail_store::{CardRec, NewDelivery, NewMessage, Org, Seat};
use std::collections::HashMap;

pub async fn send(st: &AppState, auth: &Auth, req: SendRequest) -> ApiResult<SendResponse> {
    let sender = auth.mail_seat()?.clone();
    send_from(st, auth, &sender, None, req, false).await
}

/// A comment of a task (`POST /v1/tasks/{num}/comments`), sent as the
/// caller's seat. `in_reply_to` is the task's discussion thread, which
/// every seat of the org may comment in: the reader's access to it is not
/// asked. The caller records the comment on the task.
pub async fn send_comment(
    st: &AppState,
    auth: &Auth,
    sender: &Seat,
    req: SendRequest,
) -> ApiResult<SendResponse> {
    send_from(st, auth, sender, None, req, true).await
}

/// `POST /v1/seats/{name}/messages`: the org's user writes a message in the
/// name of one of the org's agent seats. The message is the seat's in every
/// respect that decides what it may do (its source is `agent`; recipients,
/// the reachability rule, the send budget and the budget of new threads to
/// other orgs are the seat's), and it records that the user wrote it
/// (`written_by`), which every reader is shown. The user's own access
/// decides whether `in_reply_to` may be named, as for the user's own mail.
/// Only user tokens may do this, and only for agent seats.
pub async fn send_as(
    st: &AppState,
    auth: &Auth,
    name: &str,
    req: SendRequest,
) -> ApiResult<SendResponse> {
    auth.require_user()?;
    let user = auth.mail_seat()?.name.clone();
    let sender = st
        .store
        .seat_by_name(auth.org.id, name)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("no seat {name:?}")))?;
    if sender.kind != SeatKind::Agent {
        return Err(ApiError::forbidden(
            "only agent seats can be written for; the user sends as itself",
        ));
    }
    let result = send_from(st, auth, &sender, Some(user), req, false).await?;
    tracing::info!(
        org_id = auth.org.id,
        token_id = auth.token_id,
        seat = %sender.name,
        message_id = result.id,
        "message written by the user in the name of a seat"
    );
    Ok(result)
}

/// Send `req` as `sender`; `written_by`: the user seat that wrote it in the
/// sender's name; `comment`: a task comment (see [`send_comment`]).
async fn send_from(
    st: &AppState,
    auth: &Auth,
    sender: &Seat,
    written_by: Option<String>,
    req: SendRequest,
    comment: bool,
) -> ApiResult<SendResponse> {
    let org = &auth.org;
    if req.to.is_empty() && req.cc.is_empty() {
        return Err(ApiError::bad_request("at least one recipient is required"));
    }
    check_sizes(&req)?;
    if let Some(t) = req.ack_timeout_s
        && t > 30 * 86_400
    {
        return Err(ApiError::bad_request(
            "ack_timeout_s must be between 0 (no reminder) and 2592000 (30 days)",
        ));
    }
    // Protective limits (public mode): all orgs, then this org.
    if let Some(b) = &st.limiters.global_send
        && let Err(ms) = b.take(&())
    {
        return Err(ApiError::too_many(
            "server_busy",
            "the server is at its message rate limit; retry shortly",
            ms,
        ));
    }
    if let Some(b) = &st.limiters.org_send
        && let Err(ms) = b.take(&org.id)
    {
        if let Some(g) = &st.limiters.global_send {
            g.refund(&());
        }
        return Err(ApiError::too_many(
            "org_rate_limited",
            "this org is at the server's per-org message rate limit; retry shortly",
            ms,
        ));
    }
    let limited = sender.kind != SeatKind::User;
    if limited {
        st.store
            .take_send_token(sender.id, org.send_rate_per_min, org.send_burst)
            .await?
            .map_err(ApiError::rate_limited)?;
    }
    // A link ended or an opening was removed between the check and the
    // send's transaction: decide again (the recipient is then refused).
    let mut result = send_inner(st, auth, sender, written_by.clone(), req.clone(), comment).await;
    if matches!(&result, Err(e) if e.code == "boundary_changed") {
        result = send_inner(st, auth, sender, written_by, req, comment).await;
    }
    if result.is_err()
        && limited
        && let Err(e) = st.store.refund_send_token(sender.id, org.send_burst).await
    {
        tracing::warn!("refunding a send token: {e}");
    }
    result
}

/// Fixed size limits (see [`amail_proto::limits`]).
fn check_sizes(req: &SendRequest) -> ApiResult<()> {
    use amail_proto::limits::*;
    let too = |code: &'static str, msg: String| Err(ApiError::unprocessable(code, msg));
    if req.subject.chars().any(char::is_control) {
        return too(
            "invalid_subject",
            "the subject must be a single line without control characters".into(),
        );
    }
    if req.subject.chars().count() > MAX_SUBJECT_CHARS {
        return too(
            "subject_too_long",
            format!("subject is limited to {MAX_SUBJECT_CHARS} characters"),
        );
    }
    if req.body.len() > MAX_BODY_BYTES {
        return too(
            "body_too_large",
            format!("body is limited to {} KB", MAX_BODY_BYTES / 1024),
        );
    }
    if req.to.len() + req.cc.len() > MAX_ADDRESSES {
        return too(
            "too_many_addresses",
            format!("a message may name at most {MAX_ADDRESSES} addresses"),
        );
    }
    if req.meta.len() > MAX_META_KEYS {
        return too(
            "invalid_meta",
            format!("meta is limited to {MAX_META_KEYS} keys"),
        );
    }
    let bytes: usize = req.meta.iter().map(|(k, v)| k.len() + v.len()).sum();
    if bytes > MAX_META_BYTES {
        return too(
            "invalid_meta",
            format!("meta is limited to {MAX_META_BYTES} bytes"),
        );
    }
    if let Some(k) = req.meta.keys().find(|k| !valid_meta_key(k)) {
        return too(
            "invalid_meta",
            format!("invalid meta key {k:?}: use 1-32 of [a-z0-9_.-]"),
        );
    }
    if let Some(k) = req
        .meta
        .iter()
        .find(|(_, v)| v.chars().any(char::is_control))
        .map(|(k, _)| k)
    {
        return too(
            "invalid_meta",
            format!("meta value of {k:?} must be a single line without control characters"),
        );
    }
    Ok(())
}

fn valid_meta_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 32
        && k.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// `POST /v1/messages/preview`: the recipients a message would have
/// (with the contact the server would add) and its rejections, without
/// sending it or using any budget.
pub async fn preview(st: &AppState, auth: &Auth, req: SendRequest) -> ApiResult<p::SendPreview> {
    let sender = auth.mail_seat()?.clone();
    if req.to.is_empty() && req.cc.is_empty() {
        return Err(ApiError::bad_request("at least one recipient is required"));
    }
    check_sizes(&req)?;
    let (r, _) = resolve_all(st, auth, &sender, &req, false).await?;
    Ok(p::SendPreview {
        deliveries: r.seats.len() as u32,
        recipients: r.results,
        rejected: r.rejected,
    })
}

/// The thread a message joins: `(thread, parent, depth)`.
type Placement = (Option<i64>, Option<i64>, i64);

/// Thread inheritance, the hop limit and recipient resolution (with the
/// rules for other orgs).
async fn resolve_all<'a>(
    st: &'a AppState,
    auth: &'a Auth,
    sender: &'a Seat,
    req: &SendRequest,
    comment: bool,
) -> ApiResult<(Resolver<'a>, Placement)> {
    let org = &auth.org;
    // Thread inheritance and the hop limit.
    let (thread_id, parent_id, depth) = match req.in_reply_to {
        None => (None, None, 0),
        Some(pid) => {
            let parent = st.store.message(pid).await?;
            let parent = match parent {
                Some(p) if comment && p.org_id == org.id => p,
                Some(p) if crate::mail::can_read(st, auth, &p).await? => p,
                _ => return Err(ApiError::not_found(format!("no message {pid}"))),
            };
            let depth = parent.depth + 1;
            if depth > org.max_thread_depth {
                return Err(ApiError::unprocessable(
                    "thread_too_deep",
                    format!("reply chain is limited to {} hops", org.max_thread_depth),
                ));
            }
            (Some(parent.thread_id), Some(pid), depth)
        }
    };

    let mut r = Resolver::new(st, org, sender, thread_id);
    r.prefetch(req.to.iter().chain(&req.cc)).await?;
    for a in &req.to {
        r.resolve(a, DeliveryClass::To).await?;
    }
    for a in &req.cc {
        r.resolve(a, DeliveryClass::Cc).await?;
    }
    if r.seats.is_empty() {
        let mut e = ApiError::unprocessable("no_recipients", "no recipient could be resolved");
        e.rejected = r.rejected;
        return Err(e);
    }
    r.cross_org().await?;
    Ok((r, (thread_id, parent_id, depth)))
}

async fn send_inner(
    st: &AppState,
    auth: &Auth,
    sender: &Seat,
    written_by: Option<String>,
    req: SendRequest,
    comment: bool,
) -> ApiResult<SendResponse> {
    let org = &auth.org;
    let (mut r, (thread_id, parent_id, depth)) =
        resolve_all(st, auth, sender, &req, comment).await?;
    if r.seats.is_empty() {
        let mut e = ApiError::unprocessable("no_recipients", "no recipient could be resolved");
        e.rejected = r.rejected;
        return Err(e);
    }
    if r.seats.len() as i64 > org.max_recipients {
        return Err(ApiError::unprocessable(
            "too_many_recipients",
            format!(
                "message resolves to {} seats; the limit is {}",
                r.seats.len(),
                org.max_recipients
            ),
        ));
    }

    let deliveries: Vec<NewDelivery> = r.order.iter().map(|id| r.seats[id].0.clone()).collect();
    // A new thread to another org takes from the sender's budget of them
    // (the org's setting, at most the server's maximum; the user seat has
    // a budget of its own), and from each receiving seat's budget of new
    // threads from this org.
    let new_cross = thread_id.is_none() && deliveries.iter().any(|d| d.org_id != org.id);
    let per_hour = if sender.kind == SeatKind::User {
        amail_proto::limits::USER_NEW_THREADS_PER_HOUR
    } else {
        org.new_threads_per_hour
            .min(amail_proto::limits::MAX_NEW_THREADS_PER_HOUR)
    };
    let thread_token = new_cross;
    if new_cross {
        for d in deliveries
            .iter()
            .filter(|d| d.org_id != org.id && d.class == DeliveryClass::To)
        {
            if let Err(ms) = st.limiters.threads_to_seat.take(&(d.seat_id, org.id)) {
                let name = r
                    .seats
                    .get(&d.seat_id)
                    .map(|x| x.1.clone())
                    .unwrap_or_default();
                return Err(ApiError::too_many(
                    "receiver_rate_limited",
                    format!(
                        "{name} has received {} new threads from your org in the last hour; reply in an existing thread or wait",
                        amail_proto::limits::NEW_THREADS_TO_SEAT_PER_HOUR
                    ),
                    ms,
                ));
            }
        }
        st.store
            .take_thread_token(sender.id, per_hour)
            .await?
            .map_err(|ms| {
                ApiError::too_many(
                    "thread_rate_limited",
                    format!(
                        "this seat may start {per_hour} new threads to other orgs per hour; reply in an existing thread or wait"
                    ),
                    ms,
                )
            })?;
    }
    let ack_required = req.ack_required.unwrap_or(false);
    let priority = req.priority.unwrap_or(Priority::Normal);
    // `ack_timeout_s: 0`: the acknowledgement is asked for, but no reminder
    // is scheduled.
    let ack_due = (ack_required && req.ack_timeout_s != Some(0)).then(|| {
        let ms = match req.ack_timeout_s {
            Some(s) => s as i64 * 1000,
            None => org.ack_timeout_min.max(1) * 60_000,
        };
        amail_store::now_ms() + ms
    });
    let msg = NewMessage {
        org_id: org.id,
        sender_seat_id: sender.id,
        thread_id,
        parent_id,
        depth,
        subject: req.subject,
        body: req.body,
        to_addrs: std::mem::take(&mut r.to_addrs),
        cc_addrs: std::mem::take(&mut r.cc_addrs),
        priority,
        ack_required,
        meta: req.meta,
        deliveries,
        ack_due,
        written_by,
        storage_quota: st.config.limits.org_storage_bytes,
        notice: false,
        guards: std::mem::take(&mut r.reach.guards),
    };
    let sent = match st.store.send(&msg).await {
        Ok(s) => s,
        Err(e) => {
            if thread_token && let Err(e) = st.store.refund_thread_token(sender.id, per_hour).await
            {
                tracing::warn!("refunding a thread token: {e}");
            }
            return Err(match e {
                amail_store::StoreError::StorageQuota(stored) => {
                    ApiError::storage_quota(stored, st.config.limits.org_storage_bytes)
                }
                amail_store::StoreError::Conflict(m) if m == amail_store::GUARD_FAILED => {
                    ApiError::new(
                        axum::http::StatusCode::CONFLICT,
                        "boundary_changed",
                        "a link or an opening changed while sending; try again",
                    )
                }
                e => e.into(),
            });
        }
    };
    st.activity
        .record(std::iter::once(org.id).chain(sent.seqs.iter().map(|b| b.org_id)));

    let mut by_org: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
    for b in &sent.seqs {
        let name = r.seats[&b.seat_id].1.clone();
        by_org.entry(b.org_id).or_default().push((name, b.seq));
    }
    for (org_id, events) in &by_org {
        st.bus.publish(*org_id, events);
    }
    notify_users(st, org, &r.orgs, &sent).await;
    // Replies in a task's discussion thread and `[t12]` subjects (a task
    // comment is recorded by its caller).
    if !comment {
        crate::tasks::after_send(st, org, sender, msg.written_by.as_deref(), &msg, &sent).await;
        crate::docs::after_send(st, org, sender, msg.written_by.as_deref(), &msg, &sent).await;
        crate::requests::after_send(st, org, sender, msg.written_by.as_deref(), &msg, &sent).await;
    }
    crate::handover::mail_sent(st, org, sender, &msg, &sent).await;
    r.tell_new_participants(&msg.deliveries, sent.thread_id, &msg.subject)
        .await;
    let notes = notes(st, org, sender, &msg.deliveries, priority).await;

    Ok(SendResponse {
        id: sent.message_id,
        thread_id: sent.thread_id,
        recipients: std::mem::take(&mut r.results),
        rejected: std::mem::take(&mut r.rejected),
        deliveries: sent.seqs.len() as u32,
        notes,
    })
}

/// Recipients in the sender's org that will not see the message at once.
/// Agent seats only; `archive` deliveries are never shown, so not noted.
async fn notes(
    st: &AppState,
    org: &Org,
    sender: &Seat,
    deliveries: &[NewDelivery],
    priority: Priority,
) -> Vec<RecipientNote> {
    let local: Vec<&NewDelivery> = deliveries
        .iter()
        .filter(|d| d.org_id == org.id && d.seat_id != sender.id && d.mode != DeliveryMode::Archive)
        .collect();
    if local.is_empty() {
        return Vec::new();
    }
    let ids: Vec<i64> = local.iter().map(|d| d.seat_id).collect();
    let states = match st.store.recipient_states(&ids).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("reading recipient states: {e}");
            return Vec::new();
        }
    };
    let presence: HashMap<String, amail_proto::Presence> = match st.presence.snapshot(org.id).await
    {
        Ok(p) => p.into_iter().collect(),
        Err(e) => {
            tracing::warn!("reading presence: {e}");
            return Vec::new();
        }
    };
    let now = amail_store::now_ms();
    let urgent = priority == Priority::Urgent;
    let mut out = Vec::new();
    for s in states.iter().filter(|s| s.kind == SeatKind::Agent) {
        let Some(d) = local.iter().find(|d| d.seat_id == s.seat_id) else {
            continue;
        };
        // Mail that wakes an idle seat (and starts a sleeping one).
        let wakes = d.mode == DeliveryMode::Immediate && (d.class == DeliveryClass::To || urgent);
        if let Some(until) = s.focus_until.filter(|u| *u > now) {
            out.push(RecipientNote {
                seat: s.name.clone(),
                state: "focus".into(),
                wakes: urgent,
                until: Some(crate::render::ts(until)),
            });
            continue;
        }
        let state = match presence.get(&s.name) {
            None => "offline",
            Some(p) if p.state == SeatState::Offline => "offline",
            Some(p) if p.state == SeatState::Dormant => "dormant",
            Some(p) if p.wake_failed => "wake_failed",
            Some(p) if p.turn_failed.is_some() => "turn_failed",
            Some(_) => continue,
        };
        out.push(RecipientNote {
            seat: s.name.clone(),
            state: state.into(),
            wakes: wakes && state == "dormant",
            until: None,
        });
    }
    out.sort_by(|a, b| a.seat.cmp(&b.seat));
    out
}

/// Webhook notices for the orgs whose user seat got the message.
async fn notify_users(
    st: &AppState,
    org: &Org,
    seen: &HashMap<String, Option<Org>>,
    sent: &amail_store::Sent,
) {
    let mut orgs: HashMap<i64, &Org> = seen.values().flatten().map(|o| (o.id, o)).collect();
    orgs.insert(org.id, org);
    let mut message = None;
    for (org_id, target) in orgs {
        let Some(url) = target.webhook_url.clone() else {
            continue;
        };
        if !sent.seqs.iter().any(|b| b.org_id == org_id) {
            continue;
        }
        let user = match st.store.user_seat(org_id).await {
            Ok(Some(u)) => u,
            _ => continue,
        };
        if !sent.seqs.iter().any(|b| b.seat_id == user.id) {
            continue;
        }
        if message.is_none() {
            message = st.store.message(sent.message_id).await.ok().flatten();
        }
        let Some(m) = &message else { return };
        let header = crate::render::header(m, target);
        let Some(slot) = webhook_slot(st, org_id, org.id) else {
            tracing::warn!(
                "webhook of org {}: notice for message {} dropped, too many in flight",
                target.name,
                m.id
            );
            continue;
        };
        crate::webhook::spawn(
            slot,
            st.http.clone(),
            st.webhook_config.clone(),
            url,
            crate::webhook::Notice {
                event: "message",
                org: target.name.clone(),
                seat: user.name.clone(),
                message: header,
            },
            st.config.webhook_private_targets,
        );
    }
}

/// A place among the webhook notices in flight of the receiving org
/// `target`: its own mail has the whole cap; mail from other orgs shares one
/// more cap, of which each sending org may hold a quarter. So a linked org's
/// burst cannot make the receiving org's own notices, or those of other
/// orgs, be dropped.
fn webhook_slot(st: &AppState, target: i64, sender: i64) -> Option<Vec<Box<dyn Send>>> {
    let l = &st.limiters;
    if target == sender {
        return Some(vec![Box::new(l.webhooks.open(target)?)]);
    }
    let from = l.webhooks_from.open((target, sender))?;
    let all = l.webhooks_external.open(target)?;
    Some(vec![Box::new(from), Box::new(all)])
}

struct Resolver<'a> {
    st: &'a AppState,
    org: &'a Org,
    sender: &'a Seat,
    /// seat id -> (delivery, seat name for events)
    seats: HashMap<i64, (NewDelivery, String)>,
    order: Vec<i64>,
    results: Vec<AddressResult>,
    rejected: Vec<Rejected>,
    to_addrs: Vec<String>,
    cc_addrs: Vec<String>,
    /// Per-send caches.
    orgs: HashMap<String, Option<Org>>,
    /// The reachability rule, with its facts for this send.
    reach: Reach<'a>,
    /// A list of the sender's org was named.
    named_list: bool,
    /// Per org on a cross-org message: the thread's participants it knew
    /// before this message (its own slice), for the notices.
    before: HashMap<i64, std::collections::HashSet<i64>>,
    /// (org id, name) -> seat, for names looked up by `prefetch`.
    seat_cache: HashMap<(i64, String), Option<Seat>>,
}

impl<'a> Resolver<'a> {
    fn new(st: &'a AppState, org: &'a Org, sender: &'a Seat, thread: Option<i64>) -> Self {
        Resolver {
            st,
            org,
            sender,
            seats: HashMap::new(),
            order: Vec::new(),
            results: Vec::new(),
            rejected: Vec::new(),
            to_addrs: Vec::new(),
            cc_addrs: Vec::new(),
            orgs: HashMap::new(),
            reach: Reach::new(st, org, sender, thread),
            named_list: false,
            before: HashMap::new(),
            seat_cache: HashMap::new(),
        }
    }

    /// Look up the seats named by all addresses with one query per org.
    async fn prefetch(&mut self, raw: impl Iterator<Item = &String>) -> ApiResult<()> {
        let mut by_org: HashMap<i64, Vec<String>> = HashMap::new();
        for a in raw {
            let Ok(addr) = a.parse::<Address>() else {
                continue;
            };
            let Some(org) = self
                .org_named(addr.org_or(&self.org.name).to_string().as_str())
                .await?
            else {
                continue;
            };
            if self.reach.linked(org.id).await? {
                by_org.entry(org.id).or_default().push(addr.name);
            }
        }
        for (org_id, names) in by_org {
            for n in &names {
                self.seat_cache.insert((org_id, n.clone()), None);
            }
            for seat in self.st.store.seats_by_names(org_id, &names).await? {
                self.seat_cache
                    .insert((org_id, seat.name.clone()), Some(seat));
            }
        }
        Ok(())
    }

    async fn seat(&mut self, org_id: i64, name: &str) -> ApiResult<Option<Seat>> {
        if let Some(s) = self.seat_cache.get(&(org_id, name.to_string())) {
            return Ok(s.clone());
        }
        Ok(self.st.store.seat_by_name(org_id, name).await?)
    }

    fn reject(&mut self, address: &str, reason: &str) {
        self.rejected.push(Rejected {
            address: address.to_string(),
            reason: reason.to_string(),
        });
    }

    fn add(
        &mut self,
        seat_id: i64,
        org_id: i64,
        name: &str,
        class: DeliveryClass,
        mode: DeliveryMode,
    ) {
        match self.seats.get_mut(&seat_id) {
            Some((d, _)) => {
                if class.rank() > d.class.rank() {
                    d.class = class;
                }
                if mode.rank() > d.mode.rank() {
                    d.mode = mode;
                }
            }
            None => {
                let d = NewDelivery {
                    seat_id,
                    org_id,
                    class,
                    mode,
                };
                self.seats.insert(seat_id, (d, name.to_string()));
                self.order.push(seat_id);
            }
        }
    }

    /// The org of that name, as the sender's org may know it: a suspended
    /// org, and a hidden org it has no active link with, count as absent
    /// (both then answer `not_linked`, like a name no org has).
    async fn org_named(&mut self, name: &str) -> ApiResult<Option<Org>> {
        if name == self.org.name {
            return Ok(Some(self.org.clone()));
        }
        if let Some(o) = self.orgs.get(name) {
            return Ok(o.clone());
        }
        let o = self
            .st
            .store
            .org_by_name(name)
            .await?
            .filter(|o| o.suspended_at.is_none());
        self.orgs.insert(name.to_string(), o.clone());
        Ok(o)
    }

    async fn resolve(&mut self, raw: &str, class: DeliveryClass) -> ApiResult<()> {
        let Ok(addr) = raw.parse::<Address>() else {
            self.reject(raw, "invalid_address");
            return Ok(());
        };
        // Do not reveal whether an unlinked org exists: the link is looked
        // up for a name no org has too.
        let target = self
            .org_named(addr.org_or(&self.org.name).to_string().as_str())
            .await?;
        let linked = self
            .reach
            .linked(target.as_ref().map_or(0, |o| o.id))
            .await?;
        let Some(target) = target.filter(|_| linked) else {
            self.reject(raw, "not_linked");
            return Ok(());
        };
        let canonical = format!("{}@{}", addr.name, target.name);
        let shown = Address {
            name: addr.name.clone(),
            org: Some(target.name.clone()),
        }
        .relative_to(&self.org.name);
        let local = target.id == self.org.id;

        let mut wakes = None;
        let count = if !local {
            // Another org: a seat the rule accepts. A list (lists cannot be
            // addressed across orgs), a seat that is neither a contact nor
            // a participant, and a name that does not exist there are
            // answered alike.
            let seat = self.seat(target.id, &addr.name).await?;
            match self.reach.check(&target, seat.as_ref()).await? {
                Verdict::Accepted => {}
                Verdict::Unreachable => {
                    self.reject(raw, "unreachable");
                    return Ok(());
                }
                Verdict::ThreadClosed => {
                    self.reject(raw, "thread_closed");
                    return Ok(());
                }
            }
            let seat = seat.expect("accepted seats exist");
            self.add(
                seat.id,
                seat.org_id,
                &seat.name,
                class,
                DeliveryMode::Immediate,
            );
            1
        } else if let Some(seat) = self.seat(target.id, &addr.name).await? {
            self.add(
                seat.id,
                seat.org_id,
                &seat.name,
                class,
                DeliveryMode::Immediate,
            );
            1
        } else if let Some(list) = self.st.store.list_by_name(target.id, &addr.name).await? {
            self.named_list = true;
            let members = self.st.store.list_members(list.id).await?;
            let allowed = match list.post_policy {
                PostPolicy::Anyone => true,
                PostPolicy::UserOnly => self.sender.kind == SeatKind::User,
                PostPolicy::Members => {
                    self.sender.kind == SeatKind::User
                        || members.iter().any(|m| m.seat_id == Some(self.sender.id))
                }
            };
            if !allowed {
                self.reject(raw, "post_denied");
                return Ok(());
            }
            let mut n = 0;
            let mut immediate = 0;
            for m in members {
                // Local members carry their seat id (memberships are removed
                // when a seat is deleted); external ones are looked up.
                let (seat_id, org_id, name) = match m.seat_id {
                    Some(id) => (id, target.id, m.address),
                    None => match self.external_member(&m.address).await? {
                        Some(s) => (s.id, s.org_id, s.name),
                        None => continue,
                    },
                };
                if seat_id != self.sender.id {
                    self.add(seat_id, org_id, &name, class, m.mode);
                    n += 1;
                    if m.mode == DeliveryMode::Immediate {
                        immediate += 1;
                    }
                }
            }
            if class == DeliveryClass::To {
                wakes = Some(immediate);
            }
            n
        } else {
            self.reject(raw, "unknown_address");
            return Ok(());
        };

        let addrs = if class == DeliveryClass::To {
            &mut self.to_addrs
        } else {
            &mut self.cc_addrs
        };
        if !addrs.contains(&canonical) {
            addrs.push(canonical);
            self.results.push(AddressResult {
                address: shown,
                seats: count,
                wakes,
                added: false,
                added_for: None,
                note: None,
            });
        }
        Ok(())
    }

    /// A list member that is not a seat of the list's org: lists hold
    /// seats of their own org only (older memberships of other orgs' seats
    /// get no mail).
    async fn external_member(&mut self, address: &str) -> ApiResult<Option<Seat>> {
        let Ok(a) = address.parse::<Address>() else {
            return Ok(None);
        };
        if a.org.as_deref().is_none_or(|o| o == self.org.name) {
            return self.seat(self.org.id, &a.name).await;
        }
        Ok(None)
    }

    /// The rules for a message that reaches other orgs: no list on it, and
    /// a contact of the sender's org for each of them (the org's primary
    /// contact is added in Cc when none is on it).
    async fn cross_org(&mut self) -> ApiResult<()> {
        let others: std::collections::BTreeSet<i64> = self
            .seats
            .values()
            .map(|(d, _)| d.org_id)
            .filter(|o| *o != self.org.id)
            .collect();
        if others.is_empty() {
            return Ok(());
        }
        if self.named_list {
            return Err(ApiError::unprocessable(
                "list_on_cross_org",
                "lists cannot be addressed on mail to other orgs: name the seats instead",
            ));
        }
        // Every org on the message, the sender's first.
        let mut orgs = vec![self.org.clone()];
        for b in &others {
            if let Some(o) = self.orgs.values().flatten().find(|o| o.id == *b) {
                orgs.push(o.clone());
            }
        }
        // The thread as the sender's org knows it: only its own slice.
        let parts = self.reach.parts().await?.clone();
        for o in &orgs {
            let seen: std::collections::HashSet<i64> =
                match (o.id == self.org.id, self.reach.thread()) {
                    (true, _) => parts.keys().copied().collect(),
                    (false, Some(t)) => self
                        .st
                        .store
                        .thread_parts(o.id, &[t])
                        .await?
                        .iter()
                        .map(|p| p.card.id)
                        .collect(),
                    (false, None) => Default::default(),
                };
            self.before.insert(o.id, seen);
        }
        // For each pair: a contact of `o` for `p` must be on the message.
        for o in orgs.clone() {
            for p in orgs.iter().filter(|p| p.id != o.id) {
                let contacts = self.reach.contacts_of(&o, p).await?.clone();
                let on = (o.id == self.org.id && contacts.has_seat_id(self.sender.id))
                    || self.seats.keys().any(|id| contacts.has_seat_id(*id));
                if on {
                    continue;
                }
                // Its contacts that already take part in the thread (all of
                // them: they joined it to be on it), else, for the sender's
                // org, its primary contact for `p`.
                let mut add: Vec<CardRec> = contacts
                    .seats
                    .values()
                    .filter(|c| parts.get(&c.id) == Some(&o.id))
                    .cloned()
                    .collect();
                if add.is_empty() && o.id == self.org.id {
                    match self.reach.primary(p).await? {
                        Some(c) => add.push(c),
                        None => {
                            return Err(ApiError::unprocessable(
                                "no_contact",
                                format!(
                                    "your org has no contact for {0}: mail to {0} must carry one of your org's seats that is open to it; the user can open one (amail open <seat> --to {0})",
                                    p.name
                                ),
                            ));
                        }
                    }
                }
                for c in add {
                    self.add(
                        c.id,
                        o.id,
                        &c.name,
                        DeliveryClass::Cc,
                        DeliveryMode::Immediate,
                    );
                    let canonical = format!("{}@{}", c.name, o.name);
                    if self.cc_addrs.contains(&canonical) || self.to_addrs.contains(&canonical) {
                        continue;
                    }
                    self.cc_addrs.push(canonical);
                    let address = Address {
                        name: c.name.clone(),
                        org: Some(o.name.clone()),
                    }
                    .relative_to(&self.org.name);
                    self.results.push(AddressResult {
                        note: Some(format!(
                            "added {}'s contact {}: every message between {} and {} carries one of each",
                            o.name, c.name, o.name, p.name
                        )),
                        address,
                        seats: 1,
                        wakes: None,
                        added: true,
                        added_for: Some(p.name.clone()),
                    });
                }
            }
        }
        Ok(())
    }

    /// After the message was stored: tell each org's user about its seats
    /// that are not contacts for another org on the message and that just
    /// became participants of the thread (once per seat and thread).
    async fn tell_new_participants(
        &mut self,
        deliveries: &[NewDelivery],
        thread: i64,
        subject: &str,
    ) {
        if let Err(e) = self.new_participants(deliveries, thread, subject).await {
            tracing::warn!("participant notices of thread {thread}: {}", e.message);
        }
    }

    async fn new_participants(
        &mut self,
        deliveries: &[NewDelivery],
        thread: i64,
        subject: &str,
    ) -> ApiResult<()> {
        let mut on: Vec<(i64, i64)> = vec![(self.sender.id, self.org.id)];
        on.extend(deliveries.iter().map(|d| (d.seat_id, d.org_id)));
        let orgs: std::collections::BTreeSet<i64> = on.iter().map(|(_, o)| *o).collect();
        if orgs.len() < 2 {
            return Ok(());
        }
        let before = self.before.clone();
        let mut by_id: HashMap<i64, Org> = self
            .orgs
            .values()
            .flatten()
            .map(|o| (o.id, o.clone()))
            .collect();
        by_id.insert(self.org.id, self.org.clone());
        let names: HashMap<i64, String> = self
            .seats
            .iter()
            .map(|(id, (_, n))| (*id, n.clone()))
            .chain(std::iter::once((self.sender.id, self.sender.name.clone())))
            .collect();
        for &o in &orgs {
            let Some(org) = by_id.get(&o).cloned() else {
                continue;
            };
            let mut lines = Vec::new();
            for &(seat, so) in &on {
                if so != o
                    || before.get(&o).is_some_and(|b| b.contains(&seat))
                    || lines.iter().any(|(s, _)| *s == seat)
                {
                    continue;
                }
                if self
                    .st
                    .store
                    .user_seat(o)
                    .await?
                    .is_some_and(|u| u.id == seat)
                {
                    continue;
                }
                let mut not_for = Vec::new();
                for &p in &orgs {
                    if p == o {
                        continue;
                    }
                    let Some(peer) = by_id.get(&p) else { continue };
                    let contacts = crate::boundary::sight(self.st, peer, Some(&org))
                        .await?
                        .remove(&org.name)
                        .unwrap_or_default();
                    if !contacts.has_seat_id(seat) {
                        not_for.push(peer.name.clone());
                    }
                }
                if !not_for.is_empty() {
                    lines.push((seat, not_for));
                }
            }
            for (seat, not_for) in lines {
                let name = names.get(&seat).cloned().unwrap_or_default();
                let peers = not_for.join(", ");
                crate::notices::tell_user(
                    self.st,
                    o,
                    &format!("Seat {name} takes part in a thread with {peers}"),
                    &format!(
                        "Seat {name} of your org is now on thread {thread} (\"{}\") together with {peers}. It is not a contact of your org for {peers}: they see its card (name, kind, description) in this thread and may address it in replies there, but not in new threads, and it does not appear among the seats your org opens to them.\n",
                        amail_proto::text::subject(subject)
                    ),
                )
                .await;
            }
        }
        Ok(())
    }
}
