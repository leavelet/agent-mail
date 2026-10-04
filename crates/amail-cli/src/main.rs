//! `amail`: mail and mailing lists for teams of coding agents.

mod admin;
mod client;
mod onboard;
mod output;
mod profiles;
mod service;
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
    /// A saved login (see `amail profile ls`), or a TOML file with `url`
    /// and `token`. Default: AMAIL_URL / AMAIL_TOKEN, then the current
    /// login.
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
        #[arg(long, env = "AMAIL_LISTEN", default_value = "127.0.0.1:8025")]
        listen: String,
        /// Postgres pool size, or SQLite read pool size.
        #[arg(long, default_value_t = 32)]
        max_connections: u32,
        /// Postgres with several instances: the URL at which the other
        /// instances reach this one (for conversation mirrors). Default:
        /// `http://<listen address>` unless it is a wildcard address.
        #[arg(long, env = "AMAIL_ADVERTISE_URL")]
        advertise_url: Option<String>,
        #[command(flatten)]
        opts: Box<ServeOpts>,
    },
    /// Administration that works directly on the database.
    #[command(subcommand)]
    Admin(admin::AdminCmd),
    /// Log in to a server with a token; saved (mode 0600) under
    /// ~/.config/amail and used by later commands and the node.
    Login {
        /// The server, e.g. https://mail.example.com
        url: String,
        /// Profile name (default: <org>@<host>).
        #[arg(long)]
        name: Option<String>,
    },
    /// Register an org on a public server with an invite code; the new
    /// user token is saved as a login (and printed once).
    Register {
        /// The server, e.g. https://mail.example.com
        url: String,
        #[arg(long)]
        invite: String,
        /// The new org's name.
        #[arg(long)]
        org: String,
        /// Name of your own seat (default: owner).
        #[arg(long)]
        user_name: Option<String>,
        /// Profile name (default: <org>@<host>).
        #[arg(long)]
        name: Option<String>,
    },
    /// Saved logins: list, switch, forget.
    #[command(subcommand)]
    Profile(onboard::ProfileCmd),
    /// Write a commented sample team file (amail.toml) here.
    Init {
        #[arg(short, long, default_value = "amail.toml")]
        file: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Check this host and the configuration, with hints.
    Doctor {
        /// The team file to check, if present.
        #[arg(short, long, default_value = "amail.toml")]
        file: PathBuf,
    },
    /// Exit 0 when the server answers (for container health checks).
    #[command(hide = true)]
    Healthcheck {
        #[arg(default_value = "http://127.0.0.1:8025")]
        url: String,
    },
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
    /// Focus mode: only urgent mail reaches the seat until it ends.
    Focus {
        #[command(subcommand)]
        cmd: FocusCmd,
    },
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

#[derive(Args)]
struct ServeOpts {
    /// Public mode: registration with invite codes, the protective limits
    /// below (with these defaults), reserved names, always-Secure cookies,
    /// no webhooks to private addresses.
    #[arg(long, env = "AMAIL_PUBLIC")]
    public: bool,
    /// Believe X-Forwarded-For/-Host/-Proto from these peers (addresses or
    /// CIDR ranges; repeat or separate with commas), e.g. 127.0.0.1 for a
    /// proxy or tunnel on the same host.
    #[arg(
        long = "trusted-proxy",
        env = "AMAIL_TRUSTED_PROXIES",
        value_delimiter = ','
    )]
    trusted_proxies: Vec<String>,
    /// Whether webhooks may target loopback, private, link-local and
    /// reserved addresses: `allow` or `deny` (default: deny with --public).
    #[arg(long, env = "AMAIL_WEBHOOK_PRIVATE_TARGETS")]
    webhook_private_targets: Option<String>,
    /// Clear message bodies older than this many days (headers stay).
    #[arg(long, env = "AMAIL_RETENTION_DAYS")]
    retention_days: Option<u32>,
    /// Agent seats per org (public default 200; 0: no limit).
    #[arg(long, env = "AMAIL_MAX_SEATS_PER_ORG")]
    max_seats_per_org: Option<i64>,
    /// Messages per second, all orgs, per instance (public default 500).
    #[arg(long, env = "AMAIL_MAX_SENDS_PER_SEC")]
    max_sends_per_sec: Option<f64>,
    /// Messages per second per org (public default 50).
    #[arg(long, env = "AMAIL_MAX_ORG_SENDS_PER_SEC")]
    max_org_sends_per_sec: Option<f64>,
    /// Open event streams per org (public default 32).
    #[arg(long, env = "AMAIL_MAX_ORG_STREAMS")]
    max_org_streams: Option<usize>,
    /// Requests per second per token, bursts of twice that (public
    /// default 200).
    #[arg(long, env = "AMAIL_MAX_TOKEN_REQUESTS_PER_SEC")]
    max_token_requests_per_sec: Option<f64>,
    /// Link requests per hour per org (public default 20).
    #[arg(long, env = "AMAIL_MAX_LINK_REQUESTS_PER_HOUR")]
    max_link_requests_per_hour: Option<f64>,
    /// Registration attempts per hour per client address (public default 10).
    #[arg(long, env = "AMAIL_MAX_REGISTRATIONS_PER_HOUR")]
    max_registrations_per_hour: Option<f64>,
}

