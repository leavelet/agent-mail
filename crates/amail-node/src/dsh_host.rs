//! `amail dsh-host`: runs a seat's `dsh web` host (see
//! [`crate::adapter::dsh`]).
//!
//! The wrapper stays the seat's process (the pid the node watches) and the
//! host runs as its child:
//!
//! 1. The host's output is passed through to the terminal. Its start-up
//!    line `dsh web: <url>/?token=<token>` gives the address and the launch
//!    token, which go to the endpoint file (mode 0600).
//! 2. The seat's session is created (or, with `--resume`, loaded) through
//!    the host's API, and the first prompt is queued for a new session.
//! 3. The session log is followed: a `turn/end` that is not `completed`
//!    fires no hook, so it is reported to the node: a failed turn (reason
//!    `error`, with dsh's error message, or `interrupted`, a turn whose host
//!    ended during it) as a `TurnFailed` event, any other (cancelled in the
//!    GUI, ...) as an `Interrupt` event.
//!
//! Signals are passed to the host; the host gets SIGTERM if the wrapper
//! dies. The wrapper exits with the host's status.

use crate::adapter::dsh::{Api, ENDPOINT_ENV, Endpoint, INTERRUPT, TURN_FAILED, transcript_path};
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone)]
pub struct HostArgs {
    pub seat: String,
    pub endpoint: PathBuf,
    /// Load this session instead of creating one.
    pub resume: Option<String>,
    /// The first prompt of a new session (or of a resumed one, when the
    /// node gives one: an adopted session's first start).
    pub prompt: Option<String>,
    /// The seat's model, `provider/model` (a bare name is a DeepSeek model):
    /// selected for a resumed session (a new one takes it from the
    /// overlay; a resumed one keeps the model it had).
    pub model: Option<String>,
    /// The host command (`dsh web ...`).
    pub command: Vec<String>,
    /// Run the host in a bwrap jail (unattended seats).
    pub jail: bool,
    /// Writable paths inside the jail.
    pub rw: Vec<String>,
    /// Read-only paths inside the jail (besides the system).
    pub ro: Vec<String>,
    /// Paths covered by an empty read-only directory or file.
    pub empty_dirs: Vec<String>,
    pub empty_files: Vec<String>,
}

/// The `bwrap` arguments of a seat's jail, before `--` and the command.
///
/// The system is mounted read-only; `/tmp`, the home directory and the
/// user's runtime directory (`$XDG_RUNTIME_DIR`, which holds other
/// programs' sockets) are empty, so nothing in them is visible unless
/// listed; container engine sockets are replaced by `/dev/null` (a Unix
/// socket can be connected to on a read-only mount, and the Docker socket
/// would give root). The listed paths are mounted back, read-only or
/// writable; paths that do not exist are skipped. `ro_files` are
/// `(source, destination)` pairs of single files mounted read-only. The
/// jailed process starts a new session, so it has no controlling terminal
/// (`ioctl(TIOCSTI)` on the launching terminal is not possible). The
/// network is shared (the model API, the amail server).
#[allow(clippy::too_many_arguments)]
pub fn jail_args(
    home: &str,
    runtime_dir: &str,
    rw: &[String],
    ro: &[String],
    ro_files: &[(String, String)],
    empty_dirs: &[String],
    empty_files: &[String],
    empty_file: &str,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        // Its own process namespace: other processes (and their
        // environments) are not visible.
        "--unshare-pid",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--die-with-parent",
        "--new-session",
    ]
    .map(String::from)
    .to_vec();
    for dir in [home, runtime_dir] {
        if !dir.is_empty() && dir != "/" && std::path::Path::new(dir).is_dir() {
            args.extend(["--tmpfs".into(), dir.to_string()]);
        }
    }
    let mut masked = Vec::new();
    for sock in SOCKETS {
        if let Ok(real) = std::fs::canonicalize(sock)
            && !masked.contains(&real)
        {
            args.extend([
                "--ro-bind".into(),
                "/dev/null".into(),
                real.to_string_lossy().into_owned(),
            ]);
            masked.push(real);
        }
    }
    #[derive(Clone, Copy)]
    enum M<'a> {
        Ro,
        Rw,
        RoFrom(&'a str),
        EmptyDir,
        EmptyFile,
    }
    let mut mounts: Vec<(M, &String)> = ro
        .iter()
        .map(|p| (M::Ro, p))
        .chain(rw.iter().map(|p| (M::Rw, p)))
        .filter(|(_, p)| std::path::Path::new(p.as_str()).exists())
        .chain(ro_files.iter().map(|(src, dst)| (M::RoFrom(src), dst)))
        .chain(empty_dirs.iter().map(|p| (M::EmptyDir, p)))
        .chain(empty_files.iter().map(|p| (M::EmptyFile, p)))
        .collect();
    // Parents before children, so a deeper mount is not hidden.
    mounts.sort_by_key(|(_, p)| (p.matches('/').count(), (*p).clone()));
    for (m, p) in mounts {
        let p = p.clone();
        match m {
            M::Ro => args.extend(["--ro-bind".into(), p.clone(), p]),
            M::Rw => args.extend(["--bind".into(), p.clone(), p]),
            M::RoFrom(src) => args.extend(["--ro-bind".into(), src.to_string(), p]),
            M::EmptyDir => args.extend([
                "--perms".into(),
                "0555".into(),
                "--tmpfs".into(),
                p.clone(),
                "--remount-ro".into(),
                p,
            ]),
            // An empty regular file (`/dev/null` would be unreadable:
            // bwrap's bind mounts do not allow device files).
            M::EmptyFile => args.extend(["--ro-bind".into(), empty_file.to_string(), p]),
        }
    }
    args
}

