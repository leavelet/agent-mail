//! Store records to wire types, relative to the viewing org.

use amail_proto as p;
use amail_proto::{Address, SeatKind, Source};
use amail_store::{DeliveryRec, Entry, List, MessageRec, Org, Seat};

pub fn ts(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_string())
        .unwrap_or_default()
}

pub fn parse_ts(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

/// `name` if `org` is the viewer's org, otherwise `name@org`.
pub fn addr(name: &str, org: &str, viewer: &Org) -> String {
    if org == viewer.name {
        name.to_string()
    } else {
        format!("{name}@{org}")
    }
}

fn rel(stored: &str, viewer: &Org) -> String {
    match stored.parse::<Address>() {
        Ok(a) => a.relative_to(&viewer.name),
        Err(_) => stored.to_string(),
    }
}

pub fn source(m: &MessageRec, viewer: &Org) -> Source {
    if m.org_id != viewer.id {
        Source::External
    } else {
        match m.sender_kind {
            SeatKind::User => Source::User,
            SeatKind::System => Source::System,
            SeatKind::Agent => Source::Agent,
        }
    }
}

/// The first non-empty line of `body`, on one line, cut to `max` characters.
pub fn preview(body: &str, max: usize) -> String {
    let line = body
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

pub fn header(m: &MessageRec, viewer: &Org) -> p::MessageHeader {
    p::MessageHeader {
        id: m.id,
        thread_id: m.thread_id,
        in_reply_to: m.parent_id,
        from: addr(&m.sender_name, &m.org_name, viewer),
        source: source(m, viewer),
        to: m.to_addrs.iter().map(|a| rel(a, viewer)).collect(),
        cc: m.cc_addrs.iter().map(|a| rel(a, viewer)).collect(),
        subject: m.subject.clone(),
        priority: m.priority,
        ack_required: m.ack_required,
        lines: m.lines,
        created_at: ts(m.created_at),
        meta: m.meta.clone(),
        expired: m.expired_at.is_some(),
    }
}

pub fn message(m: &MessageRec, viewer: &Org) -> p::Message {
    p::Message {
        header: header(m, viewer),
        body: m.body.clone(),
    }
}

pub fn delivery(d: &DeliveryRec, viewer: &Org) -> p::Delivery {
    p::Delivery {
        seat: addr(&d.seat_name, &d.org_name, viewer),
        seq: d.seq,
        mod_seq: d.mod_seq,
        class: d.class,
        mode: d.mode,
        notified_at: d.notified_at.map(ts),
        notified_via: d.notified_via.clone(),
        read_at: d.read_at.map(ts),
        acked_at: d.acked_at.map(ts),
    }
}

pub fn entry(e: &Entry, viewer: &Org) -> p::InboxEntry {
    p::InboxEntry {
        delivery: delivery(&e.delivery, viewer),
        message: header(&e.message, viewer),
        body: None,
        preview: None,
    }
}

pub fn seat(s: &Seat, presence: Option<p::Presence>) -> p::Seat {
    p::Seat {
        name: s.name.clone(),
        kind: s.kind,
        parent: s.parent_name.clone(),
        display_name: s.display_name.clone(),
        role: s.role.clone(),
        harness: s.harness.clone(),
        model: s.model.clone(),
        permission_mode: s.permission_mode,
        group: s.group.clone(),
        created_at: ts(s.created_at),
        presence,
        focus_until: s.focus_until.filter(|t| *t > amail_store::now_ms()).map(ts),
    }
}

pub fn list(l: &List, members: Option<Vec<p::ListMember>>) -> p::List {
    p::List {
        name: l.name.clone(),
        description: l.description.clone(),
        post_policy: l.post_policy,
        member_count: l.member_count,
        managed: l.managed,
        members,
    }
}

/// Bytes in MiB, for messages: `1.5 MiB`.
pub fn mib(bytes: i64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

pub fn org(o: &Org, storage_quota: Option<i64>) -> p::OrgInfo {
    p::OrgInfo {
        name: o.name.clone(),
        send_rate_per_min: o.send_rate_per_min,
        send_burst: o.send_burst,
        max_recipients: o.max_recipients,
        max_thread_depth: o.max_thread_depth,
        ack_timeout_min: o.ack_timeout_min,
        focus_max_min: o.focus_max_min,
        digest_interval_min: o.digest_interval_min,
        webhook_url: o.webhook_url.clone(),
        stored_bytes: o.stored_bytes,
        storage_quota_bytes: storage_quota,
    }
}
