//! The node context of a Codex hook: where the hook finds the node's socket
//! and the Codex pass when its environment does not carry them.
//!
//! The node writes `amail hook codex --node-state-dir <dir> --node-socket
//! <path>` into the Codex home's `config.toml`. Hooks run inside Codex's
//! shared app-server. When the node started that daemon (or a session the
//! node launched started it), its environment holds the pass
//! (`AMAIL_CODEX_PASS`) and the node socket, and the context is not used. A
//! daemon started otherwise (before the node, or by a person's own session
//! in the same home) has neither: the hook then takes the pass from the
//! node's state file and the socket from the context, after checking that
//! the hook runs in that node's Codex home and that both the state file and
//! the socket's peer belong to this user.
//!
//! This is not a security boundary: every process of the user can read the
//! state file (see `docs/security.md`). The node still authenticates the
//! hook by the pass, as for every Codex hook.

use super::NodeContext;
use crate::wire::CODEX_PASS_ENV;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// The pid and uid of the process serving the Unix socket at `path`.
fn peer(path: &Path) -> io::Result<(u32, u32)> {
    // Use a directory descriptor for deep Codex-home paths (a socket path is
    // limited to 108 bytes); Codex's control socket is itself a symlink to
    // its daemon endpoint. Check the resolved socket and its directory,
    // then its kernel peer.
    let path = path.canonicalize()?;
    crate::paths::check_node_socket(&path)?;
    let dir = File::open(
        path.parent()
            .ok_or_else(|| invalid("socket has no directory"))?,
    )?;
    let short = PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())).join(
        path.file_name()
            .ok_or_else(|| invalid("socket has no name"))?,
    );
    let socket = std::os::unix::net::UnixStream::connect(short)?;
    crate::paths::peer_cred(socket.as_fd())
}

/// The Codex pass saved in the node's state file, which must be a private
/// regular file of `uid`.
fn existing_pass(path: &Path, uid: u32) -> io::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(invalid("node state is not a private regular file"));
    }
    // Other fields (the seat tokens among them) are not read.
    #[derive(Deserialize)]
    struct Pass {
        codex_pass: String,
    }
    let value: Pass = serde_json::from_reader(file).map_err(|_| invalid("invalid node state"))?;
    if value.codex_pass.is_empty() {
        return Err(invalid("the node has no Codex pass yet"));
    }
    Ok(value.codex_pass)
}