/// Sockets that would let a jailed process act outside the jail.
const SOCKETS: &[&str] = &[
    "/run/docker.sock",
    "/var/run/docker.sock",
    "/run/podman/podman.sock",
    "/run/containerd/containerd.sock",
];

/// The Node.js installation that runs `dsh` (a path a jailed host needs
/// besides those amail lists).
fn node_root() -> Option<String> {
    let node = std::env::var_os("PATH").and_then(|p| {
        p.to_string_lossy()
            .split(':')
            .map(|d| std::path::Path::new(d).join("node"))
            .find(|p| p.is_file())
    })?;
    let real = std::fs::canonicalize(node).ok()?;
    Some(real.parent()?.parent()?.to_string_lossy().into_owned())
}

/// Names of git's credential stores (`git credential-store`), never mounted.
const CREDENTIAL_FILES: &[&str] = &["credentials", ".git-credentials"];

/// The user's git configuration files a jailed host gets, as `(source,
/// destination)` pairs for read-only mounts: `~/.gitconfig` and, in git's
/// configuration directory (`$XDG_CONFIG_HOME/git`, by default
/// `~/.config/git`), `config`, `ignore` and `attributes`. Only these files
/// are mounted, never their directory, so a credential store next to them
/// (`~/.config/git/credentials`, `~/.git-credentials`) stays hidden. A
/// symbolic link is resolved and only the regular file it points to is
/// mounted, at the link's path; a link to a file named like a credential
/// store is skipped. Files that do not exist are skipped.
pub fn git_config_files(home: &str, xdg_config_home: Option<&str>) -> Vec<(String, String)> {
    let config_dir = match xdg_config_home.filter(|d| d.starts_with('/')) {
        Some(d) => format!("{}/git", d.trim_end_matches('/')),
        None => format!("{home}/.config/git"),
    };
    let mut wanted = vec![format!("{home}/.gitconfig")];
    wanted.extend(["config", "ignore", "attributes"].map(|f| format!("{config_dir}/{f}")));
    let mut out = Vec::new();
    for dest in wanted {
        let Ok(real) = std::fs::canonicalize(&dest) else {
            continue;
        };
        let credential = real
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| CREDENTIAL_FILES.contains(&n));
        if !real.is_file() || credential {
            continue;
        }
        out.push((real.to_string_lossy().into_owned(), dest));
    }
    out
}

/// Environment of the launching shell that must not reach the host and its
/// tools and hooks (variables of another harness the seat was started from).
fn foreign_env(key: &str) -> bool {
    key.starts_with("CLAUDE") || key.starts_with("CODEX_")
}

/// `(origin, token)` from a `dsh web: <url>` start-up line.
pub fn parse_url_line(line: &str) -> Option<(String, String)> {
    let rest = line.trim().strip_prefix("dsh web:")?.trim();
    let url = reqwest::Url::parse(rest.split_whitespace().next()?).ok()?;
    let token = url
        .query_pairs()
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())?;
    let origin = url.origin().ascii_serialization();
    Some((origin, token))
}

