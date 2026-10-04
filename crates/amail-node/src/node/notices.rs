//! The local policy and the notices that tell the person what the node
//! allowed (see [`amail_proto::LocalPolicy`] and
//! [`amail_proto::NodeNotice`]).
//!
//! - **Settings.** At start and after each `amail up`, the node compares
//!   its local policy and the permission mode and git access of its
//!   supervised seats with what the server last accepted from it
//!   ([`crate::persist::Saved::reported`]), and reports the differences.
//! - **Requests of agent sessions.** Under the open policy, the node still
//!   looks at the parent processes of each caller of a seat-management
//!   request (`up`, `down`, `forget`, `run`, `wake`, `sleep`) and of a
//!   credentials request. When the caller belongs to a session the node
//!   launched, the request is recorded and reported: at once for a session
//!   that had no notice within [`Config::notice_window`], otherwise at the
//!   end of that window, together with the other requests in it. As in the
//!   restricted policy, a process that left the session's process tree is
//!   not recognized, so these notices are best effort.
//!
//! The server writes the mail from fixed texts, for the org's user seat.

use super::caller::Caller;
use super::*;
use crate::persist::{Reported, ReportedSeat};
use amail_proto::limits::{MAX_NODE_NOTICES, MAX_NOTICE_ITEMS, MAX_NOTICE_TARGETS};
use amail_proto::{
    AgentAction, LocalPolicy, NodeNotice, NodeNotices, NoticeOp, NoticeSetting, PermissionMode,
    SettingChange,
};
use std::collections::BTreeSet;

/// Requests of one session not reported yet.
#[derive(Debug, Clone, Default)]
pub(super) struct Pending {
    /// `(op, seats named, requests)`, in the order first seen.
    actions: Vec<(NoticeOp, BTreeSet<String>, u32)>,
    /// When the last notice for this session was accepted.
    last_sent: Option<Instant>,
}

/// The seats a request is about, for a notice; `None` for requests that
/// are not reported.
pub(super) fn request_op(req: &Request) -> Option<(NoticeOp, Vec<String>)> {
    Some(match req {
        Request::Launch { seat, .. } => (NoticeOp::Run, vec![seat.clone()]),
        Request::Up { seats, only, .. } => (
            NoticeOp::Up,
            only.clone()
                .unwrap_or_else(|| seats.iter().map(|p| p.seat.clone()).collect()),
        ),
        Request::Down { seats } => (NoticeOp::Down, seats.clone()),
        Request::Forget { seats } => (NoticeOp::Forget, seats.clone()),
        Request::Wake { seat } => (NoticeOp::Wake, vec![seat.clone()]),
        Request::Sleep { seat } => (NoticeOp::Sleep, vec![seat.clone()]),
        _ => return None,
    })
}

/// The policy in effect for the node's setting and the team file's:
/// `restricted` if either says so, else the node's setting, else
/// [`LocalPolicy::DEFAULT`]. A team file lies in the project, where agents
/// can write, so it cannot loosen the node's policy.
pub fn effective(node: Option<LocalPolicy>, team: Option<LocalPolicy>) -> LocalPolicy {
    if node == Some(LocalPolicy::Restricted) || team == Some(LocalPolicy::Restricted) {
        LocalPolicy::Restricted
    } else {
        node.unwrap_or(LocalPolicy::DEFAULT)
    }
}

