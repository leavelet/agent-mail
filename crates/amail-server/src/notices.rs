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

use crate::AppState;
use crate::auth::Auth;
use crate::error::{ApiError, ApiResult, Json};
use amail_proto::limits::{
    MAX_NODE_LABEL_CHARS, MAX_NODE_NOTICES, MAX_NOTICE_ITEMS, MAX_NOTICE_TARGETS,
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

/// Values of a Codex seat's `git_access`.
const GIT_ACCESS: &[&str] = &["worktree", "full", "off"];

fn valid_value(setting: NoticeSetting, v: &str) -> bool {
    match setting {
        NoticeSetting::LocalPolicy => LocalPolicy::from_str(v).is_ok(),
        NoticeSetting::PermissionMode => PermissionMode::from_str(v).is_ok(),
        NoticeSetting::GitAccess => GIT_ACCESS.contains(&v),
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
    }
}

/// `names` as shown: seats of the org by name; other names are counted.
fn names_text(names: &[String], seats: &HashSet<String>, count: u32) -> String {
    let known: Vec<&str> = names
        .iter()
        .filter(|n| seats.contains(*n))
        .map(String::as_str)
        .collect();
    let unknown = names.len() - known.len();
    let mut out = if known.is_empty() {
        String::new()
    } else {
        format!(": {}", known.join(", "))
    };
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

/// Subject and body of the mail for `n`.
fn render(n: &NodeNotice, host: &str, seats: &HashSet<String>) -> (String, String) {
    match n {
        NodeNotice::Settings { changes } => {
            let mut lines = Vec::new();
            let mut opened = false;
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
                lines.push(format!("- {what}: {old} -> {}", c.new));
                opened |= c.setting == NoticeSetting::LocalPolicy && c.new == "open";
            }
            let mut body = format!(
                "The amail node on host {host} now runs with settings that differ from the \
                 ones it reported before:\n\n{}\n",
                lines.join("\n")
            );
            if opened {
                body.push_str(
                    "\nWith the local policy open, every process of your OS user on that host, \
                     including the agents of the sessions the node started, may manage seats \
                     and obtain any seat's credentials through the node. The node reports \
                     such requests from agent sessions to you. Set local_policy = \
                     \"restricted\" to refuse them (see docs/security.md).\n",
                );
            }
            (format!("Node settings changed on host {host}"), body)
        }
        NodeNotice::AgentActions { seat, actions } => {
            let who = match seat.as_deref() {
                Some(s) if seats.contains(s) => format!("seat {s}"),
                Some(_) => "a seat that is not in this org".to_string(),
                None => "a Codex seat (Codex seats of one node share one process, so the \
                         node cannot tell which)"
                    .to_string(),
            };
            let lines: Vec<String> = actions
                .iter()
                .map(|a| {
                    format!(
                        "- {}{}",
                        op_text(a.op),
                        names_text(&a.targets, seats, a.count)
                    )
                })
                .collect();
            let body = format!(
                "A process of the session of {who} on host {host} asked the amail node to:\n\n\
                 {}\n\nThe node's local policy (open) allowed it. If you did not expect this, \
                 look at that session. With local_policy = \"restricted\" the node refuses \
                 such requests from agent sessions (see docs/security.md). This notice is best \
                 effort: the node recognizes a session's processes by their parent processes, \
                 and a process that left the session's process tree is not recognized.\n",
                lines.join("\n")
            );
            let subject = match seat.as_deref() {
                Some(s) if seats.contains(s) => {
                    format!("Agent seat {s} used the node on host {host}")
                }
                _ => format!("An agent session used the node on host {host}"),
            };
            (subject, body)
        }
    }
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
    let system = st.store.system_seat(org).await?;
    let host = host(report.node.as_deref());
    let mut sent = 0;
    for n in &report.notices {
        let (subject, body) = render(n, &host, &seats);
        let subject: String = subject
            .chars()
            .take(amail_proto::limits::MAX_SUBJECT_CHARS)
            .collect();
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
                meta: Default::default(),
                deliveries: vec![NewDelivery {
                    seat_id: user.id,
                    org_id: org,
                    class: DeliveryClass::To,
                    mode: DeliveryMode::Immediate,
                }],
                ack_due: None,
                // Mail from the mail system is never refused; the rate
                // limit above bounds it.
                storage_quota: None,
            })
            .await?;
        let events: Vec<(String, i64)> = m
            .seqs
            .iter()
            .filter(|b| b.seat_id == user.id)
            .map(|b| (user.name.clone(), b.seq))
            .collect();
        st.bus.publish(org, &events);
        sent += 1;
    }
    tracing::info!("org {org}: {sent} node notice(s) sent to the user seat");
    Ok(Json(NodeNoticesAccepted { sent }))
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
