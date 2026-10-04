//! Getting started: `amail login`, saved profiles, `amail init` and
//! `amail doctor`.

use crate::Out;
use crate::client::Client;
use crate::profiles::{self, Profile};
use amail_proto as p;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read a token: from stdin when it is not a terminal, else prompted for
/// without echo.
pub fn read_secret(prompt: &str) -> Result<String> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim().to_string());
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    // SAFETY: tcgetattr/tcsetattr on stdin with a local termios struct.
    let saved = unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut t) == 0 {
            let mut quiet = t;
            quiet.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &quiet);
            Some(t)
        } else {
            None
        }
    };
    let mut line = String::new();
    let r = stdin.lock().read_line(&mut line);
    if let Some(t) = saved {
        // SAFETY: restores the settings read above.
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &t) };
    }
    eprintln!();
    r?;
    Ok(line.trim().to_string())
}

/// `amail login <url>`: check the token, save it as a profile, use it.
pub async fn login(url: &str, name: Option<String>, out: &Out) -> Result<()> {
    let url = url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("the server URL must start with http:// or https://");
    }
    let token = read_secret("Token (input hidden): ")?;
    if token.is_empty() {
        bail!("no token given");
    }
    let c = Client::new(&url, &token);
    let me: p::WhoAmI = c
        .get("/whoami")
        .await
        .with_context(|| format!("checking the token with {url}"))?;
    let name = name.unwrap_or_else(|| profiles::default_name(&url, &me.org));
    let path = save_profile(
        &name,
        Profile {
            url: url.clone(),
            token,
            org: (!me.org.is_empty()).then(|| me.org.clone()),
            scope: Some(me.scope.to_string()),
        },
    )?;
    let v = json!({ "profile": name, "url": url, "org": me.org, "scope": me.scope, "file": path });
    out.show(&v, || {
        Ok(format!(
            "logged in to {url} as {} of org {} (profile {name}, saved in {})",
            me.seat.as_deref().unwrap_or(me.scope.as_str()),
            if me.org.is_empty() { "-" } else { &me.org },
            path.display()
        ))
    })
}

fn save_profile(name: &str, profile: Profile) -> Result<PathBuf> {
    let mut cfg = profiles::Config::load()?;
    cfg.profiles.insert(name.to_string(), profile);
    cfg.current = Some(name.to_string());
    cfg.save()
}

/// `amail register <url> --invite <code> --org <name>`.
pub async fn register(
    url: &str,
    invite: &str,
    org: &str,
    user_name: Option<String>,
    name: Option<String>,
    out: &Out,
) -> Result<()> {
    let url = url.trim().trim_end_matches('/').to_string();
    let http = reqwest::Client::builder().no_proxy().build()?;
    let resp = http
        .post(format!("{url}/v1/register"))
        .json(&json!({ "invite": invite, "org": org, "user_name": user_name }))
        .send()
        .await
        .with_context(|| format!("connecting to {url}"))?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!(
            "{}: {}",
            v["error"]["code"].as_str().unwrap_or("error"),
            v["error"]["message"].as_str().unwrap_or(status.as_str())
        );
    }
    let r: p::RegisterResponse = serde_json::from_value(v.clone())?;
    let name = name.unwrap_or_else(|| profiles::default_name(&url, &r.org));
    let path = save_profile(
        &name,
        Profile {
            url: url.clone(),
            token: r.token.clone(),
            org: Some(r.org.clone()),
            scope: Some("user".into()),
        },
    )?;
    out.show(&v, || {
        eprintln!(
            "registered org {} with your seat {}; logged in as profile {name} ({}).\n\
             Your user token, shown once (keep it somewhere safe; it is also in that file):",
            r.org,
            r.user_seat,
            path.display()
        );
        Ok(r.token.clone())
    })
}

#[derive(clap::Subcommand)]
pub enum ProfileCmd {
    /// List saved logins (the current one is marked).
    Ls,
    /// Use this login from now on.
    Use { name: String },
    /// Forget a login.
    Rm { name: String },
}