/// How a turn that did not complete ended (see [`unfinished_turn`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfinished {
    /// Reason `error` (with dsh's message) or `interrupted`.
    Failed(String),
    /// Any other reason (`aborted`: cancelled; `blocked`, ...).
    Other,
}

/// A `turn/end` record whose reason is not `completed`.
pub fn unfinished_turn(line: &str) -> Option<Unfinished> {
    let v = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if v["type"] != "turn/end" {
        return None;
    }
    let reason = &v["data"]["reason"];
    match reason["kind"].as_str()? {
        "completed" => None,
        "error" => {
            let e = &reason["error"];
            let msg = e["message"].as_str().unwrap_or("the turn failed");
            Some(Unfinished::Failed(match e["code"].as_str() {
                Some(code) if !code.is_empty() && code != "UNKNOWN" => format!("{msg} ({code})"),
                _ => msg.to_string(),
            }))
        }
        "interrupted" => Some(Unfinished::Failed(
            "the turn ended without finishing (its host stopped during it)".to_string(),
        )),
        _ => Some(Unfinished::Other),
    }
}

pub async fn run(a: HostArgs) -> i32 {
    match run_inner(a).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("amail dsh-host: {e:#}");
            1
        }
    }
}

async fn run_inner(a: HostArgs) -> Result<i32> {
    let (program, args) = a.command.split_first().context("no host command")?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    // A stale file of an earlier host must not be used for wake-ups.
    let _ = std::fs::remove_file(&a.endpoint);
    let mut cmd = if a.jail {
        // Fail closed: if bwrap cannot run, the host does not run.
        let home = std::env::var("HOME").unwrap_or_default();
        let mut ro = a.ro.clone();
        ro.extend(node_root());
        let xdg_config = std::env::var("XDG_CONFIG_HOME").ok();
        let git_files = git_config_files(&home, xdg_config.as_deref());
        let mut c = tokio::process::Command::new("bwrap");
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        let empty = a.endpoint.with_file_name("empty");
        if !empty.is_file() || std::fs::metadata(&empty).is_ok_and(|m| m.len() > 0) {
            let _ = std::fs::remove_file(&empty);
            std::fs::write(&empty, b"")?;
        }
        std::fs::set_permissions(&empty, std::os::unix::fs::PermissionsExt::from_mode(0o444))?;
        c.args(jail_args(
            &home,
            &runtime,
            &a.rw,
            &ro,
            &git_files,
            &a.empty_dirs,
            &a.empty_files,
            &empty.to_string_lossy(),
        ))
        .arg("--")
        .arg(program)
        .args(args);
        c
    } else {
        let mut c = tokio::process::Command::new(program);
        c.args(args);
        c
    };
    // The host does not read its input; it does not get the terminal the
    // seat was started in.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, _) in std::env::vars_os() {
        if foreign_env(&k.to_string_lossy()) {
            cmd.env_remove(&k);
        }
    }
    // The host ends with the node (Linux: the parent-death signal; the node
    // runs on Linux only).
    // SAFETY: prctl is async-signal-safe; nothing else runs in pre_exec.
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let mut child = cmd.spawn().with_context(|| {
        if a.jail {
            "starting bwrap (unattended dsh seats run in a bwrap jail; install bubblewrap)"
                .to_string()
        } else {
            format!("starting {program}")
        }
    })?;
    let child_pid = child.id().context("host pid")? as i32;

    let (url_tx, url_rx) = tokio::sync::oneshot::channel::<(String, String, String)>();
    let stdout = child.stdout.take().context("host stdout")?;
    let stderr = child.stderr.take().context("host stderr")?;
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut tx = Some(url_tx);
        let mut out = tokio::io::stdout();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some((origin, token)) = parse_url_line(&line)
                && let Some(tx) = tx.take()
            {
                let _ = tx.send((origin, token, line.trim().to_string()));
            }
            let _ = out.write_all(format!("{line}\n").as_bytes()).await;
            let _ = out.flush().await;
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut err = tokio::io::stderr();
        while let Ok(Some(line)) = lines.next_line().await {
            let _ = err.write_all(format!("{line}\n").as_bytes()).await;
        }
    });
    {
        use tokio::signal::unix::{SignalKind, signal};
        for kind in [
            SignalKind::terminate(),
            SignalKind::interrupt(),
            SignalKind::hangup(),
        ] {
            if let Ok(mut s) = signal(kind) {
                tokio::spawn(async move {
                    while s.recv().await.is_some() {
                        // SAFETY: plain signal to our child.
                        unsafe { libc::kill(child_pid, kind.as_raw_value()) };
                    }
                });
            }
        }
    }

    let setup = {
        let a = a.clone();
        let cwd = cwd.clone();
        tokio::spawn(async move {
            let Ok((origin, token, line)) = url_rx.await else {
                return;
            };
            match attach(&a, &cwd, &origin, &token).await {
                Ok(session) => {
                    println!(
                        "amail: seat {} runs dsh session {session}; open {} in a browser to use it",
                        a.seat,
                        line.trim_start_matches("dsh web:").trim()
                    );
                    follow_turns(&a, &cwd, &session).await;
                }
                Err(e) => eprintln!("amail dsh-host: seat {}: {e:#}", a.seat),
            }
        })
    };

    let status = child.wait().await?;
    setup.abort();
    if Endpoint::read(&a.endpoint).is_some_and(|ep| ep.pid == std::process::id()) {
        let _ = std::fs::remove_file(&a.endpoint);
    }
    use std::os::unix::process::ExitStatusExt;
    Ok(match (status.code(), status.signal()) {
        (Some(c), _) => c,
        (None, Some(s)) => 128 + s,
        _ => 1,
    })
}