impl ServeOpts {
    fn config(&self) -> Result<amail_server::config::Config> {
        use amail_server::config::Config;
        let mut c = if self.public {
            Config::public()
        } else {
            Config::default()
        };
        for p in &self.trusted_proxies {
            if !p.trim().is_empty() {
                c.trusted_proxies.push(
                    p.parse()
                        .map_err(|e: String| anyhow::anyhow!("--trusted-proxy: {e}"))?,
                );
            }
        }
        if let Some(w) = &self.webhook_private_targets {
            c.webhook_private_targets = match w.as_str() {
                "allow" => true,
                "deny" => false,
                _ => bail!("--webhook-private-targets: expected allow or deny"),
            };
        }
        c.retention_days = self.retention_days;
        let l = &mut c.limits;
        let pos_f = |v: f64| (v > 0.0).then_some(v);
        if let Some(v) = self.max_seats_per_org {
            l.seats_per_org = (v > 0).then_some(v);
        }
        if let Some(v) = self.max_sends_per_sec {
            l.global_send_per_sec = pos_f(v);
        }
        if let Some(v) = self.max_org_sends_per_sec {
            l.org_send_per_sec = pos_f(v);
        }
        if let Some(v) = self.max_org_streams {
            l.org_streams = (v > 0).then_some(v);
        }
        if let Some(v) = self.max_token_requests_per_sec {
            l.token_requests_per_sec = pos_f(v);
        }
        if let Some(v) = self.max_link_requests_per_hour {
            l.link_requests_per_hour = pos_f(v);
        }
        if let Some(v) = self.max_registrations_per_hour {
            l.registrations_per_hour = pos_f(v);
        }
        Ok(c)
    }
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Run the node in the background (log and pid in the state
    /// directory). Takes the same options as `amail node`.
    Start {
        #[command(flatten)]
        run: Box<NodeArgs>,
    },
    /// Stop a node started with `amail node start`.
    Stop {
        #[arg(long, env = "AMAIL_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
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
    /// Create a login token for the node's browser terminal
    /// (`--web-terminal-port`); shown once. Kept apart from your own zellij
    /// tokens.
    WebToken {
        /// A token that can only watch sessions.
        #[arg(long)]
        read_only: bool,
        #[arg(long, env = "AMAIL_STATE_DIR")]
        state_dir: Option<PathBuf>,
        #[arg(long, env = "AMAIL_ZELLIJ_BIN", default_value = "zellij")]
        zellij_bin: String,
    },
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
    /// Serve the supervised sessions in the browser: run a `zellij web`
    /// server of the node's own on 127.0.0.1:PORT (off by default). The web
    /// interface then links each seat to its session.
    #[arg(long, value_name = "PORT")]
    web_terminal_port: Option<u16>,
    /// The base URL people use to reach that server (default
    /// http://127.0.0.1:PORT), e.g. behind an HTTPS reverse proxy.
    #[arg(long, value_name = "URL", requires = "web_terminal_port")]
    web_terminal_url: Option<String>,
}

#[derive(Subcommand)]
enum FocusCmd {
    /// Hold all but urgent mail (held mail arrives when the focus ends).
    On {
        /// How long (e.g. 30m, 1h); default and maximum: the org's limit.
        #[arg(long = "for", value_name = "DURATION")]
        duration: Option<String>,
        /// A seat other than your own (user tokens).
        #[arg(long)]
        seat: Option<String>,
    },
    /// End the focus now.
    Off {
        #[arg(long)]
        seat: Option<String>,
    },
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
    /// With --ack: tell me who has not acknowledged after this long (e.g.
    /// 10m; default: the org's ack_timeout_min).
    #[arg(long, value_name = "DURATION", requires = "ack")]
    ack_timeout: Option<String>,
}

impl SendOpts {
    fn ack_timeout_s(&self) -> Result<Option<u64>> {
        match &self.ack_timeout {
            None => Ok(None),
            Some(d) => Ok(Some(
                amail_node::team::parse_duration(d)
                    .map_err(anyhow::Error::msg)?
                    .context("--ack-timeout must be longer than 0")?
                    .as_secs()
                    .max(1),
            )),
        }
    }
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
    /// Replace the user token in use: print a new one, revoke this one
    /// (saved logins that held it are updated).
    Rotate {
        #[arg(long)]
        label: Option<String>,
    },
}

#[derive(Subcommand)]
enum OrgCmd {
    Show,
    /// Delete the org and all its data (seats, mail, lists, links, tokens).
    /// Asks you to type the org's name unless --confirm gives it.
    Delete {
        #[arg(long)]
        confirm: Option<String>,
    },
    Set {
        #[arg(long)]
        send_rate_per_min: Option<i64>,
        #[arg(long)]
        send_burst: Option<i64>,
        #[arg(long)]
        max_recipients: Option<i64>,
        #[arg(long)]
        max_thread_depth: Option<i64>,
        /// Default time to acknowledge before the sender is told.
        #[arg(long)]
        ack_timeout_min: Option<i64>,
        /// Longest focus a seat may declare.
        #[arg(long)]
        focus_max_min: Option<i64>,
        /// An idle seat with digest mail is woken at most this often.
        #[arg(long)]
        digest_interval_min: Option<i64>,
        /// POST a notice (message headers only) here for every new message
        /// to the user.
        #[arg(long, value_name = "URL", conflicts_with = "no_webhook")]
        webhook_url: Option<String>,
        /// Remove the webhook.
        #[arg(long)]
        no_webhook: bool,
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
        return match linux_only("amail run")
            .and_then(|()| run_seat(seat, harness.clone(), socket.clone(), extra.clone()))
        {
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
            advertise_url,
            opts,
        } => return serve(&db, &listen, max_connections, advertise_url, opts.config()?).await,
        Cmd::Admin(cmd) => return admin::run(cmd, &out).await,
        Cmd::Login { url, name } => return onboard::login(&url, name, &out).await,
        Cmd::Register {
            url,
            invite,
            org,
            user_name,
            name,
        } => return onboard::register(&url, &invite, &org, user_name, name, &out).await,
        Cmd::Profile(cmd) => return onboard::profile(cmd, &out),
        Cmd::Init { file, force } => return onboard::init(&file, force, &out),
        Cmd::Doctor { file } => {
            if !onboard::doctor(cli.profile.as_deref(), &file, &out).await? {
                bail!("doctor found problems");
            }
            return Ok(());
        }
        Cmd::Healthcheck { url } => {
            let r = reqwest::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(5))
                .build()?
                .get(format!("{}/healthz", url.trim_end_matches('/')))
                .send()
                .await?;
            if !r.status().is_success() {
                bail!("HTTP {}", r.status());
            }
            return Ok(());
        }
        Cmd::Node(NodeCli { cmd: None, run }) => {
            linux_only("amail node")?;
            return node_daemon(run).await;
        }
        Cmd::Node(NodeCli {
            cmd: Some(NodeCmd::Start { run }),
            ..
        }) => {
            linux_only("amail node")?;
            let state = run
                .state_dir
                .clone()
                .unwrap_or_else(amail_node::paths::state_dir);
            let socket = run.socket.clone().unwrap_or_else(amail_node::paths::socket);
            let (args, token) = node_args(&run);
            let msg = service::start(&state, &socket, args, token)?;
            return out.show(&json!({ "started": true }), || Ok(msg));
        }
        Cmd::Node(NodeCli {
            cmd: Some(NodeCmd::Stop { state_dir }),
            ..
        }) => {
            let state = state_dir.unwrap_or_else(amail_node::paths::state_dir);
            let msg = service::stop(&state)?;
            return out.show(&json!({ "stopped": true }), || Ok(msg));
        }
        Cmd::Up { .. }
        | Cmd::Down { .. }
        | Cmd::Ps
        | Cmd::Attach { .. }
        | Cmd::Sleep { .. }
        | Cmd::Wake { .. }
            if !cfg!(target_os = "linux") =>
        {
            linux_only("amail up, down, ps, attach, sleep and wake")?;
        }
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
        Cmd::Node(NodeCli {
            cmd:
                Some(NodeCmd::WebToken {
                    read_only,
                    state_dir,
                    zellij_bin,
                }),
            ..
        }) => {
            let dir = state_dir.unwrap_or_else(amail_node::paths::state_dir);
            std::fs::create_dir_all(amail_node::node::web_terminal_data_dir(&dir))?;
            let status = amail_node::node::web_terminal_command(&zellij_bin, &dir)
                .arg("web")
                .arg(if read_only {
                    "--create-read-only-token"
                } else {
                    "--create-token"
                })
                .status()
                .with_context(|| format!("running {zellij_bin}"))?;
            if !status.success() {
                bail!("zellij web failed ({status})");
            }
            return Ok(());
        }
        _ => {}
    }
    let c = Client::from_config(cli.profile.as_deref())?;
    match cli.cmd {
        Cmd::Serve { .. }
        | Cmd::Admin(_)
        | Cmd::Login { .. }
        | Cmd::Register { .. }
        | Cmd::Profile(_)
        | Cmd::Init { .. }
        | Cmd::Doctor { .. }
        | Cmd::Healthcheck { .. }
        | Cmd::Hook { .. }
        | Cmd::Run { .. }
        | Cmd::Down { .. }
        | Cmd::Ps
        | Cmd::Attach { .. }
        | Cmd::Sleep { .. }
        | Cmd::Wake { .. }
        | Cmd::Node(NodeCli { cmd: None, .. })
        | Cmd::Node(NodeCli {
            cmd:
                Some(
                    NodeCmd::Status
                    | NodeCmd::WebToken { .. }
                    | NodeCmd::Start { .. }
                    | NodeCmd::Stop { .. },
                ),
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
                "priority": if opts.urgent { "urgent" } else { "normal" }, "ack_required": opts.ack,
                "ack_timeout_s": opts.ack_timeout_s()?, "body": read_body(opts.body)?,
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
                "in_reply_to": id, "ack_timeout_s": opts.ack_timeout_s()?,
                "priority": if opts.urgent { "urgent" } else { "normal" }, "ack_required": opts.ack,
                "body": read_body(opts.body)?,
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
        Cmd::Focus { cmd } => {
            let (method, seat, body) = match cmd {
                FocusCmd::On { duration, seat } => {
                    let for_s = match duration {
                        None => None,
                        Some(d) => Some(
                            amail_node::team::parse_duration(&d)
                                .map_err(anyhow::Error::msg)?
                                .context("--for must be longer than 0")?
                                .as_secs()
                                .max(1),
                        ),
                    };
                    (Method::PUT, seat, Some(json!({ "for_s": for_s })))
                }
                FocusCmd::Off { seat } => (Method::DELETE, seat, None),
            };
            let path = match &seat {
                Some(s) => format!("/focus?seat={}", enc(s)),
                None => "/focus".to_string(),
            };
            let v = c.raw(method, &path, body.as_ref()).await?;
            out.show(&v, || {
                let r: p::FocusResponse = parse(&v)?;
                Ok(match r.focus_until {
                    Some(t) => format!(
                        "{} in focus until {}{}: only urgent mail reaches it",
                        r.seat,
                        output::short_time(&t),
                        if r.capped { " (the org's maximum)" } else { "" }
                    ),
                    None => format!("{} not in focus; held mail arrives now", r.seat),
                })
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
        Cmd::Token(TokenCmd::Rotate { label }) => {
            let v = c
                .raw(
                    Method::POST,
                    "/tokens/rotate",
                    Some(&json!({ "label": label })),
                )
                .await?;
            let new = parse::<p::TokenResponse>(&v)?.token;
            let updated = onboard::replace_saved_token(c.token(), &new)?;
            out.show(&v, || {
                if updated.is_empty() {
                    eprintln!("the old token is revoked; the new user token (shown once):");
                } else {
                    eprintln!(
                        "the old token is revoked; saved login {} now uses the new one:",
                        updated.join(", ")
                    );
                }
                Ok(new.clone())
            })
        }
        Cmd::Token(cmd) => {
            let (scope, label) = match cmd {
                TokenCmd::Node { label } => ("node", label),
                TokenCmd::User { label } => ("user", label),
                TokenCmd::Ls | TokenCmd::Revoke { .. } | TokenCmd::Rotate { .. } => unreachable!(),
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
        Cmd::Org(OrgCmd::Delete { confirm }) => {
            let me: p::WhoAmI = c.get("/whoami").await?;
            let confirm = match confirm {
                Some(c) => c,
                None => {
                    use std::io::IsTerminal;
                    if !std::io::stdin().is_terminal() {
                        bail!("pass --confirm {} to delete the org", me.org);
                    }
                    eprint!(
                        "This deletes org {} with all its seats, mail, lists, links and tokens, \
                         and the mail it sent to linked orgs. Type the org's name to confirm: ",
                        me.org
                    );
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim().to_string()
                }
            };
            if confirm != me.org {
                bail!("not deleted: the name does not match");
            }
            c.raw(Method::DELETE, "/org", Some(&json!({ "confirm": confirm })))
                .await?;
            out.show(&json!({ "deleted": me.org }), || {
                Ok(format!("deleted org {} and all its data", me.org))
            })
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
            ack_timeout_min,
            focus_max_min,
            digest_interval_min,
            webhook_url,
            no_webhook,
        }) => {
            let mut body = json!({
                "send_rate_per_min": send_rate_per_min, "send_burst": send_burst,
                "max_recipients": max_recipients, "max_thread_depth": max_thread_depth,
                "ack_timeout_min": ack_timeout_min, "focus_max_min": focus_max_min,
                "digest_interval_min": digest_interval_min,
            });
            // `webhook_url: null` clears it, so the key is only sent when asked.
            if no_webhook {
                body["webhook_url"] = Value::Null;
            } else if let Some(u) = webhook_url {
                body["webhook_url"] = json!(u);
            }
            let v = c.raw(Method::PATCH, "/org", Some(&body)).await?;
            out.show(&v, || org_text(&v))
        }
    }
}

fn org_text(v: &Value) -> Result<String> {
    let o: p::OrgInfo = parse(v)?;
    let mut s = format!(
        "org {} send_rate_per_min={} send_burst={} max_recipients={} max_thread_depth={} \
         ack_timeout_min={} focus_max_min={} digest_interval_min={}",
        o.name,
        o.send_rate_per_min,
        o.send_burst,
        o.max_recipients,
        o.max_thread_depth,
        o.ack_timeout_min,
        o.focus_max_min,
        o.digest_interval_min
    );
    if let Some(u) = o.webhook_url {
        s.push_str(&format!(" webhook_url={u}"));
    }
    Ok(s)
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

/// The file of a `sqlite:` URL.
fn sqlite_path(db: &str) -> Option<PathBuf> {
    let rest = db
        .strip_prefix("sqlite://")
        .or_else(|| db.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or(rest);
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// SQLite serves one process: hold an exclusive lock on `<db>.serve.lock`
/// for as long as the server runs, so a second `amail serve` on the same
/// file stops with a clear message.
fn lock_sqlite(db: &str) -> Result<Option<std::fs::File>> {
    let Some(path) = sqlite_path(db) else {
        return Ok(None);
    };
    let mut lock = path.clone().into_os_string();
    lock.push(".serve.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)
        .with_context(|| format!("opening {}", PathBuf::from(&lock).display()))?;
    use std::os::fd::AsRawFd;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!(
            "another amail serve is using {} (a SQLite database serves one process; \
             use Postgres to run several instances)",
            path.display()
        );
    }
    Ok(Some(f))
}

async fn serve(
    db: &str,
    listen: &str,
    max_connections: u32,
    advertise_url: Option<String>,
    config: amail_server::config::Config,
) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let _lock = lock_sqlite(db)?;
    let store = amail_store::open(db, &amail_store::OpenOptions { max_connections })
        .await
        .with_context(|| format!("opening {db}"))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    let addr = listener.local_addr()?;
    let mut state = if store.backend() == "postgres" {
        let url = advertise_url
            .or_else(|| (!addr.ip().is_unspecified()).then(|| format!("http://{addr}")));
        if url.is_none() {
            tracing::warn!(
                "no --advertise-url: other instances cannot forward conversation mirrors here"
            );
        }
        amail_server::AppState::clustered(store, db, url)
    } else {
        amail_server::AppState::new(store)
    };
    let public = config.public;
    state.set_config(config);
    tracing::info!(
        "amail listening on http://{addr} ({} backend{}{})",
        state.store.backend(),
        if public { ", public mode" } else { "" },
        state
            .cluster
            .as_ref()
            .map(|c| format!(", instance {}", c.id))
            .unwrap_or_default()
    );
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let shutdown = async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    };
    amail_server::serve(listener, state, shutdown).await?;
    Ok(())
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

/// Commands that need what only Linux offers today (`/proc`, the
/// parent-death signal, the harnesses' local sockets as tested).
fn linux_only(what: &str) -> Result<()> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        bail!(
            "{what} is supported on Linux only for now (this is {}); the server and the mail \
             commands work here. See docs/harnesses.md.",
            std::env::consts::OS
        )
    }
}

/// The command line of `amail node` for `amail node start` (the token goes
/// in the environment instead).
fn node_args(a: &NodeArgs) -> (Vec<String>, Option<String>) {
    let mut v = Vec::new();
    let mut opt = |k: &str, val: Option<String>| {
        if let Some(x) = val {
            v.push(k.to_string());
            v.push(x);
        }
    };
    opt("--url", a.url.clone());
    opt("--label", a.label.clone());
    opt("--claude-bin", Some(a.claude_bin.clone()));
    opt("--codex-bin", Some(a.codex_bin.clone()));
    opt("--zellij-bin", Some(a.zellij_bin.clone()));
    opt("--start-limit", Some(a.start_limit.to_string()));
    opt(
        "--codex-auth",
        a.codex_auth.as_ref().map(|p| p.display().to_string()),
    );
    opt("--wake-window-ms", Some(a.wake_window_ms.to_string()));
    opt("--wake-confirm-ms", Some(a.wake_confirm_ms.to_string()));
    opt(
        "--web-terminal-port",
        a.web_terminal_port.map(|p| p.to_string()),
    );
    opt("--web-terminal-url", a.web_terminal_url.clone());
    (v, a.token.clone())
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
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
    let saved = profiles::Config::load().ok();
    let url = a
        .url
        .or_else(|| std::env::var("AMAIL_URL").ok())
        .or(file.url)
        .or_else(|| {
            saved
                .as_ref()
                .and_then(|c| c.get(None))
                .map(|(_, p)| p.url.clone())
        })
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
    if let Some(port) = a.web_terminal_port {
        cfg.web_terminal = Some(amail_node::node::WebTerminal {
            port,
            base_url: a
                .web_terminal_url
                .unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
        });
    }
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