/// The changes from `old` to `new`. `old_policy` stands for `old` when the
/// node never reported (an earlier version that did not report used the
/// restricted policy). A seat that was not reported before is listed when
/// one of its settings is not the default, with all its settings; seats
/// that are gone are not listed.
pub(super) fn changes(
    old: Option<&Reported>,
    old_policy: Option<&str>,
    new: &Reported,
) -> Vec<SettingChange> {
    let mut out = Vec::new();
    let policy = old.map(|o| o.local_policy.as_str()).or(old_policy);
    if policy != Some(new.local_policy.as_str()) {
        out.push(SettingChange {
            seat: None,
            setting: NoticeSetting::LocalPolicy,
            old: policy.map(str::to_string),
            new: new.local_policy.clone(),
        });
    }
    let jail_default = "on";
    for (name, s) in &new.seats {
        let before = old.and_then(|o| o.seats.get(name));
        // A seat not reported before is listed when any of its settings is
        // not the default, and then with all of them (an unattended Codex
        // seat with its git access, an unattended dsh seat with its jail).
        let all = before.is_none()
            && (s.permission_mode != PermissionMode::Default.as_str()
                || s.git_access
                    .as_deref()
                    .is_some_and(|g| g != GitAccess::default().as_str())
                || s.dsh_jail.as_deref().is_some_and(|j| j != jail_default));
        let mut field = |setting, was: Option<&str>, now: &str, default: &str| {
            let changed = match was {
                Some(w) => w != now,
                None => all || now != default,
            };
            if changed {
                out.push(SettingChange {
                    seat: Some(name.clone()),
                    setting,
                    old: was.map(str::to_string),
                    new: now.to_string(),
                });
            }
        };
        field(
            NoticeSetting::PermissionMode,
            before.map(|b| b.permission_mode.as_str()),
            &s.permission_mode,
            PermissionMode::Default.as_str(),
        );
        if let Some(g) = &s.git_access {
            field(
                NoticeSetting::GitAccess,
                before.and_then(|b| b.git_access.as_deref()),
                g,
                GitAccess::default().as_str(),
            );
        }
        if let Some(j) = &s.dsh_jail {
            field(
                NoticeSetting::DshJail,
                before.and_then(|b| b.dsh_jail.as_deref()),
                j,
                jail_default,
            );
        }
    }
    out.truncate(MAX_NOTICE_ITEMS);
    out
}

impl Node {
    /// The policy in effect: `restricted` if the node or the team file says
    /// so, else the node's setting, else the default.
    pub(super) fn local_policy(&self) -> LocalPolicy {
        let team = self.lock().saved.settings.local_policy;
        self.policy_with(team)
    }

    pub(super) fn policy_with(&self, team: Option<LocalPolicy>) -> LocalPolicy {
        effective(self.cfg.local_policy, team)
    }

