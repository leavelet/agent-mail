//! `amail`: mail and mailing lists for teams of coding agents.

mod client;
mod output;
mod team_cmd;

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
    /// Run the per-host node daemon (or manage it, see `amail node --help`).
    Node(NodeCli),
    /// Hook entry point for a harness (`claude` or `codex`). Reads the hook
    /// input on stdin; never fails and never blocks for long.
    #[command(hide = true)]
    Hook { harness: String },
    /// Start a harness session for a seat in this terminal (through the node).
    Run {
        seat: String,
        /// Override the seat's harness (`claude` or `codex`).
        #[arg(long)]
        harness: Option<String>,
        /// The node socket (default: AMAIL_NODE_SOCK, then the standard path).
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Extra arguments for the harness, after `--`.
        #[arg(last = true)]
        extra: Vec<String>,
    },
    /// Bring a team up from a team file: create or update its seats and
    /// lists, prepare working directories, start the sessions (in zellij,
    /// supervised by the node). Running it again applies only the changes.
    Up {
        /// The team file.
        #[arg(short, long, default_value = "amail.toml")]
        file: PathBuf,
        /// Only start (or place) these groups or seats (comma-separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
        /// Update seats, lists and working directories; start nothing.
        #[arg(long)]
        no_start: bool,
        /// Delete seats (and generated lists) that are no longer in the file.
        #[arg(long)]
        prune: bool,
    },
    /// Stop the team's sessions (seats are kept and go offline).
    Down {
        /// Only these groups or seats (comma-separated).
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
    /// One line per supervised seat: state, pane, pending mail, activity,
    /// and whether a person is needed.
    Ps,
    /// Attach this terminal to a group's zellij session, focused on a seat.
    Attach { target: String },
    /// End a seat's process but keep its session; mail starts it again.
    Sleep { seat: String },
    /// Start a sleeping or stopped seat now.
    Wake { seat: String },
}

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
struct NodeCli {
    #[command(subcommand)]
    cmd: Option<NodeCmd>,
    #[command(flatten)]
    run: NodeArgs,
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Issue a node token (needs a user token). With --save, also write it
    /// with the server URL to <state dir>/node.toml for `amail node`.
    Token {
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        save: bool,
        #[arg(long, env = "AMAIL_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
    /// Show the running node's view of its seats.
    Status,
}

#[derive(Args)]
struct NodeArgs {
    /// Server URL (default: AMAIL_URL, then <state dir>/node.toml).
    #[arg(long)]
    url: Option<String>,
    /// Node token (default: AMAIL_NODE_TOKEN, then <state dir>/node.toml).
    #[arg(long, env = "AMAIL_NODE_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// State directory (default: ~/.local/share/amail).
    #[arg(long, env = "AMAIL_STATE_DIR")]
    state_dir: Option<PathBuf>,
    /// Node socket (default: $XDG_RUNTIME_DIR/amail/node.sock).
    #[arg(long, env = "AMAIL_NODE_SOCK")]
    socket: Option<PathBuf>,
    /// Name of this host in presence reports (default: the hostname).
    #[arg(long)]
    label: Option<String>,
    #[arg(long, env = "AMAIL_CLAUDE_BIN", default_value = "claude")]
    claude_bin: String,
    #[arg(long, env = "AMAIL_CODEX_BIN", default_value = "codex")]
    codex_bin: String,
    #[arg(long, env = "AMAIL_ZELLIJ_BIN", default_value = "zellij")]
    zellij_bin: String,
    /// Most supervised sessions starting at once.
    #[arg(long, default_value_t = 4)]
    start_limit: usize,
    /// auth.json that the node's Codex home links to (default: ~/.codex/auth.json).
    #[arg(long, env = "AMAIL_CODEX_AUTH")]
    codex_auth: Option<PathBuf>,
    /// Coalescing window for wake-ups, in milliseconds.
    #[arg(long, default_value_t = 3000)]
    wake_window_ms: u64,
    /// How long to wait for a wake-up to start a turn before retrying, in milliseconds.
    #[arg(long, default_value_t = 10000)]
    wake_confirm_ms: u64,
}

#[derive(Args)]
struct SendOpts {
    /// Message body; read from stdin when omitted.
    #[arg(short = 'm', long = "message")]
    body: Option<String>,
    /// Structured field `key=value` (repeatable), shown to recipients.
    #[arg(long = "meta", value_name = "KEY=VALUE")]
    meta: Vec<String>,
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
        /// `default` or `unattended` (run without permission prompts).
        #[arg(long)]
        permission_mode: Option<String>,
    },
    /// Change a seat's settings.
    Set {
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
        #[arg(long)]
        permission_mode: Option<String>,
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
    /// List live tokens (labels and scopes; never the secrets).
    Ls,
    /// Revoke a token by id (see `amail token ls`).
    Revoke { id: i64 },
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
    // The hook client runs on every harness hook: no runtime, no failures.
    if let Cmd::Hook { harness } = &cli.cmd {
        let out = amail_node::hook_client::run(harness);
        if !out.is_empty() {
            use std::io::Write;
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(out.as_bytes());
            let _ = stdout.flush();
        }
        return ExitCode::SUCCESS;
    }
    if let Cmd::Run {
        seat,
        harness,
        socket,
        extra,
    } = &cli.cmd
    {
        return match run_seat(seat, harness.clone(), socket.clone(), extra.clone()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        };
    }
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
pub(crate) struct Out {
    json: bool,
}

impl Out {
    pub(crate) fn show(&self, v: &Value, text: impl FnOnce() -> Result<String>) -> Result<()> {
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
        Cmd::Node(NodeCli { cmd: None, run }) => return node_daemon(run).await,
        Cmd::Down { only } => return team_cmd::down(&only, &out),
        Cmd::Ps => return team_cmd::ps(&out),
        Cmd::Attach { target } => return team_cmd::attach(&target),
        Cmd::Sleep { seat } => return team_cmd::sleep(&seat, &out),
        Cmd::Wake { seat } => return team_cmd::wake(&seat, &out),
        Cmd::Node(NodeCli {
            cmd: Some(NodeCmd::Status),
            ..
        }) => {
            let v = amail_node::request(
                &amail_node::paths::socket(),
                &amail_node::wire::Request::Status,
                std::time::Duration::from_secs(5),
            )?;
            return out.show(&v, || node_status_text(&v));
        }
        _ => {}
    }
    let c = Client::from_config(cli.profile.as_deref())?;
    match cli.cmd {
        Cmd::Serve { .. }
        | Cmd::Admin(_)
        | Cmd::Hook { .. }
        | Cmd::Run { .. }
        | Cmd::Down { .. }
        | Cmd::Ps
        | Cmd::Attach { .. }
        | Cmd::Sleep { .. }
        | Cmd::Wake { .. }
        | Cmd::Node(NodeCli { cmd: None, .. })
        | Cmd::Node(NodeCli {
            cmd: Some(NodeCmd::Status),
            ..
        }) => unreachable!(),
        Cmd::Node(NodeCli {
            cmd:
                Some(NodeCmd::Token {
                    label,
                    save,
                    state_dir,
                }),
            ..
        }) => {
            let v = c
                .raw(
                    Method::POST,
                    "/tokens",
                    Some(&json!({ "scope": "node", "label": label })),
                )
                .await?;
            let token = parse::<p::TokenResponse>(&v)?.token;
            if save {
                let dir = state_dir.unwrap_or_else(amail_node::paths::state_dir);
                std::fs::create_dir_all(&dir)?;
                let path = dir.join("node.toml");
                let text = toml::to_string(&NodeFile {
                    url: Some(c.base().to_string()),
                    token: Some(token.clone()),
                    label: None,
                })?;
                amail_node::persist::write_atomic(&path, text.as_bytes(), 0o600)?;
                eprintln!("saved to {}", path.display());
            }
            out.show(&v, || Ok(token))
        }
        Cmd::Send {
            to,
            cc,
            subject,
            opts,
        } => {
            let body = json!({
                "to": to, "cc": cc, "subject": subject, "meta": parse_meta(&opts.meta)?,
                "body": read_body(opts.body)?,
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
                "to": to, "cc": cc, "subject": subject, "meta": parse_meta(&opts.meta)?,
                "body": read_body(opts.body)?, "in_reply_to": id,
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
        Cmd::Up {
            file,
            only,
            no_start,
            prune,
        } => {
            team_cmd::up(
                &c,
                &out,
                &team_cmd::UpOpts {
                    file,
                    only,
                    no_start,
                    prune,
                },
            )
            .await
        }
        Cmd::Seat(cmd) => seat_cmd(&c, &out, cmd).await,
        Cmd::List(cmd) => list_cmd(&c, &out, cmd).await,
        Cmd::Link(cmd) => link_cmd(&c, &out, cmd).await,
        Cmd::Token(TokenCmd::Ls) => {
            let v = c.raw(Method::GET, "/tokens", None).await?;
            out.show(&v, || {
                Ok(lines(&parse::<Vec<p::TokenInfo>>(&v)?, output::token_line))
            })
        }
        Cmd::Token(TokenCmd::Revoke { id }) => {
            let v = c
                .raw(Method::DELETE, &format!("/tokens/{id}"), None)
                .await?;
            out.show(&v, || Ok(format!("revoked token {id}")))
        }
        Cmd::Token(cmd) => {
            let (scope, label) = match cmd {
                TokenCmd::Node { label } => ("node", label),
                TokenCmd::User { label } => ("user", label),
                TokenCmd::Ls | TokenCmd::Revoke { .. } => unreachable!(),
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
            permission_mode,
        } => {
            let pm = permission_mode
                .map(|m| m.parse::<p::PermissionMode>())
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let body = json!({
                "name": name, "parent": parent, "role": role, "harness": harness, "model": model,
                "display_name": display_name, "permission_mode": pm,
            });
            let v = c.raw(Method::POST, "/seats", Some(&body)).await?;
            out.show(&v, || Ok(output::seat_line(&parse(&v)?)))
        }
        SeatCmd::Set {
            name,
            parent,
            role,
            harness,
            model,
            display_name,
            permission_mode,
        } => {
            let pm = permission_mode
                .map(|m| m.parse::<p::PermissionMode>())
                .transpose()
                .map_err(anyhow::Error::msg)?;
            let mut body = serde_json::Map::new();
            for (k, v) in [
                ("parent", parent),
                ("role", role),
                ("harness", harness),
                ("model", model),
                ("display_name", display_name),
            ] {
                if let Some(v) = v {
                    body.insert(k.into(), json!(v));
                }
            }
            if let Some(pm) = pm {
                body.insert("permission_mode".into(), json!(pm));
            }
            let v = c
                .raw(
                    Method::PATCH,
                    &format!("/seats/{}", enc(&name)),
                    Some(&Value::Object(body)),
                )
                .await?;
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

fn parse_meta(items: &[String]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut m = std::collections::BTreeMap::new();
    for it in items {
        let (k, v) = it
            .split_once('=')
            .with_context(|| format!("--meta {it:?}: expected key=value"))?;
        m.insert(k.trim().to_string(), v.to_string());
    }
    Ok(m)
}

/// `<state dir>/node.toml`: settings for `amail node`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct NodeFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn node_daemon(a: NodeArgs) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let state_dir = a.state_dir.unwrap_or_else(amail_node::paths::state_dir);
    let file: NodeFile = match std::fs::read_to_string(state_dir.join("node.toml")) {
        Ok(t) => toml::from_str(&t).context("parsing node.toml")?,
        Err(_) => NodeFile::default(),
    };
    let url = a
        .url
        .or_else(|| std::env::var("AMAIL_URL").ok())
        .or(file.url)
        .unwrap_or_else(|| client::DEFAULT_URL.to_string());
    let token = a.token.or(file.token).context(
        "no node token: pass --token, set AMAIL_NODE_TOKEN, or run `amail node token --save`",
    )?;
    let mut cfg = amail_node::Config::new(&url, &token);
    cfg.state_dir = state_dir;
    if let Some(s) = a.socket {
        cfg.socket = s;
    }
    cfg.label = a.label.or(file.label).or_else(hostname);
    cfg.claude_bin = a.claude_bin;
    cfg.codex_bin = a.codex_bin;
    cfg.zellij_bin = a.zellij_bin;
    cfg.start_limit = a.start_limit;
    if let Some(p) = a.codex_auth {
        cfg.codex_auth = p;
    }
    cfg.wake_window = std::time::Duration::from_millis(a.wake_window_ms);
    cfg.wake_confirm = std::time::Duration::from_millis(a.wake_confirm_ms);
    let socket = cfg.socket.clone();
    let handle = amail_node::start(cfg).await?;
    tracing::info!("amail node listening on {} for {url}", socket.display());
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    handle.shutdown();
    Ok(())
}

fn node_status_text(v: &Value) -> Result<String> {
    let mut out = vec![format!(
        "node {} server {} ({} running registered sessions)",
        if v["connected"].as_bool() == Some(true) {
            "connected to"
        } else {
            "NOT connected to"
        },
        v["server"].as_str().unwrap_or("?"),
        v["registered_sessions"]
    )];
    for s in v["seats"].as_array().into_iter().flatten() {
        let p = &s["pending"];
        let mut line = format!(
            "{} {} {} since {} pending to={} cc={} list={} urgent={} wakes={}",
            s["seat"].as_str().unwrap_or("?"),
            s["harness"].as_str().unwrap_or("-"),
            s["state"].as_str().unwrap_or("?"),
            output::short_time(s["since"].as_str().unwrap_or("")),
            p["to"],
            p["cc"],
            p["list"],
            p["urgent"],
            s["wakes"],
        );
        if s["wake_failed"].as_bool() == Some(true) {
            line.push_str(" wake_failed");
        }
        out.push(line);
    }
    Ok(out.join("\n"))
}

/// A terminal whose size is not set yet: zellij re-runs a held pane's
/// command on a pty of size 0x0 and applies the pane's size only once the
/// pane prints something. Print a line, then wait briefly for the size, so
/// the harness starts with the pane's real width.
fn wait_for_terminal_size(seat: &str) {
    let size = || {
        // SAFETY: ioctl(TIOCGWINSZ) only writes into a local struct.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            (libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0).then_some(ws.ws_col)
        }
    };
    // SAFETY: isatty has no preconditions.
    if unsafe { libc::isatty(0) } != 1 || size() != Some(0) {
        return;
    }
    eprintln!("amail: starting seat {seat}");
    for _ in 0..60 {
        if size().is_some_and(|c| c > 0) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// `amail run`: ask the node how to start the seat's harness, then exec it.
fn run_seat(
    seat: &str,
    harness: Option<String>,
    socket: Option<PathBuf>,
    extra: Vec<String>,
) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let socket = socket.unwrap_or_else(amail_node::paths::socket);
    let pane = std::env::var("ZELLIJ_PANE_ID")
        .ok()
        .and_then(|p| p.trim().parse().ok());
    let mux_session = std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    wait_for_terminal_size(seat);
    let v = amail_node::request(
        &socket,
        &amail_node::wire::Request::Launch {
            seat: seat.to_string(),
            cwd,
            pid: std::process::id(),
            harness,
            extra,
            pane,
            mux_session,
        },
        std::time::Duration::from_secs(180),
    )?;
    let plan: amail_node::wire::LaunchPlan = serde_json::from_value(v)?;
    let mut cmd = std::process::Command::new(&plan.program);
    cmd.args(&plan.args);
    for k in &plan.unset_env {
        cmd.env_remove(k);
    }
    cmd.envs(&plan.env);
    if !plan.path_prepend.is_empty() {
        let mut path = plan.path_prepend.join(":");
        if let Some(cur) = std::env::var_os("PATH") {
            path.push(':');
            path.push_str(&cur.to_string_lossy());
        }
        cmd.env("PATH", path);
    }
    let err = cmd.exec();
    Err(anyhow::anyhow!("starting {}: {err}", plan.program))
}
