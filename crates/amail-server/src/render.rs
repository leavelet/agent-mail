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

/// The beginning of `body` with its lines joined by spaces, cut to `max`
/// characters (a preview of the first lines).
pub fn lead(body: &str, max: usize) -> String {
    let text = body
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !c.is_control()).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
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
        body_omitted: false,
    }
}

/// Size of `v` as JSON.
pub fn json_len<T: serde::Serialize>(v: &T) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX)
}

/// Cuts `items` so that their JSON stays within `max` bytes, keeping at
/// least one item: from the start of the list (`keep_last` false) or from
/// its end. Returns whether items were dropped.
pub fn fit<T: serde::Serialize>(items: &mut Vec<T>, max: usize, keep_last: bool) -> bool {
    let mut total = 0usize;
    let mut keep = 0;
    let n = items.len();
    for i in 0..n {
        let item = if keep_last {
            &items[n - 1 - i]
        } else {
            &items[i]
        };
        // One more byte for the separating comma.
        total = total.saturating_add(json_len(item) + 1);
        if total > max && keep > 0 {
            break;
        }
        keep += 1;
    }
    if keep == n {
        return false;
    }
    if keep_last {
        items.drain(..n - keep);
    } else {
        items.truncate(keep);
    }
    true
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

/// A seat of the viewer's own org. `open_to`: the audiences it is open
/// to (see [`open_to`]).
pub fn seat(s: &Seat, presence: Option<p::Presence>, open_to: Vec<String>) -> p::Seat {
    p::Seat {
        name: s.name.clone(),
        kind: s.kind,
        parent: s.parent_name.clone(),
        display_name: s.display_name.clone(),
        role: s.role.clone(),
        role_truncated: false,
        harness: s.harness.clone(),
        model: s.model.clone(),
        permission_mode: s.permission_mode,
        group: s.group.clone(),
        description: s.description.clone(),
        contact: s.contact,
        open_to,
        created_at: ts(s.created_at),
        presence,
        focus_until: s.focus_until.filter(|t| *t > amail_store::now_ms()).map(ts),
    }
}

/// A seat as listed by `GET /seats`: its role cut to
/// [`p::limits::LIST_ROLE_CHARS`] characters.
pub fn seat_in_list(s: &Seat, presence: Option<p::Presence>, open_to: Vec<String>) -> p::Seat {
    let mut out = seat(s, presence, open_to);
    if let Some(r) = &mut out.role
        && let Some((i, _)) = r.char_indices().nth(p::limits::LIST_ROLE_CHARS)
    {
        r.truncate(i);
        out.role_truncated = true;
    }
    out
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
        kept_bytes: o.kept_bytes,
        storage_quota_bytes: storage_quota,
        display_name: o.display_name.clone(),
        description: o.description.clone(),
        visibility: o.visibility,
        outbound: o.outbound,
        new_threads_per_hour: o.new_threads_per_hour,
        notice: o.notice.clone(),
    }
}

/// The audience of a grant as shown: `linked` or the org's name.
pub fn audience(a: Option<&str>) -> String {
    a.unwrap_or("linked").to_string()
}

/// The audiences each seat of the org is open to, by seat name: from
/// grants on the seat and on the team it is a contact of.
pub fn open_to(grants: &[amail_store::GrantRec]) -> std::collections::HashMap<String, Vec<String>> {
    use amail_store::GrantTarget;
    let mut out: std::collections::HashMap<String, Vec<String>> = Default::default();
    for g in grants {
        let names: Vec<&str> = match &g.target {
            GrantTarget::Seat(s) => vec![s.name.as_str()],
            GrantTarget::Team { contacts, .. } => {
                contacts.iter().map(|c| c.name.as_str()).collect()
            }
        };
        for n in names {
            let v = out.entry(n.to_string()).or_default();
            let a = audience(g.audience.as_deref());
            if !v.contains(&a) {
                v.push(a);
            }
        }
    }
    for v in out.values_mut() {
        v.sort();
    }
    out
}

/// A grant of the viewer's org. `active`: whether its audience (a named
/// org) has an active link with the org.
pub fn grant(g: &amail_store::GrantRec, active: bool) -> p::Grant {
    use amail_store::GrantTarget;
    let (target, kind) = match &g.target {
        GrantTarget::Seat(s) => (
            s.name.clone(),
            if s.kind == Some(amail_proto::SeatKind::User) {
                p::CardKind::User
            } else {
                p::CardKind::Agent
            },
        ),
        GrantTarget::Team { team, .. } => (team.name.clone(), p::CardKind::Team),
    };
    p::Grant {
        id: g.id,
        target,
        kind,
        audience: audience(g.audience.as_deref()),
        active,
        managed: g.managed,
        created_at: ts(g.created_at),
    }
}

/// A link as the org sees it.
pub fn link(l: &amail_store::Link) -> p::Link {
    p::Link {
        org: l.peer_name.clone(),
        state: l.state,
        since: Some(ts(l.since)),
        note: l.note.clone(),
        primary: None,
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fit_cuts_by_json_size() {
        // Each item is 12 bytes as JSON ("aaaaaaaaaa") plus a comma.
        let items = || vec!["a".repeat(10); 5];
        let mut v = items();
        assert!(!fit(&mut v, 100, false));
        assert_eq!(v.len(), 5);
        let mut v: Vec<String> = (0..5).map(|i| format!("{i}{}", "a".repeat(9))).collect();
        assert!(fit(&mut v, 30, false));
        assert_eq!(v, vec!["0aaaaaaaaa", "1aaaaaaaaa"]);
        let mut v: Vec<String> = (0..5).map(|i| format!("{i}{}", "a".repeat(9))).collect();
        assert!(fit(&mut v, 30, true));
        assert_eq!(v, vec!["3aaaaaaaaa", "4aaaaaaaaa"]);
        // At least one item, however large.
        let mut v = items();
        assert!(fit(&mut v, 1, false));
        assert_eq!(v.len(), 1);
        let mut v: Vec<String> = Vec::new();
        assert!(!fit(&mut v, 1, false));
    }
}
