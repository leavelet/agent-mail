//! `POST /v1/node/notices`: events a node reports so that the org's person
//! is told about them by system mail (see [`amail_proto::NodeNotice`]).
//!
//! The node sends typed fields only; the text of the mail is fixed here.
//! Setting values must be ones the setting can have, seat names must be
//! seats of the node's own org, and the node's label is escaped and shown
//! in quotes. The mail goes only to the user seat of the node's own org,
//! from the org's hidden system seat, asks for no acknowledgement and wakes
//! no agent. Reports are limited per org
//! ([`amail_proto::limits::NODE_NOTICES_PER_HOUR`]).
//!
//! One report becomes at most one mail, whose body is at most
//! [`MAX_NOTICE_BODY_BYTES`]: items that do not fit are left out and
//! counted. Notice mail is not charged to the org's storage quota and never
//! refused at it; instead each org may receive [`NOTICE_MAILS_PER_DAY`]
//! mails and [`NOTICE_BYTES_PER_DAY`] bytes of them in 24 hours (counted in
//! the database), further reports are dropped, and notice mail is deleted
//! after [`amail_proto::limits::NOTICE_RETENTION_DAYS`].
//!
//! A notice about failing turns names its seats in the mail's meta field
//! [`TURN_FAILURES_META`]. When a presence report clears a seat's turn
//! failure, [`turns_recovered`] marks such notices read for the user once
//! none of their seats fails any more, so that they leave "needs you"; the
//! mail stays in the mailbox and the archive.

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json};
use amail_proto::limits::{
    MAX_NODE_LABEL_CHARS, MAX_NODE_NOTICES, MAX_NOTICE_BODY_BYTES, MAX_NOTICE_ITEMS,
    MAX_NOTICE_TARGETS, NOTICE_BYTES_PER_DAY, NOTICE_MAILS_PER_DAY,
};
use amail_proto::{
    AgentAction, DeliveryClass, DeliveryMode, LocalPolicy, NodeNotice, NodeNotices,
    NodeNoticesAccepted, NoticeOp, NoticeSetting, PermissionMode, Priority, SeatKind,
    SettingChange, TokenScope,
};
use amail_store::{NewDelivery, NewMessage};
use axum::extract::State;
use std::collections::HashSet;
use std::str::FromStr;

/// Meta field of a notice mail about failing turns: the names of the
/// seats (of the org) it is about, separated by spaces.
pub const TURN_FAILURES_META: &str = "turn_failures";

/// Bytes of seat names kept in [`TURN_FAILURES_META`] (the whole meta of a
/// message is at most [`amail_proto::limits::MAX_META_BYTES`]).
const TURN_FAILURES_META_BYTES: usize = 2048;

/// How many of the user's newest unread deliveries [`turns_recovered`]
/// looks at.
const RECOVERED_SCAN: i64 = 200;

/// Values of a Codex seat's `git_access`.
const GIT_ACCESS: &[&str] = &["worktree", "full", "off"];

fn valid_value(setting: NoticeSetting, v: &str) -> bool {
    match setting {
        NoticeSetting::LocalPolicy => LocalPolicy::from_str(v).is_ok(),
        NoticeSetting::PermissionMode => PermissionMode::from_str(v).is_ok(),
        NoticeSetting::GitAccess => GIT_ACCESS.contains(&v),
        NoticeSetting::DshJail => matches!(v, "on" | "off"),
    }
}

/// The node's label as shown in a notice: one line, escaped, at most
/// [`MAX_NODE_LABEL_CHARS`] characters, without double quotes (it is shown
/// in them).
fn host(label: Option<&str>) -> String {
    match label.map(str::trim).filter(|l| !l.is_empty()) {
        None => "(no label)".to_string(),
        Some(l) => {
            let l: String = amail_proto::text::line(l)
                .chars()
                .map(|c| if c == '"' { '\'' } else { c })
                .take(MAX_NODE_LABEL_CHARS)
                .collect();
            format!("\"{l}\"")
        }
    }
}

fn op_text(op: NoticeOp) -> &'static str {
    match op {
        NoticeOp::Up => "start or update seats (amail up)",
        NoticeOp::Down => "stop seats (amail down)",
        NoticeOp::Forget => "stop and forget seats (amail forget)",
        NoticeOp::Run => {
            "launch a session for a seat, which gives it that seat's credentials (amail run)"
        }
        NoticeOp::Wake => "wake seats (amail wake)",
        NoticeOp::Sleep => "put seats to sleep (amail sleep)",
        NoticeOp::Credentials => "obtain a seat's mail credentials",
        NoticeOp::Hook => {
            "send a hook in another seat's name, which reads and claims that seat's mail"
        }
    }
}

