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
//! - `name@org` for another org requires an active link. It resolves to a
//!   seat, or to a list whose `post_policy` is `anyone`.
//! - List members are seats: local ones, or external `name@org` seats. An
//!   external member is delivered only if its org is the sender's org or is
//!   linked to the sender's org, so mail never crosses between unlinked orgs.
//!   Expansion is one level deep; there are no nested lists.
//! - A seat reached through several addresses gets one delivery with the
//!   highest class (`to` > `cc`) and the most immediate mode.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use amail_proto::{
    Address, AddressResult, DeliveryClass, DeliveryMode, PostPolicy, Priority, Rejected, SeatKind,
    SendRequest, SendResponse,
};
use amail_store::{NewDelivery, NewMessage, Org, Seat};
use std::collections::HashMap;

pub async fn send(st: &AppState, auth: &Auth, req: SendRequest) -> ApiResult<SendResponse> {
    let sender = auth.mail_seat()?.clone();
    let org = &auth.org;
    if req.to.is_empty() && req.cc.is_empty() {
        return Err(ApiError::bad_request("at least one recipient is required"));
    }
    check_sizes(&req)?;
    let limited = sender.kind != SeatKind::User;
    if limited {
        st.limiter
            .try_acquire(sender.id, org.send_rate_per_min, org.send_burst)
            .map_err(|wait| ApiError::rate_limited(wait.as_millis().max(1) as u64))?;
    }
    let result = send_inner(st, auth, &sender, req).await;
    if result.is_err() && limited {
        st.limiter.refund(sender.id, org.send_burst);
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

async fn send_inner(
    st: &AppState,
    auth: &Auth,
    sender: &Seat,
    req: SendRequest,
) -> ApiResult<SendResponse> {
    let org = &auth.org;

    // Thread inheritance and the hop limit.
    let (thread_id, parent_id, depth) = match req.in_reply_to {
        None => (None, None, 0),
        Some(pid) => {
            let parent = st.store.message(pid).await?;
            let parent = match parent {
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

    let mut r = Resolver::new(st, org, sender);
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
    let msg = NewMessage {
        org_id: org.id,
        sender_seat_id: sender.id,
        thread_id,
        parent_id,
        depth,
        subject: req.subject,
        body: req.body,
        to_addrs: r.to_addrs,
        cc_addrs: r.cc_addrs,
        priority: req.priority.unwrap_or(Priority::Normal),
        ack_required: req.ack_required.unwrap_or(false),
        meta: req.meta,
        deliveries,
    };
    let sent = st.store.send(&msg).await?;

    let mut by_org: HashMap<i64, Vec<(String, i64)>> = HashMap::new();
    for b in &sent.seqs {
        let name = r.seats[&b.seat_id].1.clone();
        by_org.entry(b.org_id).or_default().push((name, b.seq));
    }
    for (org_id, events) in &by_org {
        st.bus.publish(*org_id, events);
    }
    notify_users(st, org, &r.orgs, &sent).await;

    Ok(SendResponse {
        id: sent.message_id,
        thread_id: sent.thread_id,
        recipients: r.results,
        rejected: r.rejected,
        deliveries: sent.seqs.len() as u32,
    })
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
        crate::webhook::spawn(
            st.http.clone(),
            st.webhook_config.clone(),
            url,
            crate::webhook::Notice {
                event: "message",
                org: target.name.clone(),
                seat: user.name.clone(),
                message: crate::render::header(m, target),
            },
        );
    }
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
    links: HashMap<i64, bool>,
    /// (org id, name) -> seat, for names looked up by `prefetch`.
    seat_cache: HashMap<(i64, String), Option<Seat>>,
}

impl<'a> Resolver<'a> {
    fn new(st: &'a AppState, org: &'a Org, sender: &'a Seat) -> Self {
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
            links: HashMap::new(),
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
            if self.reachable(org.id).await? {
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

    async fn org_named(&mut self, name: &str) -> ApiResult<Option<Org>> {
        if name == self.org.name {
            return Ok(Some(self.org.clone()));
        }
        if let Some(o) = self.orgs.get(name) {
            return Ok(o.clone());
        }
        let o = self.st.store.org_by_name(name).await?;
        self.orgs.insert(name.to_string(), o.clone());
        Ok(o)
    }

    /// Whether mail from the sender's org may reach `org_id`.
    async fn reachable(&mut self, org_id: i64) -> ApiResult<bool> {
        if org_id == self.org.id {
            return Ok(true);
        }
        if let Some(&b) = self.links.get(&org_id) {
            return Ok(b);
        }
        let b = self.st.store.link_active(self.org.id, org_id).await?;
        self.links.insert(org_id, b);
        Ok(b)
    }

    async fn resolve(&mut self, raw: &str, class: DeliveryClass) -> ApiResult<()> {
        let Ok(addr) = raw.parse::<Address>() else {
            self.reject(raw, "invalid_address");
            return Ok(());
        };
        let Some(target) = self
            .org_named(addr.org_or(&self.org.name).to_string().as_str())
            .await?
        else {
            // Do not reveal whether an unlinked org exists.
            self.reject(raw, "not_linked");
            return Ok(());
        };
        if !self.reachable(target.id).await? {
            self.reject(raw, "not_linked");
            return Ok(());
        }
        let canonical = format!("{}@{}", addr.name, target.name);
        let shown = Address {
            name: addr.name.clone(),
            org: Some(target.name.clone()),
        }
        .relative_to(&self.org.name);
        let local = target.id == self.org.id;

        let count = if let Some(seat) = self.seat(target.id, &addr.name).await? {
            self.add(
                seat.id,
                seat.org_id,
                &seat.name,
                class,
                DeliveryMode::Immediate,
            );
            1
        } else if let Some(list) = self.st.store.list_by_name(target.id, &addr.name).await? {
            let members = self.st.store.list_members(list.id).await?;
            let allowed = match list.post_policy {
                PostPolicy::Anyone => true,
                _ if !local => false,
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
                }
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
            });
        }
        Ok(())
    }

    /// Resolve an external list member (`name@org`) to a seat, if the
    /// sender's org may reach it.
    async fn external_member(&mut self, address: &str) -> ApiResult<Option<Seat>> {
        let Ok(a) = address.parse::<Address>() else {
            return Ok(None);
        };
        let Some(org) = self
            .org_named(a.org_or(&self.org.name).to_string().as_str())
            .await?
        else {
            return Ok(None);
        };
        if !self.reachable(org.id).await? {
            return Ok(None);
        }
        self.seat(org.id, &a.name).await
    }
}