pub fn profile(cmd: ProfileCmd, out: &Out) -> Result<()> {
    let mut cfg = profiles::Config::load()?;
    match cmd {
        ProfileCmd::Ls => {
            let rows: Vec<Value> = cfg
                .profiles
                .iter()
                .map(|(n, p)| {
                    json!({
                        "name": n, "url": p.url, "org": p.org, "scope": p.scope,
                        "current": cfg.current.as_deref() == Some(n.as_str()),
                    })
                })
                .collect();
            out.show(&Value::Array(rows.clone()), || {
                if rows.is_empty() {
                    return Ok("no saved logins (amail login <url>)".into());
                }
                Ok(rows
                    .iter()
                    .map(|r| {
                        format!(
                            "{} {} {} {}",
                            if r["current"] == true { "*" } else { " " },
                            r["name"].as_str().unwrap_or(""),
                            r["url"].as_str().unwrap_or(""),
                            r["scope"].as_str().unwrap_or("")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            })
        }
        ProfileCmd::Use { name } => {
            cfg.select(&name)?;
            cfg.save()?;
            out.show(&json!({ "current": name }), || Ok(format!("using {name}")))
        }
        ProfileCmd::Rm { name } => {
            if cfg.profiles.remove(&name).is_none() {
                bail!("no profile {name:?}");
            }
            if cfg.current.as_deref() == Some(name.as_str()) {
                cfg.current = cfg.profiles.keys().next().cloned();
            }
            cfg.save()?;
            out.show(&json!({ "removed": name }), || {
                Ok(format!(
                    "forgot {name} (the token itself stays valid until revoked)"
                ))
            })
        }
    }
}

/// After `amail token rotate`: update saved profiles that held the old
/// token.
pub fn replace_saved_token(old: &str, new: &str) -> Result<Vec<String>> {
    let mut cfg = profiles::Config::load()?;
    let mut changed = Vec::new();
    for (n, p) in cfg.profiles.iter_mut() {
        if p.token == old {
            p.token = new.to_string();
            changed.push(n.clone());
        }
    }
    if !changed.is_empty() {
        cfg.save()?;
    }
    Ok(changed)
}

/// The sample team file of `amail init`.
pub fn sample_team(dir_name: &str) -> String {
    format!(
        r#"# amail team file: the seats of your team and how this host runs them.
# `amail up` creates the seats on the server and starts their sessions;
# run it again after editing this file. Reference: docs/teams.md.

[team]
# Built-in workflow: every seat gets a git worktree and branch of `repo`;
# workers send merge requests to their lead, leads to the root, and the
# root merges into the main branch.
template = "staged-merge"
repo = "."                          # relative to this file
workroot = "../{dir_name}-seats"     # the seats' worktrees go here
idle_sleep = "30m"                  # idle seats sleep (no process); mail wakes them
# max_live = 12                     # at most this many seats with a process
# restart = "on-failure"            # or "never"

[defaults]
harness = "claude"                  # or "codex"
model = "sonnet"
# "unattended" runs agents without permission prompts (Claude Code:
# --dangerously-skip-permissions; Codex: -a never with a sandbox). Use it
# only in a repository you can afford to lose; "default" keeps the
# harness's prompts (answer them with `amail attach <seat>`).
permission_mode = "unattended"

[[seat]]
name = "root"
role = "Technical lead: splits the work, reviews and merges into main, reports to the user."

[[group]]
name = "app"
parent = "root"
lead = {{ name = "app-lead", role = "Leads the app group." }}
workers = [
  {{ name = "app-1" }},
  # {{ name = "app-2", harness = "codex", model = "<a Codex model>" }},
]
"#
    )
}

/// `amail init`: write a commented sample team file.
pub fn init(file: &Path, force: bool, out: &Out) -> Result<()> {
    if file.exists() && !force {
        bail!("{} exists (use --force to overwrite)", file.display());
    }
    let dir = std::env::current_dir()?;
    let abs = if file.is_absolute() {
        file.to_path_buf()
    } else {
        dir.join(file)
    };
    let name = abs
        .parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "team".into());
    std::fs::write(file, sample_team(&name))
        .with_context(|| format!("writing {}", file.display()))?;
    out.show(&json!({ "file": file }), || {
        Ok(format!(
            "wrote {}: a root, a lead and a worker using the staged-merge workflow.\n\
             Edit it, then: amail up -f {}",
            file.display(),
            file.display()
        ))
    })
}

// --- doctor --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

struct Report {
    rows: Vec<(Level, String, String, Option<String>)>,
}

impl Report {
    fn add(&mut self, l: Level, what: &str, detail: impl Into<String>, hint: Option<String>) {
        self.rows.push((l, what.into(), detail.into(), hint));
    }
}

/// Harness versions amail has been tested with: the adapters use
/// interfaces the harnesses do not document (Claude Code's session inbox
/// socket, Codex's app-server and hook trust), which may change.
const VERIFIED: &[(&str, &str, &str)] = &[
    ("claude", "2.1.", "Claude Code 2.1.x"),
    ("codex", "0.160.", "codex-cli 0.160.x"),
    ("zellij", "0.45.", "zellij 0.45.x"),
];

fn version_of(bin: &str) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
        .map(str::to_string)
}

fn read_sysctl(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// The repository root Claude Code records trust for: for a git worktree,
/// the main repository; otherwise the directory.
fn trust_root(dir: &Path) -> PathBuf {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let common = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            common
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| dir.to_path_buf())
        }
        _ => dir.to_path_buf(),
    }
}

