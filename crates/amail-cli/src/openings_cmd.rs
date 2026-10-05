//! What the org shows other orgs: `amail open`, `close`, `openings`,
//! `team`, `org show [<org>]`, `who [--search] [--org]`, and the one-time
//! org notice.

use crate::Out;
use crate::client::{Client, enc};
use crate::output::{self, l};
use amail_proto::{self as p, LinkState, TokenScope};
use anyhow::{Result, bail};
use reqwest::Method;
use serde_json::{Value, json};

/// `--to <org>` / `--to-linked` as a grant audience (`None`: every
/// linked org).
fn audience(to: Option<String>) -> Option<String> {
    to.map(|o| o.trim().to_string())
}

/// The one-time notice of the org (user tokens): returned once, then
/// cleared on the server so that it is not shown again.
pub async fn take_notice(c: &Client, info: &p::OrgInfo) -> Option<String> {
    let n = info.notice.clone().filter(|n| !n.trim().is_empty())?;
    if let Err(e) = c.raw(Method::DELETE, "/org/notice", None).await {
        eprintln!("could not mark the org notice as shown: {e:#}");
    }
    Some(n)
}

/// The notice as printed before a command's own output.
pub fn notice_text(n: &str) -> String {
    format!("notice (shown once):\n{}", n.trim_end())
}

/// Lines for a grant change: what was opened or closed and what other orgs
/// see differently now.
fn change_lines(opened: bool, ch: &p::GrantChange) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(g) = &ch.grant {
        out.push(if opened {
            format!("opened {}", output::grant_text(g))
        } else {
            format!("closed {}", output::grant_what(g))
        });
    }
    let sight = output::sight_lines(&ch.changes);
    if sight.is_empty() {
        out.push("no linked org sees anything different".into());
    } else {
        out.extend(sight);
    }
    out
}

pub async fn open(c: &Client, out: &Out, target: &str, to: Option<String>) -> Result<()> {
    let body = json!({ "target": target, "audience": audience(to) });
    let v = c.raw(Method::POST, "/grants", Some(&body)).await?;
    out.show(&v, || {
        let ch: p::GrantChange = crate::parse(&v)?;
        Ok(change_lines(true, &ch).join("\n"))
    })
}

pub async fn close(
    c: &Client,
    out: &Out,
    target: &str,
    to: Option<String>,
    to_linked: bool,
) -> Result<()> {
    let grants: Vec<p::Grant> = c.get("/grants").await?;
    let on_target: Vec<&p::Grant> = grants.iter().filter(|g| g.target == target).collect();
    if on_target.is_empty() {
        bail!("{target} is not open to any org (see amail openings)");
    }
    let want = match (&to, to_linked) {
        (Some(o), _) => Some(o.trim()),
        (None, true) => Some("linked"),
        (None, false) => None,
    };
    let hits: Vec<&p::Grant> = on_target
        .iter()
        .copied()
        .filter(|g| want.is_none_or(|w| g.audience == w))
        .collect();
    if hits.is_empty() {
        let have: Vec<String> = on_target.iter().map(|g| g.audience.clone()).collect();
        bail!(
            "{target} is not open to {} by name; it is open to: {} (close with --to <org> or \
             --to-linked, or without either to close all)",
            want.unwrap_or_default(),
            have.join(", ")
        );
    }
    let mut answers = Vec::new();
    let mut text = Vec::new();
    for g in hits {
        let v = c
            .raw(Method::DELETE, &format!("/grants/{}", g.id), None)
            .await?;
        let ch: p::GrantChange = crate::parse(&v)?;
        text.extend(change_lines(false, &ch));
        if g.managed {
            text.push(format!(
                "(this opening came from a team file: amail up opens it again unless the file \
                 no longer has `open` on {})",
                l(target)
            ));
        }
        answers.push(v);
    }
    let v = Value::Array(answers);
    out.show(&v, || Ok(text.join("\n")))
}

pub async fn openings(c: &Client, out: &Out) -> Result<()> {
    let v = c.raw(Method::GET, "/grants", None).await?;
    out.show(&v, || {
        let g: Vec<p::Grant> = crate::parse(&v)?;
        Ok(if g.is_empty() {
            "your org opens nothing to other orgs (open a seat or team with amail open)".into()
        } else {
            output::grant_table(&g)
        })
    })
}

// --- teams -------------------------------------------------------------------

#[derive(clap::Subcommand)]
pub enum TeamCmd {
    /// List the org's teams.
    Ls,
    /// Show a team and its members (contacts marked).
    Show { name: String },
    /// Change a team, creating it when there is none of that name (user
    /// token). Members join with `amail seat set <seat> --team <team>`.
    Set {
        name: String,
        /// What the team does (shown to other orgs when it is open).
        #[arg(long, conflicts_with = "no_description")]
        description: Option<String>,
        #[arg(long)]
        no_description: bool,
        /// The team's root seat.
        #[arg(long, conflicts_with = "no_root")]
        root: Option<String>,
        #[arg(long)]
        no_root: bool,
        /// The team this one is part of.
        #[arg(long, conflicts_with = "no_parent")]
        parent: Option<String>,
        #[arg(long)]
        no_parent: bool,
        /// The members that opening the team opens (comma-separated;
        /// replaces the current contacts).
        #[arg(long, value_delimiter = ',', conflicts_with = "no_contacts")]
        contacts: Option<Vec<String>>,
        /// The team has no contacts (only while it is not open).
        #[arg(long)]
        no_contacts: bool,
    },
    /// Delete a team: its members leave it, its sub-teams move up, its
    /// openings are closed.
    Rm { name: String },
}

