//! `amail admin`: operator commands that work directly on the database
//! (`--db` or `AMAIL_DB`): orgs, invite codes, operator tokens, statistics,
//! suspending and deleting orgs, resetting an org's user token.

use crate::Out;
use amail_proto as p;
use amail_store::Store;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::json;
use std::sync::Arc;

#[derive(Subcommand)]
pub enum AdminCmd {
    /// Create an org and its user seat; prints the user token.
    CreateOrg {
        org: String,
        #[arg(long, default_value = "owner")]
        user_name: String,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    /// Invite codes for registration on a public server (one-time use).
    Invite {
        #[command(subcommand)]
        cmd: InviteCmd,
    },
    /// Operator tokens (for `/admin` in the web interface and the operator
    /// API); they belong to no org and never see mail.
    OperatorToken {
        #[command(subcommand)]
        cmd: OperatorTokenCmd,
    },
    /// Orgs: list, suspend, resume, delete.
    Org {
        #[command(subcommand)]
        cmd: OrgAdminCmd,
    },
    /// User tokens of an org.
    Token {
        #[command(subcommand)]
        cmd: TokenAdminCmd,
    },
    /// Orgs, seats, recent mail, the busiest orgs (no mail content).
    Stats {
        #[arg(long, env = "AMAIL_DB")]
        db: String,
        /// Show at most this many orgs.
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
}

#[derive(Subcommand)]
pub enum InviteCmd {
    /// Create invite codes and print them (they are stored only as hashes).
    Create {
        #[arg(long, default_value_t = 1)]
        count: u32,
        /// A note for yourself, e.g. who the code is for.
        #[arg(long)]
        note: Option<String>,
        /// Valid for this long (e.g. 7d, 12h); default: no expiry.
        #[arg(long, value_name = "DURATION")]
        expires: Option<String>,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    /// List invites and their state.
    Ls {
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    /// Revoke an unused invite.
    Revoke {
        id: i64,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
}

#[derive(Subcommand)]
pub enum OperatorTokenCmd {
    Create {
        #[arg(long)]
        label: Option<String>,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    Ls {
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    Revoke {
        id: i64,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
}

#[derive(Subcommand)]
pub enum OrgAdminCmd {
    Ls {
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    /// Its tokens stop working and mail to it is refused; data is kept.
    /// Open connections end within about 15 seconds.
    Suspend {
        org: String,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    Resume {
        org: String,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
    /// Delete the org with all its data (mail it sent, also to linked orgs).
    Delete {
        org: String,
        /// The org's name again, to confirm.
        #[arg(long)]
        confirm: String,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
}

#[derive(Subcommand)]
pub enum TokenAdminCmd {
    /// Revoke all user tokens of the org (and its browser sessions) and
    /// print a new one: for a user who lost theirs.
    Reset {
        org: String,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
}

async fn open(db: &str) -> Result<Arc<dyn Store>> {
    amail_store::open(db, &amail_store::OpenOptions { max_connections: 2 })
        .await
        .with_context(|| format!("opening {}", crate::redact_db_url(db)))
}

fn ts(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

async fn org(store: &dyn Store, name: &str) -> Result<amail_store::Org> {
    store
        .org_by_name(name)
        .await?
        .with_context(|| format!("no org {name:?}"))
}

pub async fn run(cmd: AdminCmd, out: &Out) -> Result<()> {
    match cmd {
        AdminCmd::CreateOrg { org, user_name, db } => create_org(&db, &org, &user_name, out).await,
        AdminCmd::Invite { cmd } => invite(cmd, out).await,
        AdminCmd::OperatorToken { cmd } => operator_token(cmd, out).await,
        AdminCmd::Org { cmd } => org_cmd(cmd, out).await,
        AdminCmd::Token {
            cmd: TokenAdminCmd::Reset { org: name, db },
        } => {
            let store = open(&db).await?;
            let o = org(&*store, &name).await?;
            let token = amail_server::new_token();
            store
                .reset_user_tokens(o.id, &amail_server::hash_token(&token))
                .await?;
            let v = json!({ "org": name, "token": token });
            out.show(&v, || {
                eprintln!("revoked every user token of {name}; the new user token:");
                Ok(token.clone())
            })
        }
        AdminCmd::Stats { db, top } => stats(&db, top, out).await,
    }
}

async fn create_org(db: &str, org: &str, user_name: &str, out: &Out) -> Result<()> {
    for n in [org, user_name] {
        if !p::valid_name(n) {
            bail!("invalid name {n:?}: must match [a-z0-9][a-z0-9._-]{{0,62}}");
        }
    }
    let store = open(db).await?;
    let token = amail_server::new_token();
    store
        .create_org(org, user_name, &amail_server::hash_token(&token))
        .await?;
    let v = json!({ "org": org, "user_seat": user_name, "token": token });
    out.show(&v, || {
        eprintln!("created org {org} with user seat {user_name}; user token:");
        Ok(token.clone())
    })
}

async fn invite(cmd: InviteCmd, out: &Out) -> Result<()> {
    match cmd {
        InviteCmd::Create {
            count,
            note,
            expires,
            db,
        } => {
            if count == 0 || count > 1000 {
                bail!("--count must be between 1 and 1000");
            }
            let expires_at = match expires {
                None => None,
                Some(d) => Some(
                    amail_store::now_ms()
                        + amail_node::team::parse_duration(&d)
                            .map_err(anyhow::Error::msg)?
                            .context("--expires must be longer than 0")?
                            .as_millis() as i64,
                ),
            };
            let store = open(&db).await?;
            let mut codes = Vec::new();
            for _ in 0..count {
                let code = amail_server::new_invite();
                let id = store
                    .create_invite(
                        &amail_server::hash_token(&code),
                        note.as_deref(),
                        expires_at,
                    )
                    .await?;
                codes.push(json!({ "id": id, "code": code }));
            }
            let v = json!({ "invites": codes, "expires_at": expires_at.map(ts) });
            out.show(&v, || {
                eprintln!(
                    "{count} invite code(s){}; each works once:",
                    expires_at
                        .map(|t| format!(", valid until {}", ts(t)))
                        .unwrap_or_default()
                );
                Ok(codes
                    .iter()
                    .map(|c| c["code"].as_str().unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        }
        InviteCmd::Ls { db } => {
            let store = open(&db).await?;
            let now = amail_store::now_ms();
            let rows: Vec<serde_json::Value> = store
                .invites()
                .await?
                .into_iter()
                .map(|i| {
                    let state = if i.used_at.is_some() {
                        "used"
                    } else if i.revoked_at.is_some() {
                        "revoked"
                    } else if i.name_conflicts >= amail_proto::limits::MAX_INVITE_NAME_CONFLICTS {
                        "spent"
                    } else if i.expires_at.is_some_and(|e| e <= now) {
                        "expired"
                    } else {
                        "open"
                    };
                    json!({
                        "id": i.id, "state": state, "note": i.note, "created": ts(i.created_at),
                        "expires": i.expires_at.map(ts), "used": i.used_at.map(ts), "org": i.used_by,
                    })
                })
                .collect();
            let v = serde_json::Value::Array(rows.clone());
            out.show(&v, || {
                if rows.is_empty() {
                    return Ok("no invites".into());
                }
                Ok(rows
                    .iter()
                    .map(|r| {
                        let mut s = format!(
                            "{} {} created {}",
                            r["id"],
                            r["state"].as_str().unwrap_or(""),
                            r["created"].as_str().unwrap_or("")
                        );
                        if let Some(e) = r["expires"].as_str() {
                            s.push_str(&format!(" expires {e}"));
                        }
                        if let Some(o) = r["org"].as_str() {
                            s.push_str(&format!(" -> org {o}"));
                        }
                        if let Some(n) = r["note"].as_str() {
                            s.push_str(&format!(" ({n})"));
                        }
                        s
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        }
        InviteCmd::Revoke { id, db } => {
            let store = open(&db).await?;
            if !store.revoke_invite(id).await? {
                bail!("no unused invite {id}");
            }
            out.show(&json!({ "revoked": id }), || {
                Ok(format!("revoked invite {id}"))
            })
        }
    }
}

async fn operator_token(cmd: OperatorTokenCmd, out: &Out) -> Result<()> {
    match cmd {
        OperatorTokenCmd::Create { label, db } => {
            let store = open(&db).await?;
            let token = amail_server::new_token();
            let id = store
                .create_operator_token(&amail_server::hash_token(&token), label.as_deref())
                .await?;
            out.show(&json!({ "id": id, "token": token }), || {
                eprintln!("operator token {id} (shown once):");
                Ok(token.clone())
            })
        }
        OperatorTokenCmd::Ls { db } => {
            let store = open(&db).await?;
            let rows = store.operator_tokens().await?;
            let v = json!(
                rows.iter()
                    .map(|t| json!({ "id": t.id, "label": t.label, "created": ts(t.created_at) }))
                    .collect::<Vec<_>>()
            );
            out.show(&v, || {
                Ok(rows
                    .iter()
                    .map(|t| {
                        format!(
                            "{} {} {}",
                            t.id,
                            ts(t.created_at),
                            t.label.as_deref().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        }
        OperatorTokenCmd::Revoke { id, db } => {
            let store = open(&db).await?;
            if !store.revoke_operator_token(id).await? {
                bail!("no live operator token {id}");
            }
            out.show(&json!({ "revoked": id }), || {
                Ok(format!("revoked operator token {id}"))
            })
        }
    }
}

async fn org_cmd(cmd: OrgAdminCmd, out: &Out) -> Result<()> {
    match cmd {
        OrgAdminCmd::Ls { db } => {
            let store = open(&db).await?;
            let orgs = store.orgs().await?;
            let v = json!(
                orgs.iter()
                    .map(|o| json!({
                        "name": o.name, "created": ts(o.created_at),
                        "suspended": o.suspended_at.map(ts),
                    }))
                    .collect::<Vec<_>>()
            );
            out.show(&v, || {
                Ok(orgs
                    .iter()
                    .map(|o| {
                        format!(
                            "{} created {}{}",
                            o.name,
                            ts(o.created_at),
                            o.suspended_at
                                .map(|t| format!(" SUSPENDED {}", ts(t)))
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        }
        OrgAdminCmd::Suspend { org: name, db } => {
            let store = open(&db).await?;
            let o = org(&*store, &name).await?;
            store
                .set_org_suspended(o.id, Some(amail_store::now_ms()))
                .await?;
            out.show(&json!({ "suspended": name }), || {
                Ok(format!(
                    "suspended {name}: its tokens no longer work; open connections end within ~15 s"
                ))
            })
        }
        OrgAdminCmd::Resume { org: name, db } => {
            let store = open(&db).await?;
            let o = org(&*store, &name).await?;
            store.set_org_suspended(o.id, None).await?;
            out.show(&json!({ "resumed": name }), || {
                Ok(format!("resumed {name}"))
            })
        }
        OrgAdminCmd::Delete {
            org: name,
            confirm,
            db,
        } => {
            if confirm != name {
                bail!("--confirm must repeat the org's name");
            }
            let store = open(&db).await?;
            let o = org(&*store, &name).await?;
            store.delete_org(o.id).await?;
            out.show(&json!({ "deleted": name }), || {
                Ok(format!("deleted {name} and all its data"))
            })
        }
    }
}

async fn stats(db: &str, top: usize, out: &Out) -> Result<()> {
    let store = open(db).await?;
    let s = store.operator_stats().await?;
    let nodes: std::collections::HashMap<i64, i64> = s.node_streams.iter().copied().collect();
    let seats: i64 = s.orgs.iter().map(|o| o.seats).sum();
    let hour: i64 = s.orgs.iter().map(|o| o.messages_hour).sum();
    let suspended = s.orgs.iter().filter(|o| o.suspended_at.is_some()).count();
    let list: Vec<serde_json::Value> = s
        .orgs
        .iter()
        .map(|o| {
            json!({
                "name": o.name, "seats": o.seats, "nodes": nodes.get(&o.id),
                "messages": o.messages, "messages_hour": o.messages_hour,
                "messages_day": o.messages_day, "last_activity": o.last_activity.map(ts),
                "suspended": o.suspended_at.is_some(), "stored_bytes": o.stored_bytes,
            })
        })
        .collect();
    let v = json!({
        "orgs": s.orgs.len(), "suspended": suspended, "seats": seats,
        "mail_last_5min": s.messages_5min, "mail_last_hour": hour, "list": list,
    });
    out.show(&v, || {
        let mut lines = vec![
            format!(
                "{} orgs ({suspended} suspended), {seats} agent seats",
                s.orgs.len()
            ),
            format!(
                "mail: {} in the last 5 min ({:.1}/s), {hour} in the last hour",
                s.messages_5min,
                s.messages_5min as f64 / 300.0
            ),
        ];
        if s.node_streams.is_empty() {
            lines.push(
                "nodes: known to running instances only (see /admin in the web interface)".into(),
            );
        } else {
            lines.push(format!(
                "nodes online: {}",
                s.node_streams.iter().map(|(_, n)| n).sum::<i64>()
            ));
        }
        lines.push(format!(
            "{:<24} {:>6} {:>8} {:>8} {:>10} {:>10}  last activity",
            "org", "seats", "mail/h", "mail/d", "mail", "MiB"
        ));
        for o in s.orgs.iter().take(top) {
            lines.push(format!(
                "{:<24} {:>6} {:>8} {:>8} {:>10} {:>10.1}  {}{}",
                o.name,
                o.seats,
                o.messages_hour,
                o.messages_day,
                o.messages,
                o.stored_bytes as f64 / (1024.0 * 1024.0),
                o.last_activity.map(ts).unwrap_or_else(|| "-".into()),
                if o.suspended_at.is_some() {
                    "  (suspended)"
                } else {
                    ""
                }
            ));
        }
        Ok(lines.join("\n"))
    })
}
