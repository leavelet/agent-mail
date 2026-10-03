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