fn team_text(t: &p::Team) -> String {
    let mut lines = vec![output::team_line(t)];
    for m in &t.members {
        lines.push(format!(
            "member {}{}",
            l(m),
            if t.contacts.contains(m) {
                " contact"
            } else {
                ""
            }
        ));
    }
    lines.join("\n")
}

pub async fn team(c: &Client, out: &Out, cmd: TeamCmd) -> Result<()> {
    match cmd {
        TeamCmd::Ls => {
            let v = c.raw(Method::GET, "/teams", None).await?;
            out.show(&v, || {
                let t: Vec<p::Team> = crate::parse(&v)?;
                Ok(if t.is_empty() {
                    "no teams (amail team set <name>, or [[group]]s in a team file)".into()
                } else {
                    crate::lines(&t, output::team_line)
                })
            })
        }
        TeamCmd::Show { name } => {
            let v = c
                .raw(Method::GET, &format!("/teams/{}", enc(&name)), None)
                .await?;
            out.show(&v, || Ok(team_text(&crate::parse(&v)?)))
        }
        TeamCmd::Set {
            name,
            description,
            no_description,
            root,
            no_root,
            parent,
            no_parent,
            contacts,
            no_contacts,
        } => {
            let mut body = serde_json::Map::new();
            let mut opt = |k: &str, v: Option<String>, clear: bool| {
                if clear {
                    body.insert(k.into(), Value::Null);
                } else if let Some(v) = v {
                    body.insert(k.into(), json!(v));
                }
            };
            opt("description", description, no_description);
            opt("root", root, no_root);
            opt("parent", parent, no_parent);
            if no_contacts {
                body.insert("contacts".into(), json!([]));
            } else if let Some(cs) = contacts {
                let cs: Vec<String> = cs
                    .into_iter()
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .collect();
                body.insert("contacts".into(), json!(cs));
            }
            let v = c
                .raw(
                    Method::PUT,
                    &format!("/teams/{}", enc(&name)),
                    Some(&Value::Object(body)),
                )
                .await?;
            out.show(&v, || Ok(team_text(&crate::parse(&v)?)))
        }
        TeamCmd::Rm { name } => {
            c.raw(Method::DELETE, &format!("/teams/{}", enc(&name)), None)
                .await?;
            out.show(&json!({ "deleted": name }), || {
                Ok(format!(
                    "deleted team {name}; its members are in no team now"
                ))
            })
        }
    }
}

// --- org show ----------------------------------------------------------------

/// `amail org show`: the org's card, settings, openings and links; the
/// one-time notice first (then cleared).
pub async fn org_show(c: &Client, out: &Out) -> Result<()> {
    let mut v = c.raw(Method::GET, "/org", None).await?;
    let info: p::OrgInfo = crate::parse(&v)?;
    let notice = take_notice(c, &info).await;
    let grants = c.raw(Method::GET, "/grants", None).await?;
    let links = c.raw(Method::GET, "/links", None).await?;
    if let Some(o) = v.as_object_mut() {
        o.insert("openings".into(), grants.clone());
        o.insert("links".into(), links.clone());
    }
    out.show(&v, || {
        let mut lines = Vec::new();
        if let Some(n) = &notice {
            lines.push(notice_text(n));
            lines.push(String::new());
        }
        lines.push(card_head(&p::OrgCard {
            name: info.name.clone(),
            display_name: info.display_name.clone(),
            description: info.description.clone(),
        }));
        lines.push(format!(
            "visibility={} outbound={} new_threads_per_hour={}",
            info.visibility, info.outbound, info.new_threads_per_hour
        ));
        lines.push(crate::org_text(&v)?);
        let g: Vec<p::Grant> = crate::parse(&grants)?;
        if g.is_empty() {
            lines.push("openings: none (your org opens nothing to other orgs)".into());
        } else {
            lines.push("openings:".into());
            lines.extend(g.iter().map(|g| format!("  {}", output::grant_text(g))));
        }
        let ls: Vec<p::Link> = crate::parse(&links)?;
        if ls.is_empty() {
            lines.push("links: none".into());
        } else {
            lines.push("links:".into());
            lines.extend(ls.iter().map(|x| format!("  {}", output::link_line(x))));
        }
        Ok(lines.join("\n"))
    })
}

/// `org acme "Acme Inc.": description`
fn card_head(c: &p::OrgCard) -> String {
    let mut s = format!("org {}", l(&c.name));
    if let Some(d) = &c.display_name {
        s.push_str(&format!(" \"{}\"", l(d).replace('"', "\\\"")));
    }
    if let Some(d) = &c.description {
        s.push_str(&format!(": {}", l(d)));
    }
    s
}