fn setting_text(s: NoticeSetting) -> &'static str {
    match s {
        NoticeSetting::LocalPolicy => "local policy",
        NoticeSetting::PermissionMode => "permission mode",
        NoticeSetting::GitAccess => "git access",
        NoticeSetting::DshJail => "dsh jail",
    }
}

/// Bytes of the seat names shown for one action; further names are
/// counted.
const MAX_NAMES_BYTES: usize = 400;

/// `names` as shown: seats of the org by name, up to [`MAX_NAMES_BYTES`];
/// other names are counted.
fn names_text(names: &[String], seats: &HashSet<String>, count: u32) -> String {
    let known: Vec<&str> = names
        .iter()
        .filter(|n| seats.contains(*n))
        .map(String::as_str)
        .collect();
    let unknown = names.len() - known.len();
    let mut shown = Vec::new();
    let mut len = 0;
    for n in &known {
        if len + n.len() + 2 > MAX_NAMES_BYTES {
            break;
        }
        len += n.len() + 2;
        shown.push(*n);
    }
    let mut out = if shown.is_empty() {
        String::new()
    } else {
        format!(": {}", shown.join(", "))
    };
    let hidden = known.len() - shown.len();
    if hidden > 0 {
        out.push_str(&format!(
            " (and {hidden} more seat{})",
            if hidden == 1 { "" } else { "s" }
        ));
    }
    if unknown > 0 {
        out.push_str(&format!(
            " ({unknown} name{} that {} not a seat of this org)",
            if unknown == 1 { "" } else { "s" },
            if unknown == 1 { "is" } else { "are" }
        ));
    }
    if count > 1 {
        out.push_str(&format!(" ({count} requests)"));
    }
    out
}

/// One notice as shown: its subject, a first paragraph, list items and
/// closing paragraphs, all from fixed sentences and typed values.
struct Section {
    subject: String,
    intro: String,
    items: Vec<String>,
    outro: String,
}