/// Create or load the seat's session, publish the endpoint, queue the first
/// prompt. Returns the session id.
async fn attach(a: &HostArgs, cwd: &str, origin: &str, token: &str) -> Result<String> {
    let mut ep = Endpoint {
        pid: std::process::id(),
        url: origin.to_string(),
        token: token.to_string(),
        session: None,
        cwd: cwd.to_string(),
    };
    // Published before the session exists, so the session's SessionStart
    // hook can find the wrapper's pid.
    ep.write(&a.endpoint)?;
    let api = Api::login(origin, token).await?;
    let mut last = None;
    let mut session = None;
    for _ in 0..10 {
        match api.create(cwd, a.resume.as_deref()).await {
            Ok(s) => {
                session = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let Some(session) = session else {
        bail!(
            "could not {} the session: {:#}",
            if a.resume.is_some() { "load" } else { "create" },
            last.unwrap_or_else(|| anyhow::anyhow!("unknown error"))
        );
    };
    ep.session = Some(session.clone());
    ep.write(&a.endpoint)?;
    if a.resume.is_some()
        && let Some(m) = a.model.as_deref().filter(|m| !m.is_empty())
    {
        let (provider, model) = crate::adapter::dsh::split_model(m);
        if let Err(e) = api.select_model(&session, provider, model).await {
            eprintln!(
                "amail: the resumed session keeps its earlier model; selecting {m} failed: {e:#}"
            );
        }
    }
    // A resumed session gets a prompt only when the node gives one (an
    // adopted session's first start).
    if let Some(p) = a.prompt.as_deref().filter(|p| !p.is_empty()) {
        api.prompt(&session, p).await?;
    }
    Ok(session)
}

/// Follow the session log from its current end; report unfinished turns.
async fn follow_turns(a: &HostArgs, cwd: &str, session: &str) {
    let home = std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::home().join(".dsh"));
    let path = transcript_path(&home, cwd, session);
    let mut offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut partial = String::new();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Ok(mut f) = tokio::fs::File::open(&path).await else {
            continue;
        };
        let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
        if len < offset {
            offset = 0;
            partial.clear();
        }
        if len == offset || f.seek(std::io::SeekFrom::Start(offset)).await.is_err() {
            continue;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).await.is_err() {
            continue;
        }
        offset += buf.len() as u64;
        partial.push_str(&String::from_utf8_lossy(&buf));
        while let Some(i) = partial.find('\n') {
            let line: String = partial.drain(..=i).collect();
            if let Some(u) = unfinished_turn(&line) {
                report_turn_end(a, cwd, session, u).await;
            }
        }
    }
}

async fn report_turn_end(a: &HostArgs, cwd: &str, session: &str, how: Unfinished) {
    let input = match how {
        Unfinished::Failed(reason) => serde_json::json!({
            "hook_event_name": TURN_FAILED, "session_id": session, "cwd": cwd,
            "reason": reason,
        }),
        Unfinished::Other => serde_json::json!({
            "hook_event_name": INTERRUPT, "session_id": session, "cwd": cwd,
        }),
    };
    let env = std::collections::BTreeMap::from([
        ("AMAIL_SEAT".to_string(), a.seat.clone()),
        (
            ENDPOINT_ENV.to_string(),
            a.endpoint.to_string_lossy().into_owned(),
        ),
    ]);
    let req = crate::wire::Request::Hook {
        harness: "dsh".into(),
        env,
        input: input.to_string(),
        budget_ms: None,
    };
    let socket = crate::paths::socket();
    let r = tokio::task::spawn_blocking(move || {
        crate::node::request(&socket, &req, Duration::from_secs(5))
    })
    .await;
    if !matches!(r, Ok(Ok(_))) {
        eprintln!("amail dsh-host: could not report the end of a turn to the node");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_line() {
        assert_eq!(
            parse_url_line("dsh web: http://127.0.0.1:46029/?token=ab-c_d"),
            Some(("http://127.0.0.1:46029".into(), "ab-c_d".into()))
        );
        assert_eq!(parse_url_line("dsh web: http://127.0.0.1:1/"), None);
        assert_eq!(parse_url_line("something else"), None);
    }

    #[test]
    fn unfinished_turns() {
        assert_eq!(
            unfinished_turn(r#"{"type":"turn/end","data":{"reason":{"kind":"aborted"}}}"#),
            Some(Unfinished::Other)
        );
        assert_eq!(
            unfinished_turn(r#"{"type":"turn/end","data":{"reason":{"kind":"error"}}}"#),
            Some(Unfinished::Failed("the turn failed".into()))
        );
        assert_eq!(
            unfinished_turn(
                r#"{"type":"turn/end","seq":5,"data":{"turn":3,"reason":{"kind":"error","error":{"message":"no API key for provider route","code":"AUTH"}},"new":1}}"#
            ),
            Some(Unfinished::Failed(
                "no API key for provider route (AUTH)".into()
            ))
        );
        assert!(matches!(
            unfinished_turn(r#"{"type":"turn/end","data":{"reason":{"kind":"interrupted"}}}"#),
            Some(Unfinished::Failed(_))
        ));
        assert_eq!(
            unfinished_turn(r#"{"type":"turn/end","data":{"reason":{"kind":"completed"}}}"#),
            None
        );
        assert_eq!(unfinished_turn(r#"{"type":"turn/start"}"#), None);
        assert_eq!(unfinished_turn("garbage"), None);
    }

    #[test]
    fn jail() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().into_owned();
        let work = format!("{d}/work");
        let deep = format!("{d}/work/.git");
        std::fs::create_dir_all(&deep).unwrap();
        let args = jail_args(
            &d,
            "/nonexistent-runtime-dir",
            &[deep.clone(), work.clone(), format!("{d}/missing")],
            std::slice::from_ref(&d),
            &[("/src/gitconfig".into(), format!("{d}/.gitconfig"))],
            &[format!("{deep}/hooks")],
            &[format!("{deep}/config")],
            "/e",
        );
        let s = args.join(" ");
        assert!(s.starts_with("--ro-bind / / --dev /dev --unshare-pid --proc /proc --tmpfs /tmp"));
        assert!(s.contains(&format!("--tmpfs {d} ")));
        assert!(!s.contains("nonexistent-runtime-dir"));
        if std::path::Path::new("/run/docker.sock").exists() {
            assert!(s.contains("--ro-bind /dev/null /run/docker.sock"), "{s}");
        }
        let pos = |x: &str| s.find(x).unwrap();
        assert!(pos(&format!("--ro-bind {d} {d}")) < pos(&format!("--bind {work} {work}")));
        assert!(pos(&format!("--bind {work} {work}")) < pos(&format!("--bind {deep} {deep}")));
        assert!(!s.contains("missing"));
        assert!(s.contains(&format!("--tmpfs {deep}/hooks --remount-ro {deep}/hooks")));
        assert!(s.contains(&format!("--ro-bind /e {deep}/config")));
        assert!(pos(&format!("--bind {deep} {deep}")) < pos(&format!("{deep}/hooks")));
        assert!(s.contains(&format!("--ro-bind /src/gitconfig {d}/.gitconfig")));
        assert!(pos(&format!("--tmpfs {d} ")) < pos("/src/gitconfig"));
        // No controlling terminal inside the jail.
        assert!(args.iter().any(|a| a == "--new-session"), "{s}");
    }

    /// A fake home with git's configuration, a credential store next to it,
    /// and symbolic links (a dotfiles directory; a link to a credential
    /// store).
    fn fake_git_home() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let h = dir.path().canonicalize().unwrap();
        let w = |p: &str, c: &str| {
            let p = h.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        };
        w(".config/git/config", "[core]\n");
        w(".config/git/attributes", "*.bin binary\n");
        w(
            ".config/git/credentials",
            "https://u:fake-token@example.com\n",
        );
        w(".git-credentials", "https://u:fake-token2@example.com\n");
        w("dotfiles/gitconfig", "[user]\n");
        std::os::unix::fs::symlink("dotfiles/gitconfig", h.join(".gitconfig")).unwrap();
        // `ignore` points at a credential store: not mounted.
        std::os::unix::fs::symlink("../../.git-credentials", h.join(".config/git/ignore")).unwrap();
        let home = h.to_string_lossy().into_owned();
        (dir, home)
    }

    #[test]
    fn git_config_only() {
        let (_dir, h) = fake_git_home();
        let files = git_config_files(&h, None);
        assert_eq!(
            files,
            vec![
                (format!("{h}/dotfiles/gitconfig"), format!("{h}/.gitconfig")),
                (
                    format!("{h}/.config/git/config"),
                    format!("{h}/.config/git/config")
                ),
                (
                    format!("{h}/.config/git/attributes"),
                    format!("{h}/.config/git/attributes")
                ),
            ]
        );
        let s = jail_args(&h, "", &[], &[], &files, &[], &[], "/e").join(" ");
        assert!(!s.contains("credentials"), "{s}");
        assert!(!s.contains(&format!("{h}/.config/git ")), "{s}");
        assert!(!s.contains("ignore"), "{s}");
        // `$XDG_CONFIG_HOME/git` instead of `~/.config/git` when set.
        let xdg = git_config_files(&h, Some(&format!("{h}/dotfiles/")));
        assert_eq!(xdg.len(), 1);
        std::fs::create_dir_all(format!("{h}/xdg/git")).unwrap();
        std::fs::write(format!("{h}/xdg/git/config"), "").unwrap();
        std::fs::write(format!("{h}/xdg/git/credentials"), "x").unwrap();
        let xdg = git_config_files(&h, Some(&format!("{h}/xdg")));
        assert_eq!(
            xdg[1],
            (format!("{h}/xdg/git/config"), format!("{h}/xdg/git/config"))
        );
        assert_eq!(xdg.len(), 2);
    }

    /// In a real jail with a fake home: git's configuration is readable,
    /// its credential stores are not, and there is no controlling terminal.
    #[test]
    fn git_credentials_hidden_in_jail() {
        let ok = std::process::Command::new("bwrap")
            .args(["--ro-bind", "/", "/", "--", "true"])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("bwrap cannot run here: jail check skipped");
            return;
        }
        let (_dir, h) = fake_git_home();
        let files = git_config_files(&h, None);
        let args = jail_args(&h, "", &[], &[], &files, &[], &[], "/e");
        let script = format!(
            "for f in .config/git/config .gitconfig .config/git/attributes \
               .config/git/credentials .git-credentials .config/git/ignore dotfiles/gitconfig; do \
               if cat {h}/$f >/dev/null 2>&1; then echo \"$f readable\"; else echo \"$f hidden\"; fi; done; \
             if (: </dev/tty) 2>/dev/null; then echo tty; else echo no-tty; fi"
        );
        let out = std::process::Command::new("bwrap")
            .args(&args)
            .args(["--", "sh", "-c", &script])
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [
                ".config/git/config readable",
                ".gitconfig readable",
                ".config/git/attributes readable",
                ".config/git/credentials hidden",
                ".git-credentials hidden",
                ".config/git/ignore hidden",
                "dotfiles/gitconfig hidden",
                "no-tty",
            ],
            "{out}"
        );
    }

    #[test]
    fn foreign() {
        assert!(foreign_env("CLAUDE_CODE_SESSION_ID"));
        assert!(foreign_env("CLAUDECODE"));
        assert!(foreign_env("CODEX_THREAD_ID"));
        assert!(!foreign_env("AMAIL_SEAT"));
        assert!(!foreign_env("DEEPSEEK_API_KEY"));
    }
}
