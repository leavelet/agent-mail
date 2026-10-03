//! `amail`: mail and mailing lists for teams of coding agents.

mod client;
mod output;

use amail_proto::{self as p, DeliveryMode, PostPolicy};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use client::{Client, enc, is_not_found};
use reqwest::Method;
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "amail",
    version,
    about = "Mail and mailing lists for teams of coding agents"
)]
struct Cli {
    /// Print JSON instead of compact text.
    #[arg(long, global = true)]
    json: bool,
    /// TOML file with `url` and `token` (default: AMAIL_URL / AMAIL_TOKEN).
    #[arg(long, global = true, env = "AMAIL_PROFILE")]
    profile: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server.
    Serve {
        /// `sqlite://path` or `postgres://...`
        #[arg(long, env = "AMAIL_DB")]
        db: String,
        #[arg(long, default_value = "127.0.0.1:8025")]
        listen: String,
        /// Postgres pool size, or SQLite read pool size.
        #[arg(long, default_value_t = 32)]
        max_connections: u32,
    },
    /// Administration that works directly on the database.
    #[command(subcommand)]
    Admin(AdminCmd),
    /// Send a message. The body comes from -m or stdin.
    Send {
        /// Comma-separated recipients (`name` or `name@org`).
        #[arg(long, value_delimiter = ',')]
        to: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        cc: Vec<String>,
        #[arg(short, long)]
        subject: String,
        #[command(flatten)]
        opts: SendOpts,
    },
    /// List messages (unread only unless --all).
    Inbox {
        #[arg(long)]
        all: bool,
        /// Mailbox to show (user and node tokens).
        #[arg(long)]
        seat: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Show a message and mark it read.
    Read {
        id: i64,
        /// Do not mark as read.
        #[arg(long)]
        peek: bool,
    },
    /// Reply to a message; subject and thread are inherited.
    Reply {
        id: i64,
        /// Reply to the sender and all other recipients.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        opts: SendOpts,
    },
    /// Acknowledge a message.
    Ack { id: i64 },
    /// Delivery state of each recipient of a message you sent.
    Status { id: i64 },
    /// Show a thread (by thread id or the id of any message in it).
    Thread { id: i64 },
    /// Directory: seats, lists and linked orgs.
    Who,
    /// Claim pending notifications (marks them notified).
    Claim {
        #[arg(long)]
        seat: Option<String>,
        #[arg(long, default_value = "cli")]
        via: String,
        /// Comma-separated classes to claim.
        #[arg(long, default_value = "to,cc,list")]
        classes: String,
        #[arg(long)]
        urgent_only: bool,
        #[arg(long)]
        include_digest: bool,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        inline_body_max: Option<u32>,
    },
    /// Manage seats.
    #[command(subcommand)]
    Seat(SeatCmd),
    /// Manage mailing lists.
    #[command(subcommand)]
    List(ListCmd),
    /// Manage links with other orgs.
    #[command(subcommand)]
    Link(LinkCmd),
    /// Create node or extra user tokens.
    #[command(subcommand)]
    Token(TokenCmd),
    /// Show or change org settings.
    #[command(subcommand)]
    Org(OrgCmd),
}

#[derive(Args)]
struct SendOpts {
    /// Message body; read from stdin when omitted.
    #[arg(short = 'm', long = "message")]
    body: Option<String>,
    #[arg(long)]
    urgent: bool,
    /// Ask recipients to acknowledge.
    #[arg(long)]
    ack: bool,
}

#[derive(Subcommand)]
enum AdminCmd {
    /// Create an org and its user seat; prints the user token.
    CreateOrg {
        org: String,
        #[arg(long, default_value = "owner")]
        user_name: String,
        #[arg(long, env = "AMAIL_DB")]
        db: String,
    },
}

#[derive(Subcommand)]
enum SeatCmd {
    Add {
        name: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        display_name: Option<String>,
    },
    Ls,
    Rm {
        name: String,
    },
    /// Create a token for a seat and print it.
    Token {
        name: String,
        #[arg(long)]
        label: Option<String>,
    },
}

#[derive(Subcommand)]
enum ListCmd {
    Create {
        name: String,
        #[arg(long, default_value = "anyone")]
        policy: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// List all lists, or the members of one.
    Ls {
        name: Option<String>,
    },
    Rm {
        name: String,
    },
    /// Add members (or change their mode).
    Add {
        list: String,
        #[arg(required = true)]
        addresses: Vec<String>,
        #[arg(long, default_value = "immediate")]
        mode: String,
    },
    Remove {
        list: String,
        #[arg(required = true)]
        addresses: Vec<String>,
    },
}

#[derive(Subcommand)]
enum LinkCmd {
    Request { org: String },
    Accept { org: String },
    Ls,
    Revoke { org: String },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// A node token (claim/changes/events for any seat of the org).
    Node {
        #[arg(long)]
        label: Option<String>,
    },
    /// An additional user token.
    User {
        #[arg(long)]
        label: Option<String>,
    },
}

#[derive(Subcommand)]
enum OrgCmd {
    Show,
    Set {
        #[arg(long)]
        send_rate_per_min: Option<i64>,
        #[arg(long)]
        send_burst: Option<i64>,
        #[arg(long)]
        max_recipients: Option<i64>,
        #[arg(long)]
        max_thread_depth: Option<i64>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Print either the raw JSON or the text rendering.
struct Out {
    json: bool,
}

impl Out {
    fn show(&self, v: &Value, text: impl FnOnce() -> Result<String>) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(v)?);
        } else {
            let t = text()?;
            if !t.is_empty() {
                println!("{t}");
            }
        }
        Ok(())
    }
}

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    Ok(serde_json::from_value(v.clone())?)
}

