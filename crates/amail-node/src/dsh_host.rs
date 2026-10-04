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
//!    (cancelled in the GUI, failed) fires no hook, so it is reported to the
//!    node as an `Interrupt` event.
//!
//! Signals are passed to the host; the host gets SIGTERM if the wrapper
//! dies. The wrapper exits with the host's status.

use crate::adapter::dsh::{Api, ENDPOINT_ENV, Endpoint, INTERRUPT, transcript_path};
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
    /// The first prompt of a new session.
    pub prompt: Option<String>,
    /// The host command (`dsh web ...`).
    pub command: Vec<String>,
    /// Run the host in a bwrap jail (unattended seats).
    pub jail: bool,
    /// Writable paths inside the jail.
    pub rw: Vec<String>,
    /// Read-only paths inside the jail (besides the system).
    pub ro: Vec<String>,
}

/// The `bwrap` arguments of a seat's jail, before `--` and the command.
///
/// The system is mounted read-only; `/tmp`, the home directory and the
/// user's runtime directory (`$XDG_RUNTIME_DIR`, which holds other
/// programs' sockets) are empty, so nothing in them is visible unless
/// listed; container engine sockets are replaced by `/dev/null` (a Unix
/// socket can be connected to on a read-only mount, and the Docker socket
/// would give root). The listed paths are mounted back, read-only or
/// writable; paths that do not exist are skipped. The network is shared
/// (the model API, the amail server).
pub fn jail_args(home: &str, runtime_dir: &str, rw: &[String], ro: &[String]) -> Vec<String> {
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
    let mut mounts: Vec<(&str, &String)> = ro
        .iter()
        .map(|p| ("--ro-bind", p))
        .chain(rw.iter().map(|p| ("--bind", p)))
        .filter(|(_, p)| std::path::Path::new(p.as_str()).exists())
        .collect();
    // Parents before children, so a deeper mount is not hidden.
    mounts.sort_by_key(|(_, p)| (p.matches('/').count(), (*p).clone()));
    for (flag, p) in mounts {
        args.extend([flag.to_string(), p.clone(), p.clone()]);
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

/// Paths a jailed host needs besides those amail lists: the Node.js
/// runtime that runs `dsh` and the user's git configuration.
fn runtime_paths(home: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(node) = std::env::var_os("PATH").and_then(|p| {
        p.to_string_lossy()
            .split(':')
            .map(|d| std::path::Path::new(d).join("node"))
            .find(|p| p.is_file())
    }) && let Ok(real) = std::fs::canonicalize(node)
        && let Some(root) = real.parent().and_then(|b| b.parent())
    {
        out.push(root.to_string_lossy().into_owned());
    }
    for rel in [".gitconfig", ".config/git"] {
        out.push(format!("{home}/{rel}"));
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

/// A `turn/end` record whose reason is not `completed`.
pub fn is_unfinished_turn(line: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    v["type"] == "turn/end"
        && v["data"]["reason"]["kind"]
            .as_str()
            .is_some_and(|k| k != "completed")
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
        ro.extend(runtime_paths(&home));
        let mut c = tokio::process::Command::new("bwrap");
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
        c.args(jail_args(&home, &runtime, &a.rw, &ro))
            .arg("--")
            .arg(program)
            .args(args);
        c
    } else {
        let mut c = tokio::process::Command::new(program);
        c.args(args);
        c
    };
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, _) in std::env::vars_os() {
        if foreign_env(&k.to_string_lossy()) {
            cmd.env_remove(&k);
        }
    }
    // SAFETY: prctl is async-signal-safe; nothing else runs in pre_exec.
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
    if a.resume.is_none()
        && let Some(p) = a.prompt.as_deref().filter(|p| !p.is_empty())
    {
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
            if is_unfinished_turn(&line) {
                report_interrupt(a, cwd, session).await;
            }
        }
    }
}

async fn report_interrupt(a: &HostArgs, cwd: &str, session: &str) {
    let input = serde_json::json!({
        "hook_event_name": INTERRUPT, "session_id": session, "cwd": cwd,
    });
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
        eprintln!("amail dsh-host: could not report an interrupted turn to the node");
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
        assert!(is_unfinished_turn(
            r#"{"type":"turn/end","data":{"reason":{"kind":"aborted"}}}"#
        ));
        assert!(is_unfinished_turn(
            r#"{"type":"turn/end","data":{"reason":{"kind":"error"}}}"#
        ));
        assert!(!is_unfinished_turn(
            r#"{"type":"turn/end","data":{"reason":{"kind":"completed"}}}"#
        ));
        assert!(!is_unfinished_turn(r#"{"type":"turn/start"}"#));
        assert!(!is_unfinished_turn("garbage"));
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