/// The parts of the mail for `n`.
fn render(n: &NodeNotice, host: &str, seats: &HashSet<String>) -> Section {
    match n {
        NodeNotice::Settings { changes } => {
            let mut items = Vec::new();
            let mut opened = false;
            let mut unjailed = false;
            for c in changes {
                let what = match c.seat.as_deref() {
                    Some(s) if seats.contains(s) => {
                        format!("seat {s}, {}", setting_text(c.setting))
                    }
                    Some(_) => format!(
                        "a seat that is not in this org, {}",
                        setting_text(c.setting)
                    ),
                    None => setting_text(c.setting).to_string(),
                };
                let old = c.old.as_deref().unwrap_or("not reported before");
                items.push(format!("- {what}: {old} -> {}", c.new));
                opened |= c.setting == NoticeSetting::LocalPolicy && c.new == "open";
                unjailed |= c.setting == NoticeSetting::DshJail && c.new == "off";
            }
            let mut outro = String::new();
            if opened {
                outro.push_str(
                    "With the local policy open, every process of your OS user on that host, \
                     including the agents of the sessions the node started, may manage seats \
                     and obtain any seat's credentials through the node. The node reports \
                     such requests from agent sessions to you. Set local_policy = \
                     \"restricted\" to refuse them (see docs/security.md).\n",
                );
            }
            if unjailed {
                if !outro.is_empty() {
                    outro.push('\n');
                }
                outro.push_str(
                    "With the dsh jail off, an unattended DeepSeek Harness seat runs without \
                     the bubblewrap jail: its agent can read every file your OS user can read \
                     on that host, saved logins and keys included (see docs/harnesses.md).\n",
                );
            }
            Section {
                subject: format!("Node settings changed on host {host}"),
                intro: format!(
                    "The amail node on host {host} now runs with settings that differ from the \
                     ones it reported before:"
                ),
                items,
                outro,
            }
        }
        NodeNotice::AgentActions { seat, actions } => {
            let who = match seat.as_deref() {
                Some(s) if seats.contains(s) => format!("seat {s}"),
                Some(_) => "a seat that is not in this org".to_string(),
                None => "a Codex seat (Codex seats of one node share one process, so the \
                         node cannot tell which)"
                    .to_string(),
            };
            let items = actions
                .iter()
                .map(|a| {
                    format!(
                        "- {}{}",
                        op_text(a.op),
                        names_text(&a.targets, seats, a.count)
                    )
                })
                .collect();
            let subject = match seat.as_deref() {
                Some(s) if seats.contains(s) => {
                    format!("Agent seat {s} used the node on host {host}")
                }
                _ => format!("An agent session used the node on host {host}"),
            };
            Section {
                subject,
                intro: format!(
                    "A process of the session of {who} on host {host} asked the amail node to:"
                ),
                items,
                outro: "The node's local policy (open) allowed it. If you did not expect this, \
                        look at that session. With local_policy = \"restricted\" the node \
                        refuses such requests from agent sessions (see docs/security.md). This \
                        notice is best effort: the node recognizes a session's processes by \
                        their parent processes, and a process that left the session's process \
                        tree is not recognized.\n"
                    .to_string(),
            }
        }
        NodeNotice::TurnFailures {
            seat,
            count,
            harness,
            error,
        } => {
            let who = if seats.contains(seat) {
                format!("seat {seat}")
            } else {
                "a seat that is not in this org".to_string()
            };
            let harness = harness
                .as_deref()
                .filter(|h| matches!(*h, "claude" | "codex" | "dsh"))
                .map(|h| format!(" ({h})"))
                .unwrap_or_default();
            let error: String = amail_proto::text::line(error)
                .chars()
                .take(amail_proto::limits::MAX_HINT_CHARS)
                .collect();
            // Seat names that are not of this org are not shown.
            let named = seats.contains(seat);
            let subject = if named {
                format!("Turns of seat {seat} are failing on host {host}")
            } else {
                format!("Turns of a seat are failing on host {host}")
            };
            let (attach, wake) = if named {
                (format!("amail attach {seat}"), format!("amail wake {seat}"))
            } else {
                ("amail attach".to_string(), "amail wake".to_string())
            };
            Section {
                subject,
                intro: format!(
                    "The last {count} turns of {who}{harness} on host {host} failed. The node \
                     has stopped showing the mail of those turns to the seat again; the mail \
                     is marked not read, and the seat gets it with its next turn. The \
                     harness's last error (as it reported it):"
                ),
                items: vec![format!("    {error}")],
                outro: format!(
                    "Look at the seat's session ({attach}, or the seat's conversation in the \
                     web interface). When the cause is fixed, wake the seat ({wake}); a turn \
                     that ends normally clears the failure.\n"
                ),
            }
        }
    }
}

/// Room kept at the end of the body for the lines that count what was left
/// out.
const RESERVE: usize = 200;

/// Subject and body of the one mail for a report: the notices in order,
/// within [`MAX_NOTICE_BODY_BYTES`]. Items and whole notices that do not
/// fit are left out and counted.
fn compose(sections: &[Section], host: &str) -> (String, String) {
    let subject = match sections {
        [one] => one.subject.clone(),
        _ => format!(
            "{} notices from the amail node on host {host}",
            sections.len()
        ),
    };
    let mut body = String::new();
    let mut left = 0;
    for (i, s) in sections.iter().enumerate() {
        let sep = if body.is_empty() { "" } else { "\n-----\n\n" };
        if body.len() + sep.len() + s.intro.len() + s.outro.len() + 3 + RESERVE
            > MAX_NOTICE_BODY_BYTES
        {
            left = sections.len() - i;
            break;
        }
        body.push_str(sep);
        body.push_str(&s.intro);
        body.push_str("\n\n");
        let mut shown = 0;
        for item in &s.items {
            if body.len() + item.len() + 1 + s.outro.len() + 1 + RESERVE > MAX_NOTICE_BODY_BYTES {
                break;
            }
            body.push_str(item);
            body.push('\n');
            shown += 1;
        }
        if shown < s.items.len() {
            body.push_str(&format!("- ({} more not shown)\n", s.items.len() - shown));
        }
        if !s.outro.is_empty() {
            body.push('\n');
            body.push_str(&s.outro);
        }
    }
    if left > 0 {
        body.push_str(&format!(
            "\n-----\n\n{left} more notice{} of this report {} not shown.\n",
            if left == 1 { "" } else { "s" },
            if left == 1 { "is" } else { "are" }
        ));
    }
    let subject = subject
        .chars()
        .take(amail_proto::limits::MAX_SUBJECT_CHARS)
        .collect();
    (subject, body)
}