fn read_body(body: Option<String>) -> Result<String> {
    match body {
        Some(b) => Ok(b),
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("reading body from stdin")?;
            Ok(s)
        }
    }
}

fn lines<T>(items: &[T], f: impl Fn(&T) -> String) -> String {
    items.iter().map(f).collect::<Vec<_>>().join("\n")
}

async fn run(cli: Cli) -> Result<()> {
    let out = Out { json: cli.json };
    match cli.cmd {
        Cmd::Serve {
            db,
            listen,
            max_connections,
        } => return serve(&db, &listen, max_connections).await,
        Cmd::Admin(AdminCmd::CreateOrg { org, user_name, db }) => {
            return create_org(&db, &org, &user_name, &out).await;
        }
        _ => {}
    }
    let c = Client::from_config(cli.profile.as_deref())?;
    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Admin(_) => unreachable!(),
        Cmd::Send {
            to,
            cc,
            subject,
            opts,
        } => {
            let body = json!({
                "to": to, "cc": cc, "subject": subject, "body": read_body(opts.body)?,
                "priority": if opts.urgent { "urgent" } else { "normal" }, "ack_required": opts.ack,
            });
            let v = c.raw(Method::POST, "/messages", Some(&body)).await?;
            out.show(&v, || Ok(output::send_result(&parse(&v)?)))
        }
        Cmd::Inbox { all, seat, limit } => {
            let mut q = format!("limit={limit}");
            if all {
                q.push_str(&format!("&before_seq={}", i64::MAX));
            } else {
                q.push_str("&unread=1");
            }
            if let Some(s) = seat {
                q.push_str(&format!("&seat={}", enc(&s)));
            }
            let v = c.raw(Method::GET, &format!("/inbox?{q}"), None).await?;
            out.show(&v, || {
                let r: p::InboxResponse = parse(&v)?;
                Ok(if r.entries.is_empty() {
                    if all {
                        "no messages".into()
                    } else {
                        "no unread messages".into()
                    }
                } else {
                    lines(&r.entries, output::entry_line)
                })
            })
        }
        Cmd::Read { id, peek } => {
            let path = if peek {
                format!("/messages/{id}?peek=1")
            } else {
                format!("/messages/{id}")
            };
            let v = c.raw(Method::GET, &path, None).await?;
            out.show(&v, || {
                let r: p::MessageResponse = parse(&v)?;
                Ok(output::message(&r.message).trim_end().to_string())
            })
        }
        Cmd::Reply { id, all, opts } => {
            let orig: p::MessageResponse = c.get(&format!("/messages/{id}?peek=1")).await?;
            let me: p::WhoAmI = c.get("/whoami").await?;
            let h = orig.message.header;
            let me = me.seat.unwrap_or_default();
            let mut to: Vec<String> = vec![h.from.clone()];
            let mut cc: Vec<String> = Vec::new();
            if all {
                to.extend(h.to.iter().cloned());
                cc.extend(h.cc.iter().cloned());
            }
            let mut seen = std::collections::HashSet::new();
            to.retain(|a| a != &me && seen.insert(a.clone()));
            cc.retain(|a| a != &me && seen.insert(a.clone()));
            if to.is_empty() {
                // Replying to one's own message: go to its original recipients.
                to = h.to.iter().filter(|a| *a != &me).cloned().collect();
            }
            let subject = if h.subject.to_ascii_lowercase().starts_with("re:") {
                h.subject.clone()
            } else {
                format!("Re: {}", h.subject)
            };
            let body = json!({
                "to": to, "cc": cc, "subject": subject, "body": read_body(opts.body)?, "in_reply_to": id,
                "priority": if opts.urgent { "urgent" } else { "normal" }, "ack_required": opts.ack,
            });
            let v = c.raw(Method::POST, "/messages", Some(&body)).await?;
            out.show(&v, || Ok(output::send_result(&parse(&v)?)))
        }
        Cmd::Ack { id } => {
            let v = c
                .raw(Method::POST, &format!("/messages/{id}/ack"), None)
                .await?;
            out.show(&v, || Ok(format!("acked #{id}")))
        }
        Cmd::Status { id } => {
            let v = c
                .raw(Method::GET, &format!("/messages/{id}/status"), None)
                .await?;
            out.show(&v, || {
                let r: p::StatusResponse = parse(&v)?;
                Ok(lines(&r.recipients, output::status_line))
            })
        }
        Cmd::Thread { id } => {
            let v = match c.raw(Method::GET, &format!("/threads/{id}"), None).await {
                Ok(v) => v,
                Err(e) if is_not_found(&e) => {
                    let m: p::MessageResponse = c.get(&format!("/messages/{id}?peek=1")).await?;
                    c.raw(
                        Method::GET,
                        &format!("/threads/{}", m.message.header.thread_id),
                        None,
                    )
                    .await?
                }
                Err(e) => return Err(e),
            };
            out.show(&v, || {
                let t: p::ThreadResponse = parse(&v)?;
                Ok(t.messages
                    .iter()
                    .map(|m| output::message(m).trim_end().to_string())
                    .collect::<Vec<_>>()
                    .join("\n---\n"))
            })
        }
        Cmd::Who => {
            let seats = c.raw(Method::GET, "/seats", None).await?;
            let lists = c.raw(Method::GET, "/lists", None).await?;
            let links = c.raw(Method::GET, "/links", None).await?;
            let all = json!({ "seats": seats, "lists": lists, "links": links });
            out.show(&all, || {
                let mut text = Vec::new();
                text.extend(parse::<Vec<p::Seat>>(&seats)?.iter().map(output::seat_line));
                text.extend(parse::<Vec<p::List>>(&lists)?.iter().map(output::list_line));
                text.extend(parse::<Vec<p::Link>>(&links)?.iter().map(output::link_line));
                Ok(text.join("\n"))
            })
        }
        Cmd::Claim {
            seat,
            via,
            classes,
            urgent_only,
            include_digest,
            limit,
            inline_body_max,
        } => {
            let classes = output::class_list(&classes).map_err(anyhow::Error::msg)?;
            let body = json!({
                "classes": classes, "via": via, "include_digest": include_digest, "limit": limit,
                "inline_body_max": inline_body_max, "min_priority": if urgent_only { Some("urgent") } else { None },
            });
            let path = match seat {
                Some(s) => format!("/claim?seat={}", enc(&s)),
                None => "/claim".to_string(),
            };
            let v = c.raw(Method::POST, &path, Some(&body)).await?;
            out.show(&v, || {
                let r: p::ClaimResponse = parse(&v)?;
                let mut text = Vec::new();
                for e in &r.entries {
                    text.push(output::entry_line(e));
                    if let Some(b) = &e.body {
                        text.extend(b.lines().map(|l| format!("  {l}")));
                    }
                }
                Ok(text.join("\n"))
            })
        }
        Cmd::Seat(cmd) => seat_cmd(&c, &out, cmd).await,
        Cmd::List(cmd) => list_cmd(&c, &out, cmd).await,
        Cmd::Link(cmd) => link_cmd(&c, &out, cmd).await,
        Cmd::Token(cmd) => {
            let (scope, label) = match cmd {
                TokenCmd::Node { label } => ("node", label),
                TokenCmd::User { label } => ("user", label),
            };
            let v = c
                .raw(
                    Method::POST,
                    "/tokens",
                    Some(&json!({ "scope": scope, "label": label })),
                )
                .await?;
            out.show(&v, || Ok(parse::<p::TokenResponse>(&v)?.token))
        }
        Cmd::Org(OrgCmd::Show) => {
            let v = c.raw(Method::GET, "/org", None).await?;
            out.show(&v, || org_text(&v))
        }
        Cmd::Org(OrgCmd::Set {
            send_rate_per_min,
            send_burst,
            max_recipients,
            max_thread_depth,
        }) => {
            let body = json!({
                "send_rate_per_min": send_rate_per_min, "send_burst": send_burst,
                "max_recipients": max_recipients, "max_thread_depth": max_thread_depth,
            });
            let v = c.raw(Method::PATCH, "/org", Some(&body)).await?;
            out.show(&v, || org_text(&v))
        }
    }
}

