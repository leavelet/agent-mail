//! Default locations of the node socket and state directory.

use std::path::PathBuf;

/// Environment variable naming the node socket.
pub const SOCK_ENV: &str = "AMAIL_NODE_SOCK";
/// Environment variable naming the node state directory.
pub const STATE_ENV: &str = "AMAIL_STATE_DIR";

pub fn uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// `$AMAIL_NODE_SOCK`, else `$XDG_RUNTIME_DIR/amail/node.sock`, else
/// `/tmp/amail-<uid>/node.sock`.
pub fn socket() -> PathBuf {
    if let Some(p) = std::env::var_os(SOCK_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|d| PathBuf::from(d).join("amail"))
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/amail-{}", uid())));
    base.join("node.sock")
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