fn check(report: &NodeNotices) -> ApiResult<()> {
    crate::directory::check_text("node", report.node.as_deref(), MAX_NODE_LABEL_CHARS)?;
    if report.notices.len() > MAX_NODE_NOTICES {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_NODE_NOTICES} notices per report"
        )));
    }
    for n in &report.notices {
        match n {
            NodeNotice::Settings { changes } => {
                if changes.is_empty() || changes.len() > MAX_NOTICE_ITEMS {
                    return Err(ApiError::bad_request(format!(
                        "a settings notice lists 1 to {MAX_NOTICE_ITEMS} changes"
                    )));
                }
                for SettingChange {
                    setting, old, new, ..
                } in changes
                {
                    if !valid_value(*setting, new)
                        || old.as_deref().is_some_and(|o| !valid_value(*setting, o))
                    {
                        return Err(ApiError::bad_request(format!("not a value of {setting}")));
                    }
                }
            }
            NodeNotice::AgentActions { actions, .. } => {
                if actions.is_empty() || actions.len() > MAX_NOTICE_ITEMS {
                    return Err(ApiError::bad_request(format!(
                        "an actions notice lists 1 to {MAX_NOTICE_ITEMS} actions"
                    )));
                }
                for AgentAction { targets, .. } in actions {
                    if targets.len() > MAX_NOTICE_TARGETS {
                        return Err(ApiError::bad_request(format!(
                            "at most {MAX_NOTICE_TARGETS} seats per action"
                        )));
                    }
                }
            }
            NodeNotice::TurnFailures {
                seat,
                harness,
                error,
                ..
            } => {
                if !amail_proto::valid_name(seat) {
                    return Err(ApiError::bad_request("not a seat name"));
                }
                crate::directory::check_text("harness", harness.as_deref(), 32)?;
                // Cut when shown; refuse only what is far too long.
                crate::directory::check_text("error", Some(error), 4000)?;
            }
        }
    }
    Ok(())
}

/// `POST /v1/node/notices` (node tokens).
pub async fn post(
    State(st): State<AppState>,
    auth: Auth,
    Json(report): Json<NodeNotices>,
) -> ApiResult<Json<NodeNoticesAccepted>> {
    if auth.scope != TokenScope::Node {
        return Err(ApiError::forbidden("requires a node token"));
    }
    check(&report)?;
    if report.notices.is_empty() {
        return Ok(Json(NodeNoticesAccepted { sent: 0 }));
    }
    if let Err(ms) = st.limiters.node_notices.take(&auth.org.id) {
        return Err(ApiError::too_many(
            "rate_limited",
            "too many node notices from this org",
            ms,
        ));
    }
    let org = auth.org.id;
    let all = st.store.seats(org).await?;
    let user = all
        .iter()
        .find(|s| s.kind == SeatKind::User)
        .cloned()
        .ok_or_else(|| ApiError::not_found("the org has no user seat"))?;
    let seats: HashSet<String> = all
        .iter()
        .filter(|s| s.kind != SeatKind::System)
        .map(|s| s.name.clone())
        .collect();
    let host = host(report.node.as_deref());
    let sections: Vec<Section> = report
        .notices
        .iter()
        .map(|n| render(n, &host, &seats))
        .collect();
    let (subject, body) = compose(&sections, &host);
    let mut meta = amail_proto::Meta::new();
    let mut failing: Vec<&str> = Vec::new();
    for n in &report.notices {
        if let NodeNotice::TurnFailures { seat, .. } = n
            && seats.contains(seat)
            && !failing.contains(&seat.as_str())
            && failing.iter().map(|s| s.len() + 1).sum::<usize>() + seat.len()
                <= TURN_FAILURES_META_BYTES
        {
            failing.push(seat);
        }
    }
    if !failing.is_empty() {
        meta.insert(TURN_FAILURES_META.to_string(), failing.join(" "));
    }
    // The org's daily budget, counted from the notice mail it holds; one
    // report at a time per instance, so that the count is not read twice
    // before a send.
    let turn = st.limiters.notice_budget.lock().await;
    let since = amail_store::now_ms() - 86_400_000;
    let (mails, bytes) = st.store.notice_usage(org, since).await?;
    if mails >= NOTICE_MAILS_PER_DAY
        || bytes + (subject.len() + body.len()) as i64 > NOTICE_BYTES_PER_DAY
    {
        // Answered as taken, so that the node does not send it again.
        tracing::warn!(
            "org {org}: node notice report dropped, over the daily notice budget \
             ({mails} mails, {bytes} bytes in 24 hours)"
        );
        return Ok(Json(NodeNoticesAccepted { sent: 0 }));
    }
    let system = st.store.system_seat(org).await?;
    let m = st
        .store
        .send(&NewMessage {
            org_id: org,
            sender_seat_id: system.id,
            thread_id: None,
            parent_id: None,
            depth: 0,
            subject,
            body,
            to_addrs: vec![format!("{}@{}", user.name, auth.org.name)],
            cc_addrs: Vec::new(),
            priority: Priority::Normal,
            ack_required: false,
            meta,
            deliveries: vec![NewDelivery {
                seat_id: user.id,
                org_id: org,
                class: DeliveryClass::To,
                mode: DeliveryMode::Immediate,
            }],
            ack_due: None,
            // Notice mail is never refused at the storage quota and is not
            // charged to it; the daily budget above and its own retention
            // bound it.
            storage_quota: None,
            notice: true,
        })
        .await?;
    drop(turn);
    let events: Vec<(String, i64)> = m
        .seqs
        .iter()
        .filter(|b| b.seat_id == user.id)
        .map(|b| (user.name.clone(), b.seq))
        .collect();
    st.bus.publish(org, &events);
    tracing::info!(
        "org {org}: {} node notice(s) sent to the user seat in one mail",
        report.notices.len()
    );
    Ok(Json(NodeNoticesAccepted { sent: 1 }))
}

