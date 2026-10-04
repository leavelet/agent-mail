//! `amail node start` / `stop`: run the node in the background, with its
//! log in `<state dir>/node.log` and its process id in
//! `<state dir>/node.pid`. (For a systemd user unit, see the operations
//! guide.)

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn pid_file(state: &Path) -> PathBuf {
    state.join("node.pid")
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn read_pid(state: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_file(state))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|p| *p > 0)
}

/// Start `amail node <args>` detached. `token`: passed in the environment,
/// not on the command line.
pub fn start(
    state: &Path,
    socket: &Path,
    args: Vec<String>,
    token: Option<String>,
) -> Result<String> {
    if let Some(pid) = read_pid(state).filter(|p| alive(*p)) {
        bail!("the node is already running (pid {pid}); amail node stop first");
    }
    if amail_node::request(
        socket,
        &amail_node::wire::Request::Status,
        Duration::from_secs(2),
    )
    .is_ok()
    {
        bail!(
            "a node already answers on {} (started some other way)",
            socket.display()
        );
    }
    std::fs::create_dir_all(state).with_context(|| format!("creating {}", state.display()))?;
    let log_path = state.join("node.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let exe = std::env::current_exe().context("finding the amail binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("node").args(&args);
    cmd.env("AMAIL_STATE_DIR", state);
    cmd.env("AMAIL_NODE_SOCK", socket);
    if let Some(t) = token {
        cmd.env("AMAIL_NODE_TOKEN", t);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid is async-signal-safe; the child leaves our
        // session, so closing this terminal does not stop it.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().context("starting the node")?;
    let pid = child.id();
    amail_node::persist::write_atomic(&pid_file(state), format!("{pid}\n").as_bytes(), 0o600)?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(pid_file(state));
            bail!(
                "the node exited ({status}); see {}:\n{}",
                log_path.display(),
                tail(&log_path, 10)
            );
        }
        if amail_node::request(
            socket,
            &amail_node::wire::Request::Status,
            Duration::from_secs(1),
        )
        .is_ok()
        {
            return Ok(format!(
                "node started (pid {pid}), socket {}, log {}",
                socket.display(),
                log_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!(
        "the node (pid {pid}) does not answer on {} yet; see {}",
        socket.display(),
        log_path.display()
    )
}

/// Stop the node started with [`start`].
pub fn stop(state: &Path) -> Result<String> {
    let Some(pid) = read_pid(state) else {
        bail!(
            "no {} (was the node started with amail node start?)",
            pid_file(state).display()
        );
    };
    if !alive(pid) {
        let _ = std::fs::remove_file(pid_file(state));
        return Ok(format!("the node (pid {pid}) was not running"));
    }
    // SAFETY: SIGTERM to the recorded process.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if !alive(pid) {
            let _ = std::fs::remove_file(pid_file(state));
            return Ok(format!("node stopped (pid {pid})"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("the node (pid {pid}) did not stop within 15 s")
}

fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