fn org_text(v: &Value) -> Result<String> {
    let o: p::OrgInfo = parse(v)?;
    Ok(format!(
        "org {} send_rate_per_min={} send_burst={} max_recipients={} max_thread_depth={}",
        o.name, o.send_rate_per_min, o.send_burst, o.max_recipients, o.max_thread_depth
    ))
}

async fn seat_cmd(c: &Client, out: &Out, cmd: SeatCmd) -> Result<()> {
    match cmd {
        SeatCmd::Add {
            name,
            parent,
            role,
            harness,
            model,
            display_name,
        } => {
            let body = json!({
                "name": name, "parent": parent, "role": role, "harness": harness, "model": model,
                "display_name": display_name,
            });
            let v = c.raw(Method::POST, "/seats", Some(&body)).await?;
            out.show(&v, || Ok(output::seat_line(&parse(&v)?)))
        }
        SeatCmd::Ls => {
            let v = c.raw(Method::GET, "/seats", None).await?;
            out.show(&v, || {
                Ok(lines(&parse::<Vec<p::Seat>>(&v)?, output::seat_line))
            })
        }
        SeatCmd::Rm { name } => {
            let v = c
                .raw(Method::DELETE, &format!("/seats/{}", enc(&name)), None)
                .await?;
            out.show(&v, || Ok(format!("removed seat {name}")))
        }
        SeatCmd::Token { name, label } => {
            let v = c
                .raw(
                    Method::POST,
                    &format!("/seats/{}/tokens", enc(&name)),
                    Some(&json!({ "label": label })),
                )
                .await?;
            out.show(&v, || Ok(parse::<p::TokenResponse>(&v)?.token))
        }
    }
}