/// A presence report cleared the turn failure of the seats `cleared`: mark
/// read the user's unread notices about failing turns (among its newest
/// [`RECOVERED_SCAN`] unread deliveries) that name one of them, once no seat
/// they name has a turn failure. Best effort: errors are logged.
pub async fn turns_recovered(st: &AppState, org_id: i64, cleared: &[String]) {
    if let Err(e) = mark_recovered(st, org_id, cleared).await {
        tracing::warn!("org {org_id}: marking turn-failure notices read: {e}");
    }
}

async fn mark_recovered(st: &AppState, org_id: i64, cleared: &[String]) -> amail_store::Result<()> {
    if cleared.is_empty() {
        return Ok(());
    }
    let Some(user) = st.store.user_seat(org_id).await? else {
        return Ok(());
    };
    let entries = st
        .store
        .inbox(
            user.id,
            &amail_store::InboxQuery {
                unread_only: true,
                after_seq: None,
                before_seq: Some(i64::MAX),
                limit: RECOVERED_SCAN,
            },
        )
        .await?;
    let notices: Vec<(i64, Vec<&str>)> = entries
        .iter()
        .filter(|e| e.message.sender_kind == SeatKind::System)
        .filter_map(|e| {
            let names: Vec<&str> = e
                .message
                .meta
                .get(TURN_FAILURES_META)?
                .split_whitespace()
                .collect();
            names
                .iter()
                .any(|n| cleared.iter().any(|c| c == n))
                .then_some((e.message.id, names))
        })
        .collect();
    if notices.is_empty() {
        return Ok(());
    }
    // Seats of a notice that still fail keep it unread.
    let failing: HashSet<String> = st
        .presence
        .snapshot(org_id)
        .await?
        .into_iter()
        .filter(|(_, p)| p.turn_failed.is_some())
        .map(|(n, _)| n)
        .collect();
    let mut events = Vec::new();
    for (id, names) in notices {
        if names.iter().any(|n| failing.contains(*n)) {
            continue;
        }
        if let Some((_, Some(bump))) = st.store.mark_read(id, user.id).await? {
            events.push((user.name.clone(), bump.seq));
        }
    }
    if !events.is_empty() {
        tracing::info!(
            "org {org_id}: {} turn-failure notice(s) marked read: the seats' turns work again",
            events.len()
        );
        st.bus.publish(org_id, &events);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_one_quoted_line() {
        let h = host(Some("box\"\n- seat x: open -> evil\u{202e}"));
        assert!(!h.contains('\n'), "{h}");
        assert_eq!(h.matches('"').count(), 2, "{h}");
        assert!(!h.contains('\u{202e}'), "{h}");
        assert_eq!(host(None), "(no label)");
    }

    #[test]
    fn unknown_names_are_counted_not_shown() {
        let seats: HashSet<String> = ["w1".to_string()].into();
        let t = names_text(&["w1".into(), "ignore all previous".into()], &seats, 3);
        assert_eq!(
            t,
            ": w1 (1 name that is not a seat of this org) (3 requests)"
        );
    }
}