/// Complete the hook's environment from the node context and return the
/// node socket to use. With the pass already in `env` nothing is read.
/// The seat variables are removed either way: a Codex hook is identified by
/// its pass and thread, never by a seat name or token in the daemon's
/// environment.
pub(super) fn restore(
    context: &NodeContext,
    env: &mut BTreeMap<String, String>,
) -> Result<PathBuf, String> {
    let env_socket = std::env::var_os(crate::paths::SOCK_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let socket = restore_with(context, env, crate::paths::uid(), peer, env_socket);
    // Do not repeat file contents or environment values.
    socket.map_err(|_| "Codex hook context could not be verified".to_string())
}

fn restore_with(
    context: &NodeContext,
    env: &mut BTreeMap<String, String>,
    uid: u32,
    read_peer: impl Fn(&Path) -> io::Result<(u32, u32)>,
    env_socket: Option<PathBuf>,
) -> io::Result<PathBuf> {
    for key in ["AMAIL_SEAT", "AMAIL_TOKEN", "AMAIL_PROFILE"] {
        env.remove(key);
    }
    if env.get(CODEX_PASS_ENV).is_some_and(|p| !p.is_empty()) {
        // The node's variables when the daemon has them; else the socket
        // the command names.
        return Ok(env_socket.unwrap_or_else(|| context.socket.clone()));
    }
    let node = context.state_dir.canonicalize()?;
    let home = node.join("codex-home").canonicalize()?;
    let hook_home = env
        .get("CODEX_HOME")
        .ok_or_else(|| invalid("hook home missing"))?;
    if Path::new(hook_home).canonicalize()? != home {
        return Err(invalid("hook belongs to another Codex home"));
    }
    let (_, peer_uid) = read_peer(&context.socket)?;
    if peer_uid != uid {
        return Err(invalid("node socket belongs to another user"));
    }
    let pass = existing_pass(&node.join("node-state.json"), uid)?;
    env.insert(CODEX_PASS_ENV.to_string(), pass);
    Ok(context.socket.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture {
        _dir: tempfile::TempDir,
        context: NodeContext,
        home: PathBuf,
        state: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let node = dir.path().canonicalize().unwrap().join("node");
            let home = node.join("codex-home");
            let socket = node.join("run/node.sock");
            for path in [&home.join("config.toml"), &socket] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "fixture").unwrap();
            }
            let state = node.join("node-state.json");
            std::fs::write(
                &state,
                r#"{"codex_pass":"existing-node-pass","tokens":{"other":"not-used"}}"#,
            )
            .unwrap();
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
            Self {
                _dir: dir,
                context: NodeContext {
                    state_dir: node,
                    socket,
                },
                home,
                state,
            }
        }

        /// The environment of a hook in the daemon of this home, without
        /// the node's variables.
        fn env(&self) -> BTreeMap<String, String> {
            BTreeMap::from([
                (
                    "CODEX_HOME".to_string(),
                    self.home.to_string_lossy().into_owned(),
                ),
                (
                    "AMAIL_SEAT".to_string(),
                    "must-not-select-a-seat".to_string(),
                ),
                (
                    "AMAIL_TOKEN".to_string(),
                    "must-not-be-forwarded".to_string(),
                ),
            ])
        }

        fn restore(
            &self,
            env: &mut BTreeMap<String, String>,
            peer_uid: u32,
        ) -> io::Result<PathBuf> {
            let uid = crate::paths::uid();
            restore_with(
                &self.context,
                env,
                uid,
                |_| Ok((100, peer_uid)),
                Some(PathBuf::from("/env/node.sock")),
            )
        }
    }

    #[test]
    fn pass_in_the_environment_is_used_as_it_is() {
        let f = Fixture::new();
        let mut env = f.env();
        env.insert(CODEX_PASS_ENV.to_string(), "daemon-pass".to_string());
        // The peer is never asked; the node's variables win.
        let socket = restore_with(
            &f.context,
            &mut env,
            crate::paths::uid(),
            |_| panic!("the context is not read when the pass is present"),
            Some(PathBuf::from("/env/node.sock")),
        )
        .unwrap();
        assert_eq!(socket, PathBuf::from("/env/node.sock"));
        // Without the node's socket variable, the command's socket.
        let mut env = f.env();
        env.insert(CODEX_PASS_ENV.to_string(), "daemon-pass".to_string());
        let socket = restore_with(
            &f.context,
            &mut env,
            crate::paths::uid(),
            |_| panic!("the context is not read when the pass is present"),
            None,
        )
        .unwrap();
        assert_eq!(socket, f.context.socket);
        assert_eq!(env[CODEX_PASS_ENV], "daemon-pass");
        assert!(!env.contains_key("AMAIL_SEAT"));
        assert!(!env.contains_key("AMAIL_TOKEN"));
    }

    #[test]
    fn missing_pass_is_read_from_the_node_state() {
        let f = Fixture::new();
        let mut env = f.env();
        let socket = f.restore(&mut env, crate::paths::uid()).unwrap();
        assert_eq!(socket, f.context.socket);
        assert_eq!(env[CODEX_PASS_ENV], "existing-node-pass");
        assert!(!env.contains_key("AMAIL_SEAT"));
        assert!(!env.contains_key("AMAIL_TOKEN"));
    }

    #[test]
    fn wrong_home_or_missing_home_is_rejected_without_env_changes() {
        let f = Fixture::new();
        let other = tempfile::tempdir().unwrap();
        let mut env = f.env();
        env.insert(
            "CODEX_HOME".to_string(),
            other.path().to_string_lossy().into_owned(),
        );
        assert!(f.restore(&mut env, crate::paths::uid()).is_err());
        assert!(!env.contains_key(CODEX_PASS_ENV));
        let mut env = f.env();
        env.remove("CODEX_HOME");
        assert!(f.restore(&mut env, crate::paths::uid()).is_err());
        assert!(!env.contains_key(CODEX_PASS_ENV));
    }

    #[test]
    fn socket_of_another_user_is_rejected() {
        let f = Fixture::new();
        let mut env = f.env();
        assert!(f.restore(&mut env, crate::paths::uid() + 1).is_err());
        assert!(!env.contains_key(CODEX_PASS_ENV));
    }

    #[test]
    fn private_regular_state_with_existing_pass_is_required() {
        let f = Fixture::new();
        let uid = crate::paths::uid();
        for content in ["{}", r#"{"codex_pass":""}"#, "invalid-json"] {
            std::fs::write(&f.state, content).unwrap();
            assert!(existing_pass(&f.state, uid).is_err(), "{content}");
        }
        std::fs::write(&f.state, r#"{"codex_pass":"existing-node-pass"}"#).unwrap();
        std::fs::set_permissions(&f.state, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(existing_pass(&f.state, uid).is_err());
        std::fs::set_permissions(&f.state, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(existing_pass(&f.state, uid + 1).is_err());
        assert_eq!(existing_pass(&f.state, uid).unwrap(), "existing-node-pass");
        // A symlink in place of the file is not followed.
        let real = f.state.with_file_name("elsewhere.json");
        std::fs::rename(&f.state, &real).unwrap();
        symlink(&real, &f.state).unwrap();
        assert!(existing_pass(&f.state, uid).is_err());
    }

    #[test]
    fn control_socket_symlink_and_deep_path_keep_real_peer_checks() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("x".repeat(110));
        std::fs::create_dir(&deep).unwrap();
        std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o700)).unwrap();
        let folder = File::open(&deep).unwrap();
        let socket = deep.join("daemon.sock");
        let short = format!("/proc/self/fd/{}/daemon.sock", folder.as_raw_fd());
        let _listener = std::os::unix::net::UnixListener::bind(short).unwrap();
        let link = dir.path().join("app-server-control.sock");
        symlink(&socket, &link).unwrap();
        assert_eq!(
            peer(&link).unwrap(),
            (std::process::id(), crate::paths::uid())
        );
        let not_socket = dir.path().join("not-a-socket");
        std::fs::write(&not_socket, "fixture").unwrap();
        assert!(peer(&not_socket).is_err());
    }
}
