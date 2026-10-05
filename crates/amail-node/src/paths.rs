//! Default locations of the node socket and state directory.

use std::path::{Path, PathBuf};

/// Environment variable naming the node socket.
pub const SOCK_ENV: &str = "AMAIL_NODE_SOCK";
/// Environment variable naming the node state directory.
pub const STATE_ENV: &str = "AMAIL_STATE_DIR";

pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `$AMAIL_NODE_SOCK`, else `$XDG_RUNTIME_DIR/amail/node.sock`, else
/// `<state dir>/run/node.sock` (see [`state_dir`]). There is no fallback to
/// a shared directory such as `/tmp`, where another user could create the
/// directory first.
pub fn socket() -> PathBuf {
    if let Some(p) = std::env::var_os(SOCK_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|d| PathBuf::from(d).join("amail"))
        .unwrap_or_else(|| state_dir().join("run"));
    base.join("node.sock")
}

fn untrusted(what: &str, path: &Path, why: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!("refusing {what} {}: {why}", path.display()),
    )
}

/// Check that a directory can hold the node socket: a real directory (not a
/// symbolic link) owned by this user that no other user can write to.
pub fn check_socket_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(dir)?;
    if !m.is_dir() {
        return Err(untrusted("socket directory", dir, "not a directory"));
    }
    if m.uid() != uid() {
        return Err(untrusted("socket directory", dir, "owned by another user"));
    }
    if m.mode() & 0o022 != 0 {
        return Err(untrusted(
            "socket directory",
            dir,
            "writable by other users (expected mode 0700)",
        ));
    }
    Ok(())
}

/// Check the node socket before connecting: its directory passes
/// [`check_socket_dir`], and the socket is a socket owned by this user.
pub fn check_node_socket(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    check_socket_dir(dir)?;
    let m = std::fs::symlink_metadata(path)?;
    if !m.file_type().is_socket() {
        return Err(untrusted("node socket", path, "not a socket"));
    }
    if m.uid() != uid() {
        return Err(untrusted("node socket", path, "owned by another user"));
    }
    Ok(())
}

/// The process id and user id of the peer of a connected Unix socket
/// (`SO_PEERCRED`), as they were when it connected.
#[cfg(target_os = "linux")]
pub fn peer_cred(fd: std::os::fd::BorrowedFd<'_>) -> std::io::Result<(u32, u32)> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for writes of the sizes given.
    let r = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((cred.pid as u32, cred.uid))
}

/// The user id of the peer of a connected Unix socket (`getpeereid`); the
/// process id is not known there (0). The node and session supervision
/// run on Linux only; this keeps the socket checks of the other commands.
#[cfg(not(target_os = "linux"))]
pub fn peer_cred(fd: std::os::fd::BorrowedFd<'_>) -> std::io::Result<(u32, u32)> {
    use std::os::fd::AsRawFd;
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: `uid` and `gid` are valid for writes.
    let r = unsafe { libc::getpeereid(fd.as_raw_fd(), &mut uid, &mut gid) };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((0, uid))
}

/// Connect to the node socket at `path`, after [`check_node_socket`], and
/// make sure the process listening on it runs as this user. A socket that
/// another user put in place is refused, so its answers (which can name
/// commands to run) are never used.
pub fn connect_node(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::AsFd;
    check_node_socket(path)?;
    let s = std::os::unix::net::UnixStream::connect(path)?;
    let (_, peer) = peer_cred(s.as_fd())?;
    if peer != uid() {
        return Err(untrusted("node socket", path, "served by another user"));
    }
    Ok(s)
}

/// `$AMAIL_STATE_DIR`, else `$XDG_DATA_HOME/amail`, else
/// `~/.local/share/amail`.
pub fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os(STATE_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("amail");
    }
    home().join(".local/share/amail")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Longest path `connect(2)` accepts for a Unix socket (`sun_path` holds 108
/// bytes including the terminating NUL).
pub const MAX_UNIX_PATH: usize = 107;

/// Connect to the Unix socket at `path`, also when the path is longer than
/// [`MAX_UNIX_PATH`]: the socket's directory is then opened and the socket is
/// reached through `/proc/self/fd/<fd>/<name>`, which is short. (Programs
/// that create sockets under a deep directory, such as Codex's app-server in
/// a node state directory, may still serve them.)
pub async fn connect_unix(path: &std::path::Path) -> std::io::Result<tokio::net::UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    if path.as_os_str().as_bytes().len() <= MAX_UNIX_PATH {
        return tokio::net::UnixStream::connect(path).await;
    }
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return tokio::net::UnixStream::connect(path).await;
    };
    let d = std::fs::File::open(dir)?;
    use std::os::fd::AsRawFd;
    let short = PathBuf::from(format!("/proc/self/fd/{}", d.as_raw_fd())).join(name);
    let r = tokio::net::UnixStream::connect(&short).await;
    drop(d);
    r
}

/// Where a program runs from: `bin` itself when it contains a slash, else
/// the first executable file of that name on `PATH`.
pub fn find_program(bin: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let exec = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if bin.is_empty() {
        return None;
    }
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return exec(&p).then_some(p);
    }
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(bin))
        .find(|p| exec(p))
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn long_socket_paths() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        // Bind through a short relative path from inside the directory.
        let path = deep.join("app.sock");
        assert!(path.as_os_str().len() > super::MAX_UNIX_PATH);
        let short = format!("/proc/self/fd/{}/app.sock", {
            use std::os::fd::AsRawFd;
            let f = std::fs::File::open(&deep).unwrap();
            let fd = f.as_raw_fd();
            std::mem::forget(f);
            fd
        });
        let listener = tokio::net::UnixListener::bind(&short).unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.is_ok() });
        super::connect_unix(&path).await.unwrap();
        assert!(accept.await.unwrap());
        assert!(
            super::connect_unix(&deep.join("missing.sock"))
                .await
                .is_err()
        );
    }
}