/// Whether Claude Code's configuration records that the directory is
/// trusted (`None`: cannot tell).
fn claude_trusts(dir: &Path) -> Option<bool> {
    let file = match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d).join(".claude.json"),
        None => amail_node::paths::home().join(".claude.json"),
    };
    let v: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    let key = dir.to_string_lossy();
    Some(
        v["projects"][key.as_ref()]["hasTrustDialogAccepted"]
            .as_bool()
            .unwrap_or(false),
    )
}

/// `amail doctor`: check this host and the configuration; exits non-zero
/// when something is broken.
pub async fn doctor(profile: Option<&Path>, team_file: &Path, out: &Out) -> Result<bool> {
    let mut r = Report { rows: Vec::new() };
    let linux = cfg!(target_os = "linux");
    r.add(
        if linux { Level::Ok } else { Level::Warn },
        "platform",
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        (!linux).then(|| {
            "the node, amail run and amail up support Linux only; mail commands and the server work here".into()
        }),
    );

    // Server and token.
    match Client::from_config(profile) {
        Err(e) => r.add(
            Level::Warn,
            "login",
            format!("{e:#}"),
            Some("amail login <server URL>".into()),
        ),
        Ok(c) => {
            let health = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()?
                .get(format!("{}/healthz", c.base()))
                .send()
                .await;
            match health {
                Ok(resp) if resp.status().is_success() => {
                    r.add(Level::Ok, "server", format!("{} answers", c.base()), None)
                }
                Ok(resp) => r.add(
                    Level::Fail,
                    "server",
                    format!("{} answered HTTP {}", c.base(), resp.status()),
                    None,
                ),
                Err(e) => r.add(
                    Level::Fail,
                    "server",
                    format!("{} is not reachable: {e}", c.base()),
                    Some("is `amail serve` running, and is the URL right?".into()),
                ),
            }
            match c.get::<p::WhoAmI>("/whoami").await {
                Ok(me) => r.add(
                    Level::Ok,
                    "token",
                    format!(
                        "{} token of org {}{}",
                        me.scope,
                        if me.org.is_empty() { "-" } else { &me.org },
                        me.seat.map(|s| format!(", seat {s}")).unwrap_or_default()
                    ),
                    None,
                ),
                Err(e) => r.add(
                    Level::Fail,
                    "token",
                    format!("{e:#}"),
                    Some(
                        "get a new token from the org's user (amail token user) or log in again"
                            .into(),
                    ),
                ),
            }
        }
    }

    // The node.
    let sock = amail_node::paths::socket();
    match amail_node::request(
        &sock,
        &amail_node::wire::Request::Status,
        Duration::from_secs(3),
    ) {
        Ok(v) => r.add(
            if v["connected"] == true {
                Level::Ok
            } else {
                Level::Warn
            },
            "node",
            format!(
                "running ({}), {} to {}",
                sock.display(),
                if v["connected"] == true {
                    "connected"
                } else {
                    "NOT connected"
                },
                v["server"].as_str().unwrap_or("?")
            ),
            (v["connected"] != true)
                .then(|| "see the node's log; is the server reachable from it?".into()),
        ),
        Err(_) => r.add(
            Level::Info,
            "node",
            format!("not running ({})", sock.display()),
            Some("needed to run agents on this host: amail node start".into()),
        ),
    }

    // Harnesses and zellij.
    for (bin, prefix, label) in VERIFIED {
        match version_of(bin) {
            None => r.add(
                Level::Info,
                bin,
                "not found",
                Some(match *bin {
                    "zellij" => "amail up runs sessions in zellij: install it to run teams".into(),
                    _ => format!("install it to run {bin} seats"),
                }),
            ),
            Some(v) if v.starts_with(prefix) => {
                r.add(Level::Ok, bin, format!("{v} (verified: {label})"), None)
            }
            Some(v) => r.add(
                Level::Warn,
                bin,
                format!("{v}, not a verified version ({label})"),
                Some(
                    "amail relies on undocumented harness interfaces; if wake-ups or hooks misbehave, \
                     try a verified version"
                        .into(),
                ),
            ),
        }
    }

    // Codex: the control socket and the sandbox.
    let state = amail_node::paths::state_dir();
    let ctl = state.join("codex-home/app-server-control/app-server-control.sock");
    let len = ctl.as_os_str().len();
    if len > amail_node::paths::MAX_UNIX_PATH {
        r.add(
            if linux { Level::Info } else { Level::Warn },
            "codex socket",
            format!(
                "{} is {len} bytes (> {})",
                ctl.display(),
                amail_node::paths::MAX_UNIX_PATH
            ),
            Some(if linux {
                "fine on Linux (reached through /proc/self/fd); a shorter AMAIL_STATE_DIR avoids it"
                    .into()
            } else {
                "use a shorter AMAIL_STATE_DIR".into()
            }),
        );
    } else {
        r.add(
            Level::Ok,
            "codex socket",
            format!("path length {len} bytes"),
            None,
        );
    }
    if linux {
        let restrict = read_sysctl("/proc/sys/kernel/apparmor_restrict_unprivileged_userns");
        let clone = read_sysctl("/proc/sys/kernel/unprivileged_userns_clone");
        let max = read_sysctl("/proc/sys/user/max_user_namespaces");
        let blocked = restrict.as_deref() == Some("1")
            || clone.as_deref() == Some("0")
            || max.as_deref() == Some("0");
        if blocked {
            r.add(
                Level::Warn,
                "user namespaces",
                format!(
                    "restricted (apparmor_restrict_unprivileged_userns={}, unprivileged_userns_clone={}, max_user_namespaces={})",
                    restrict.as_deref().unwrap_or("-"),
                    clone.as_deref().unwrap_or("-"),
                    max.as_deref().unwrap_or("-")
                ),
                Some(
                    "Codex's sandbox needs them: every sandboxed command fails. Allow them \
                     (sysctl kernel.apparmor_restrict_unprivileged_userns=0) or run Codex seats \
                     without the sandbox"
                        .into(),
                ),
            );
        } else {
            r.add(
                Level::Ok,
                "user namespaces",
                "allowed (Codex's sandbox can start)",
                None,
            );
        }
    }

    // Claude Code's trust of this directory.
    let cwd = std::env::current_dir()?;
    let root = trust_root(&cwd);
    match claude_trusts(&root) {
        Some(true) => r.add(
            Level::Ok,
            "claude trust",
            format!("{} is trusted", root.display()),
            None,
        ),
        Some(false) => r.add(
            Level::Info,
            "claude trust",
            format!("{} is not trusted yet", root.display()),
            Some(
                "Claude Code asks once per repository; answer in any seat's pane \
                 (amail attach <seat>), then amail wake the others"
                    .into(),
            ),
        ),
        None => {}
    }

    // The team file here, if any.
    if team_file.exists() {
        match amail_node::team::load(team_file) {
            Ok(t) => r.add(
                Level::Ok,
                "team file",
                format!("{}: {} seats", team_file.display(), t.seats.len()),
                None,
            ),
            Err(e) => r.add(Level::Fail, "team file", format!("{e}"), None),
        }
    }

    let worst = r.rows.iter().map(|x| x.0).max().unwrap_or(Level::Ok);
    let v = json!(r
        .rows
        .iter()
        .map(|(l, w, d, h)| json!({ "level": format!("{l:?}").to_lowercase(), "check": w, "detail": d, "hint": h }))
        .collect::<Vec<_>>());
    out.show(&v, || {
        Ok(r.rows
            .iter()
            .map(|(l, w, d, h)| {
                let tag = match l {
                    Level::Ok => "ok  ",
                    Level::Info => "info",
                    Level::Warn => "warn",
                    Level::Fail => "FAIL",
                };
                let mut s = format!("{tag} {w}: {d}");
                if let Some(h) = h {
                    s.push_str(&format!("\n     -> {h}"));
                }
                s
            })
            .collect::<Vec<_>>()
            .join("\n"))
    })?;
    Ok(worst < Level::Fail)
}