async fn list_cmd(c: &Client, out: &Out, cmd: ListCmd) -> Result<()> {
    match cmd {
        ListCmd::Create {
            name,
            policy,
            description,
        } => {
            let policy: PostPolicy = policy.parse().map_err(anyhow::Error::msg)?;
            let body = json!({ "name": name, "post_policy": policy, "description": description });
            let v = c.raw(Method::POST, "/lists", Some(&body)).await?;
            out.show(&v, || Ok(output::list_line(&parse(&v)?)))
        }
        ListCmd::Ls { name: None } => {
            let v = c.raw(Method::GET, "/lists", None).await?;
            out.show(&v, || {
                Ok(lines(&parse::<Vec<p::List>>(&v)?, output::list_line))
            })
        }
        ListCmd::Ls { name: Some(name) } => {
            let v = c
                .raw(Method::GET, &format!("/lists/{}", enc(&name)), None)
                .await?;
            out.show(&v, || {
                let l: p::List = parse(&v)?;
                let mut text = vec![output::list_line(&l)];
                text.extend(
                    l.members
                        .unwrap_or_default()
                        .iter()
                        .map(output::member_line),
                );
                Ok(text.join("\n"))
            })
        }
        ListCmd::Rm { name } => {
            let v = c
                .raw(Method::DELETE, &format!("/lists/{}", enc(&name)), None)
                .await?;
            out.show(&v, || Ok(format!("removed list {name}")))
        }
        ListCmd::Add {
            list,
            addresses,
            mode,
        } => {
            let mode: DeliveryMode = mode.parse().map_err(anyhow::Error::msg)?;
            let mut results = Vec::new();
            for a in &addresses {
                let path = format!("/lists/{}/members/{}", enc(&list), enc(a));
                results.push(
                    c.raw(Method::PUT, &path, Some(&json!({ "mode": mode })))
                        .await?,
                );
            }
            let v = Value::Array(results);
            out.show(&v, || {
                Ok(lines(
                    &parse::<Vec<p::ListMember>>(&v)?,
                    output::member_line,
                ))
            })
        }
        ListCmd::Remove { list, addresses } => {
            for a in &addresses {
                c.raw(
                    Method::DELETE,
                    &format!("/lists/{}/members/{}", enc(&list), enc(a)),
                    None,
                )
                .await?;
            }
            out.show(&json!({ "removed": addresses }), || {
                Ok(format!("removed {} from {list}", addresses.join(", ")))
            })
        }
    }
}

