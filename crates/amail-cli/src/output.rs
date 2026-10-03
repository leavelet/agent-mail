//! Compact plain-text rendering, one line per item where possible. Meant to
//! be read by agents: short, stable, greppable.

use amail_proto::{
    self as p, Delivery, DeliveryClass, DeliveryMode, InboxEntry, Message, MessageHeader, Priority,
    Source,
};

/// `2026-10-04T10:00:01.234Z` -> `2026-10-04 10:00Z` (always UTC).
pub fn short_time(ts: &str) -> String {
    match (ts.get(0..10), ts.get(11..16)) {
        (Some(d), Some(t)) => format!("{d} {t}Z"),
        _ => ts.to_string(),
    }
}

fn source(s: Source) -> &'static str {
    match s {
        Source::User => "user",
        Source::Agent => "agent",
        Source::External => "external",
    }
}

/// `#12 [to unread urgent ack] owner (user): Subject | 3 lines, thread 10, 2026-10-04 10:00Z`
pub fn entry_line(e: &InboxEntry) -> String {
    let m = &e.message;
    let d = &e.delivery;
    let mut tags = vec![d.class.as_str()];
    if d.mode != DeliveryMode::Immediate {
        tags.push(d.mode.as_str());
    }
    if d.read_at.is_none() {
        tags.push("unread");
    }
    tags.extend(header_tags(m));
    if m.ack_required && d.acked_at.is_some() {
        tags.push("acked");
    }
    format!(
        "#{} [{}] {} ({}): {} | {} line{}, thread {}, {}",
        m.id,
        tags.join(" "),
        m.from,
        source(m.source),
        m.subject,
        m.lines,
        if m.lines == 1 { "" } else { "s" },
        m.thread_id,
        short_time(&m.created_at)
    )
}

fn header_tags(m: &MessageHeader) -> Vec<&'static str> {
    let mut tags = Vec::new();
    if m.priority == Priority::Urgent {
        tags.push("urgent");
    }
    if m.ack_required {
        tags.push("ack");
    }
    tags
}

/// Mail-style headers, a blank line, then the body.
pub fn message(m: &Message) -> String {
    let h = &m.header;
    let mut out = format!(
        "#{} thread {} from {} ({}) {}\n",
        h.id,
        h.thread_id,
        h.from,
        source(h.source),
        short_time(&h.created_at)
    );
    if !h.to.is_empty() {
        out.push_str(&format!("To: {}\n", h.to.join(", ")));
    }
    if !h.cc.is_empty() {
        out.push_str(&format!("Cc: {}\n", h.cc.join(", ")));
    }
    out.push_str(&format!("Subject: {}\n", h.subject));
    if let Some(p) = h.in_reply_to {
        out.push_str(&format!("In-Reply-To: #{p}\n"));
    }
    let tags = header_tags(h);
    if !tags.is_empty() {
        out.push_str(&format!("Flags: {}\n", tags.join(" ")));
    }
    for (k, v) in &h.meta {
        out.push_str(&format!("Meta: {k}={v}\n"));
    }
    out.push('\n');
    out.push_str(&m.body);
    if !m.body.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// `a read [to] notified 10:00 via hook, read 10:01`
pub fn status_line(r: &p::RecipientStatus) -> String {
    let d: &Delivery = &r.delivery;
    let mut kind = d.class.as_str().to_string();
    if d.mode != DeliveryMode::Immediate {
        kind.push(' ');
        kind.push_str(d.mode.as_str());
    }
    let mut parts = Vec::new();
    if let Some(t) = &d.notified_at {
        let via = d
            .notified_via
            .as_deref()
            .map(|v| format!(" via {v}"))
            .unwrap_or_default();
        parts.push(format!("notified {}{}", short_time(t), via));
    }
    if let Some(t) = &d.read_at {
        parts.push(format!("read {}", short_time(t)));
    }
    if let Some(t) = &d.acked_at {
        parts.push(format!("acked {}", short_time(t)));
    }
    let detail = if parts.is_empty() {
        String::new()
    } else {
        format!(" {}", parts.join(", "))
    };
    format!("{} {} [{}]{}", r.address, r.state, kind, detail)
}

fn quoted(label: &str, v: &Option<String>) -> String {
    match v {
        Some(s) => format!(" {label}={s:?}"),
        None => String::new(),
    }
}

pub fn seat_line(s: &p::Seat) -> String {
    let mut out = format!("seat {} {}", s.name, s.kind);
    if let Some(p) = &s.parent {
        out.push_str(&format!(" parent={p}"));
    }
    if let Some(h) = &s.harness {
        out.push_str(&format!(" harness={h}"));
    }
    if let Some(m) = &s.model {
        out.push_str(&format!(" model={m}"));
    }
    if let Some(p) = &s.permission_mode {
        out.push_str(&format!(" permissions={p}"));
    }
    if let Some(g) = &s.group {
        out.push_str(&format!(" group={g}"));
    }
    if let Some(p) = &s.presence {
        out.push_str(&format!(" state={}", p.state));
        if p.wake_failed {
            out.push_str(" wake_failed");
        }
    }
    out.push_str(&quoted("name", &s.display_name));
    out.push_str(&quoted("role", &s.role));
    out
}

pub fn list_line(l: &p::List) -> String {
    format!(
        "list {} members={} policy={}{}{}",
        l.name,
        l.member_count,
        l.post_policy,
        if l.managed { " managed" } else { "" },
        quoted("description", &l.description)
    )
}

pub fn member_line(m: &p::ListMember) -> String {
    format!("member {} {}", m.address, m.mode)
}

pub fn link_line(l: &p::Link) -> String {
    format!("link {} {}", l.org, l.state)
}

pub fn send_result(r: &p::SendResponse) -> String {
    let parts: Vec<String> = r
        .recipients
        .iter()
        .map(|a| format!("{}={}", a.address, a.seats))
        .collect();
    let mut out = format!(
        "sent #{} thread {} to {} seat{}: {}",
        r.id,
        r.thread_id,
        r.deliveries,
        if r.deliveries == 1 { "" } else { "s" },
        parts.join(" ")
    );
    for rej in &r.rejected {
        out.push_str(&format!("\nrejected {}: {}", rej.address, rej.reason));
    }
    out
}

pub fn class_list(s: &str) -> Result<Vec<DeliveryClass>, String> {
    s.split(',')
        .filter(|c| !c.is_empty())
        .map(|c| c.trim().parse())
        .collect()
}

/// `3 user owner "initial user token" 2026-10-04 10:00Z`
pub fn token_line(t: &p::TokenInfo) -> String {
    let mut out = format!("{} {}", t.id, t.scope);
    if let Some(s) = &t.seat {
        out.push_str(&format!(" {s}"));
    }
    if let Some(l) = &t.label {
        out.push_str(&format!(" {l:?}"));
    }
    out.push_str(&format!(" {}", short_time(&t.created_at)));
    out
}