    /// Where the policy in effect comes from, for `ps` and `status`.
    pub(super) fn policy_source(&self) -> &'static str {
        let team = self.lock().saved.settings.local_policy;
        match (self.cfg.local_policy, team) {
            (Some(LocalPolicy::Restricted), _) | (Some(_), None) => "node setting",
            (_, Some(LocalPolicy::Restricted)) => "team file",
            (Some(_), Some(_)) => "node setting",
            (None, _) => "default",
        }
    }

    /// The settings as they would be reported now.
    fn reported_now(&self, st: &State) -> Reported {
        Reported {
            local_policy: self
                .policy_with(st.saved.settings.local_policy)
                .as_str()
                .to_string(),
            seats: st
                .saved
                .managed
                .iter()
                .map(|(name, m)| {
                    (
                        name.clone(),
                        ReportedSeat {
                            permission_mode: m
                                .permission_mode
                                .unwrap_or(PermissionMode::Default)
                                .as_str()
                                .to_string(),
                            git_access: matches!(m.harness.as_str(), "codex" | "dsh")
                                .then(|| m.git_access.unwrap_or_default().as_str().to_string()),
                            dsh_jail: (m.harness == "dsh").then(|| {
                                if m.dsh_jail.unwrap_or(true) {
                                    "on"
                                } else {
                                    "off"
                                }
                                .to_string()
                            }),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Record a request of `caller` for the next notice if `caller` belongs
    /// to an agent session.
    pub(super) fn note_request(&self, caller: &Caller, op: NoticeOp, targets: Vec<String>) {
        if !self.cfg.notices || !caller.is_session() {
            return;
        }
        let key = caller
            .session
            .as_ref()
            .map(|(s, _)| s.clone())
            .unwrap_or_default();
        tracing::warn!(
            "an agent session ({}) asked the node to {op} {}",
            if key.is_empty() { "Codex" } else { &key },
            targets.join(", ")
        );
        let mut st = self.lock();
        if st.notices.len() >= MAX_NODE_NOTICES * 10 && !st.notices.contains_key(&key) {
            return;
        }
        let p = st.notices.entry(key).or_default();
        let i = match p.actions.iter().position(|a| a.0 == op) {
            Some(i) => i,
            None => {
                if p.actions.len() >= MAX_NOTICE_ITEMS {
                    return;
                }
                p.actions.push((op, BTreeSet::new(), 0));
                p.actions.len() - 1
            }
        };
        let a = &mut p.actions[i];
        a.2 = a.2.saturating_add(1);
        for t in targets {
            if a.1.len() >= MAX_NOTICE_TARGETS {
                break;
            }
            a.1.insert(t);
        }
    }

    pub(super) async fn notice_loop(self: Arc<Self>) {
        loop {
            self.send_notices().await;
            tokio::time::sleep(self.cfg.notice_tick).await;
        }
    }

    /// Send what is due; keep it for the next try if the server does not
    /// take it.
    pub(super) async fn send_notices(&self) {
        if !self.lock().connected {
            // Kept until the server is reachable again.
            return;
        }
        let now = Instant::now();
        let (notices, settings, sessions, turn_notices) = {
            let mut st = self.lock();
            let mut notices = Vec::new();
            let mut settings = None;
            if st.settings_due {
                let new = self.reported_now(&st);
                let old_policy = st.prior_state.then_some(LocalPolicy::Restricted.as_str());
                let ch = changes(st.saved.reported.as_ref(), old_policy, &new);
                if ch.is_empty() {
                    if st.saved.reported.as_ref() != Some(&new) {
                        st.saved.reported = Some(new);
                        st.save_dirty = true;
                    }
                    st.settings_due = false;
                } else {
                    notices.push(NodeNotice::Settings { changes: ch });
                    settings = Some(new);
                }
            }
            // Failing turns (at most a few per report; the rest next time).
            let n = st
                .turn_notices
                .len()
                .min(MAX_NODE_NOTICES.saturating_sub(notices.len()));
            let turn_notices: Vec<NodeNotice> = st.turn_notices.drain(..n).collect();
            notices.extend(turn_notices.iter().cloned());
            let mut sessions = Vec::new();
            for (key, p) in st.notices.iter_mut() {
                if notices.len() >= MAX_NODE_NOTICES {
                    break;
                }
                if p.actions.is_empty()
                    || p.last_sent
                        .is_some_and(|t| now < t + self.cfg.notice_window)
                {
                    continue;
                }
                let actions = std::mem::take(&mut p.actions);
                notices.push(NodeNotice::AgentActions {
                    seat: (!key.is_empty()).then(|| key.clone()),
                    actions: actions
                        .iter()
                        .map(|(op, t, n)| AgentAction {
                            op: *op,
                            targets: t.iter().cloned().collect(),
                            count: *n,
                        })
                        .collect(),
                });
                sessions.push((key.clone(), actions));
            }
            (notices, settings, sessions, turn_notices)
        };
        if notices.is_empty() {
            return;
        }
        let report = NodeNotices {
            node: self.cfg.label.clone(),
            notices,
        };
        let sent = self.server.notices(&report).await;
        let mut st = self.lock();
        if let Err(e) = sent {
            tracing::warn!("node notices not sent (will retry): {e:#}");
            let newer = std::mem::replace(&mut st.turn_notices, turn_notices);
            st.turn_notices.extend(newer);
            // Put the requests back, with those recorded meanwhile.
            for (key, actions) in sessions {
                let p = st.notices.entry(key).or_default();
                let newer = std::mem::replace(&mut p.actions, actions);
                for (op, targets, n) in newer {
                    match p.actions.iter_mut().find(|a| a.0 == op) {
                        Some(a) => {
                            a.2 = a.2.saturating_add(n);
                            for t in targets {
                                if a.1.len() < MAX_NOTICE_TARGETS {
                                    a.1.insert(t);
                                }
                            }
                        }
                        None => p.actions.push((op, targets, n)),
                    }
                }
            }
            return;
        }
        if let Some(new) = settings {
            st.saved.reported = Some(new);
            st.save_dirty = true;
            // `up` may have changed the settings again meanwhile; the next
            // pass compares once more.
        }
        for (key, _) in sessions {
            st.notices.entry(key).or_default().last_sent = Some(now);
        }
        let window = self.cfg.notice_window;
        st.notices
            .retain(|_, p| !p.actions.is_empty() || p.last_sent.is_some_and(|t| now < t + window));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(policy: &str, seats: &[(&str, &str, Option<&str>)]) -> Reported {
        Reported {
            local_policy: policy.into(),
            seats: seats
                .iter()
                .map(|(n, p, g)| {
                    (
                        n.to_string(),
                        ReportedSeat {
                            permission_mode: p.to_string(),
                            git_access: g.map(str::to_string),
                            dsh_jail: None,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn policy_in_effect() {
        use LocalPolicy::*;
        assert_eq!(effective(None, None), LocalPolicy::DEFAULT);
        assert_eq!(effective(Some(Open), None), Open);
        assert_eq!(effective(Some(Open), Some(Restricted)), Restricted);
        assert_eq!(effective(Some(Restricted), Some(Open)), Restricted);
        // The team file alone never loosens the default.
        assert_eq!(effective(None, Some(Open)), LocalPolicy::DEFAULT);
        assert_eq!(effective(None, Some(Restricted)), Restricted);
    }

    #[test]
    fn dsh_jail_changes() {
        let seat = |j: &str| ReportedSeat {
            permission_mode: "default".into(),
            git_access: None,
            dsh_jail: Some(j.into()),
        };
        let with = |j: &str| Reported {
            local_policy: "open".into(),
            seats: [("d".to_string(), seat(j))].into(),
        };
        // A new dsh seat with the jail on (the default) is not news; off is.
        assert!(changes(None, Some("open"), &with("on")).is_empty());
        // (listed with its permission mode, as all its settings are).
        let c = changes(None, Some("open"), &with("off"));
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].setting, NoticeSetting::DshJail);
        // A change either way is reported.
        let c = changes(Some(&with("off")), None, &with("on"));
        assert_eq!(
            (c[0].seat.as_deref(), c[0].old.as_deref(), c[0].new.as_str()),
            (Some("d"), Some("off"), "on")
        );
    }

    /// The first report of an unattended Codex or dsh seat names its git
    /// access or jail too, though they are the defaults.
    #[test]
    fn first_report_names_all_settings_of_a_listed_seat() {
        let seat = |p: &str, g: Option<&str>, j: Option<&str>| ReportedSeat {
            permission_mode: p.into(),
            git_access: g.map(str::to_string),
            dsh_jail: j.map(str::to_string),
        };
        let new = Reported {
            local_policy: "open".into(),
            seats: [
                ("cx".to_string(), seat("unattended", Some("worktree"), None)),
                (
                    "d".to_string(),
                    seat("unattended", Some("worktree"), Some("on")),
                ),
                ("quiet".to_string(), seat("default", Some("worktree"), None)),
            ]
            .into(),
        };
        let c = changes(None, Some("open"), &new);
        let shown: Vec<(&str, NoticeSetting, &str)> = c
            .iter()
            .map(|c| (c.seat.as_deref().unwrap(), c.setting, c.new.as_str()))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("cx", NoticeSetting::PermissionMode, "unattended"),
                ("cx", NoticeSetting::GitAccess, "worktree"),
                ("d", NoticeSetting::PermissionMode, "unattended"),
                ("d", NoticeSetting::GitAccess, "worktree"),
                ("d", NoticeSetting::DshJail, "on"),
            ]
        );
        // Reported once, nothing is news any more.
        assert!(changes(Some(&new), None, &new).is_empty());
    }

    #[test]
    fn setting_changes() {
        let a = rep(
            "restricted",
            &[("w1", "default", None), ("x", "default", Some("worktree"))],
        );
        // Nothing changed.
        assert!(changes(Some(&a), None, &a).is_empty());
        // First report of a fresh node: the policy only (defaults are not news).
        let c = changes(None, None, &a);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].old.as_deref(), c[0].new.as_str()),
            (None, "restricted")
        );
        // A node of an earlier version used the restricted policy.
        assert!(changes(None, Some("restricted"), &a).is_empty());
        let b = rep(
            "open",
            &[
                ("w1", "unattended", None),
                ("x", "default", Some("full")),
                ("new", "unattended", None),
            ],
        );
        let c = changes(Some(&a), None, &b);
        let shown: Vec<(Option<&str>, NoticeSetting, Option<&str>, &str)> = c
            .iter()
            .map(|c| {
                (
                    c.seat.as_deref(),
                    c.setting,
                    c.old.as_deref(),
                    c.new.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (None, NoticeSetting::LocalPolicy, Some("restricted"), "open"),
                (
                    Some("new"),
                    NoticeSetting::PermissionMode,
                    None,
                    "unattended"
                ),
                (
                    Some("w1"),
                    NoticeSetting::PermissionMode,
                    Some("default"),
                    "unattended"
                ),
                (
                    Some("x"),
                    NoticeSetting::GitAccess,
                    Some("worktree"),
                    "full"
                ),
            ]
        );
    }
}