async fn link_cmd(c: &Client, out: &Out, cmd: LinkCmd) -> Result<()> {
    let v = match &cmd {
        LinkCmd::Request { org } => {
            c.raw(Method::POST, "/links", Some(&json!({ "org": org })))
                .await?
        }
        LinkCmd::Accept { org } => {
            c.raw(Method::POST, &format!("/links/{}/accept", enc(org)), None)
                .await?
        }
        LinkCmd::Ls => c.raw(Method::GET, "/links", None).await?,
        LinkCmd::Revoke { org } => {
            c.raw(Method::DELETE, &format!("/links/{}", enc(org)), None)
                .await?
        }
    };
    out.show(&v, || {
        Ok(match &cmd {
            LinkCmd::Ls => lines(&parse::<Vec<p::Link>>(&v)?, output::link_line),
            LinkCmd::Revoke { org } => format!("revoked link with {org}"),
            _ => output::link_line(&parse(&v)?),
        })
    })
}

async fn serve(db: &str, listen: &str, max_connections: u32) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let store = amail_store::open(db, &amail_store::OpenOptions { max_connections })
        .await
        .with_context(|| format!("opening {db}"))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    tracing::info!(
        "amail listening on http://{} ({} backend)",
        listener.local_addr()?,
        store.backend()
    );
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    amail_server::serve(listener, amail_server::AppState::new(store), shutdown).await?;
    Ok(())
}

async fn create_org(db: &str, org: &str, user_name: &str, out: &Out) -> Result<()> {
    for n in [org, user_name] {
        if !p::valid_name(n) {
            bail!("invalid name {n:?}: must match [a-z0-9][a-z0-9._-]{{0,62}}");
        }
    }
    let store = amail_store::open(db, &amail_store::OpenOptions { max_connections: 2 })
        .await
        .with_context(|| format!("opening {db}"))?;
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