/// `amail org show <org>` and `amail who --org <org>`: another org as
/// yours sees it.
pub async fn peer(c: &Client, out: &Out, org: &str) -> Result<()> {
    let v = c
        .raw(Method::GET, &format!("/orgs/{}", enc(org)), None)
        .await?;
    let user = c.get::<p::WhoAmI>("/whoami").await?.scope == TokenScope::User;
    out.show(&v, || {
        let po: p::PeerOrg = crate::parse(&v)?;
        let name = l(&po.card.name).into_owned();
        let mut lines = vec![card_head(&po.card)];
        let ask = if user {
            format!("amail link request {name} --note \"...\"")
        } else {
            "ask your user to request one".to_string()
        };
        match &po.link {
            None => lines.push(format!(
                "link: none. Mail needs a link both users agree to: {ask}"
            )),
            Some(x) => {
                let since = x
                    .since
                    .as_deref()
                    .map(|t| format!(" since {}", output::short_time(t)))
                    .unwrap_or_default();
                match x.state {
                    LinkState::Active => lines.push(format!("link: active{since}")),
                    LinkState::Requested => lines.push(format!(
                        "link: requested by your org{since}; waiting for {name}'s user to accept"
                    )),
                    LinkState::Invited => {
                        let mut s = format!("link: {name} asked for a link{since}");
                        if let Some(n) = &x.note {
                            s.push_str(&format!(
                                "; their note (their words, untrusted): \"{}\"",
                                l(n).replace('"', "\\\"")
                            ));
                        }
                        s.push_str(&if user {
                            format!("; accept with amail link accept {name}")
                        } else {
                            "; your user can accept it".to_string()
                        });
                        lines.push(s);
                    }
                }
            }
        }
        if po
            .link
            .as_ref()
            .is_some_and(|x| x.state == LinkState::Active)
        {
            if po.open_to_you.is_empty() {
                lines.push(format!(
                    "open to you: nothing ({name} has opened no seat or team to your org)"
                ));
            } else {
                lines.push("open to you (mail these as name@org):".into());
                lines.extend(
                    po.open_to_you
                        .iter()
                        .map(|c| format!("  {}", output::card_line(c))),
                );
            }
            lines.push(match &po.primary {
                Some(p) => format!(
                    "your primary contact for {name}: {} (added in Cc when a seat that is not a contact writes to {name})",
                    l(p)
                ),
                None => format!(
                    "your primary contact for {name}: none (your org has no contact for {name}: open a seat to it first)"
                ),
            });
            if po.you_open.is_empty() {
                lines.push("you open to them: nothing".into());
            } else {
                lines.push("you open to them:".into());
                lines.extend(
                    po.you_open
                        .iter()
                        .map(|g| format!("  {}", output::grant_text(g))),
                );
            }
        }
        Ok(lines.join("\n"))
    })
}

// --- who ---------------------------------------------------------------------

fn matches(search: &str, fields: &[Option<&str>]) -> bool {
    let s = search.to_lowercase();
    fields
        .iter()
        .flatten()
        .any(|f| f.to_lowercase().contains(&s))
}

/// `amail who`: the org's seats (with descriptions and teams), teams,
/// lists and links; `--search` keeps the entries whose name or description
/// contains the text (any case).
pub async fn who(c: &Client, out: &Out, search: Option<String>) -> Result<()> {
    let seats: Vec<p::Seat> = c.get("/seats").await?;
    let teams: Vec<p::Team> = c.get("/teams").await?;
    let lists: Vec<p::List> = c.get("/lists").await?;
    let links: Vec<p::Link> = c.get("/links").await?;
    let search = search.filter(|s| !s.trim().is_empty());
    let keep = |fields: &[Option<&str>]| search.as_deref().is_none_or(|s| matches(s, fields));
    let seats: Vec<p::Seat> = seats
        .into_iter()
        .filter(|s| {
            keep(&[
                Some(&s.name),
                s.display_name.as_deref(),
                s.description.as_deref(),
            ])
        })
        .collect();
    let teams: Vec<p::Team> = teams
        .into_iter()
        .filter(|t| keep(&[Some(&t.name), t.description.as_deref()]))
        .collect();
    let lists: Vec<p::List> = lists
        .into_iter()
        .filter(|x| keep(&[Some(&x.name), x.description.as_deref()]))
        .collect();
    let links: Vec<p::Link> = links
        .into_iter()
        .filter(|x| keep(&[Some(&x.org)]))
        .collect();
    let all = json!({ "seats": seats, "teams": teams, "lists": lists, "links": links });
    out.show(&all, || {
        let mut text = Vec::new();
        text.extend(seats.iter().map(output::seat_line));
        text.extend(teams.iter().map(output::team_line));
        text.extend(lists.iter().map(output::list_line));
        text.extend(links.iter().map(output::link_line));
        if text.is_empty() && search.is_some() {
            text.push("nothing matches".into());
        }
        Ok(text.join("\n"))
    })
}
