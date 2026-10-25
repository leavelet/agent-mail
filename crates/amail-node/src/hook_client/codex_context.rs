//! A context locator is not an identity override. It is used only by an
//! actual hook child of this node's Codex daemon. The node still authenticates
//! the forwarded hook with its existing pass and original hook protocol.

use super::NodeContext;
use crate::wire::CODEX_PASS_ENV;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct Process {
    start: u64,
    // This is a kernel display path, not executable identity. It can name a
    // deleted dentry even when another link still reaches the running file.
    executable: PathBuf,
    executable_image: Arc<File>,
    args: Vec<String>,
    home: Option<PathBuf>,
    cwd: PathBuf,
    directories: BTreeMap<String, String>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn process(pid: u32) -> io::Result<Process> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let stat = std::fs::read_to_string(root.join("stat"))?;
    let (_, tail) = stat
        .rsplit_once(") ")
        .ok_or_else(|| invalid("invalid process stat"))?;
    let fields: Vec<_> = tail.split_whitespace().collect();
    if fields.first() == Some(&"Z") {
        return Err(invalid("dead process"));
    }
    let start = fields
        .get(19)
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| invalid("invalid process start"))?;
    let args = std::fs::read(root.join("cmdline"))?;
    let env = std::fs::read(root.join("environ"))?;
    Ok(Process {
        start,
        executable: std::fs::read_link(root.join("exe"))?,
        executable_image: Arc::new(File::open(root.join("exe"))?),
        args: args
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
        home: env
            .split(|b| *b == 0)
            .find_map(|s| s.strip_prefix(b"CODEX_HOME="))
            .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned())),
        cwd: std::fs::read_link(root.join("cwd"))?,
        directories: [
            "HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "AMAIL_STATE_DIR",
            "AMAIL_NODE_SOCK",
        ]
        .into_iter()
        .filter_map(|key| {
            let prefix = format!("{key}=");
            env.split(|b| *b == 0)
                .find_map(|s| s.strip_prefix(prefix.as_bytes()))
                .map(|s| (key.to_string(), String::from_utf8_lossy(s).into_owned()))
        })
        .collect(),
    })
}

fn same_executable(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.is_file() && right.is_file() && left.dev() == right.dev() && left.ino() == right.ino())
}

fn option(args: &[String], key: &str) -> Option<PathBuf> {
    let prefix = format!("{key}=");
    args.iter().enumerate().find_map(|(i, arg)| {
        arg.strip_prefix(&prefix).map(PathBuf::from).or_else(|| {
            (arg == key)
                .then(|| args.get(i + 1))
                .flatten()
                .map(PathBuf::from)
        })
    })
}

fn node_paths(p: &Process) -> (PathBuf, PathBuf) {
    let var = |key: &str| {
        p.directories
            .get(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    // Same defaults as paths::{state_dir,socket}, read from the node process
    // rather than from the hook's potentially different environment.
    let default_state = var("AMAIL_STATE_DIR").unwrap_or_else(|| {
        var("XDG_DATA_HOME")
            .map(|d| d.join("amail"))
            .unwrap_or_else(|| {
                p.directories
                    .get("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/"))
                    .join(".local/share/amail")
            })
    });
    let state = option(&p.args, "--state-dir").unwrap_or_else(|| default_state.clone());
    let socket = option(&p.args, "--socket")
        .or_else(|| var("AMAIL_NODE_SOCK"))
        .unwrap_or_else(|| {
            var("XDG_RUNTIME_DIR")
                .map(|d| d.join("amail"))
                .unwrap_or_else(|| default_state.join("run"))
                .join("node.sock")
        });
    let absolute = |path: PathBuf| {
        if path.is_absolute() {
            path
        } else {
            p.cwd.join(path)
        }
    };
    (absolute(state), absolute(socket))
}

fn peer(path: &Path) -> io::Result<(u32, u32)> {
    // Both endpoints are owned by this user. Use a directory descriptor for
    // deep Codex-home paths instead of copying sockets or state to /tmp.
    // Codex's control socket is itself a symlink to its daemon endpoint.
    // Check the resolved socket and private directory, then its kernel peer.
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

fn existing_pass(path: &Path, uid: u32) -> io::Result<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(invalid("node state is not a private regular file"));
    }
    // Unknown fields (including seat tokens) are ignored, never selected or
    // copied into the hook environment.
    #[derive(Deserialize)]
    struct Pass {
        codex_pass: String,
    }
    let value: Pass = serde_json::from_reader(file).map_err(|_| invalid("invalid node context"))?;
    if value.codex_pass.is_empty() {
        return Err(invalid("node context is unavailable"));
    }
    Ok(value.codex_pass)
}

pub(super) fn restore(
    context: &NodeContext,
    env: &mut BTreeMap<String, String>,
    parent: u32,
) -> Result<(), String> {
    // Open the actual running hook image, not the pathname printed by
    // /proc/self/exe. Keep both image handles alive through the final process
    // recheck so an unlinked inode cannot be recycled during verification.
    let result = File::open("/proc/self/exe").and_then(|exe| {
        restore_with(
            context,
            env,
            parent,
            &exe,
            crate::paths::uid(),
            process,
            peer,
        )
    });
    // Do not format private file contents, environment values or the pass.
    result.map_err(|_| "Codex hook context could not be verified".to_string())
}

fn restore_with(
    context: &NodeContext,
    env: &mut BTreeMap<String, String>,
    parent: u32,
    executable: &File,
    uid: u32,
    mut read_process: impl FnMut(u32) -> io::Result<Process>,
    read_peer: impl Fn(&Path) -> io::Result<(u32, u32)>,
) -> io::Result<()> {
    let node = context.state_dir.canonicalize()?;
    let home = node.join("codex-home").canonicalize()?;
    let hook_home = env
        .get("CODEX_HOME")
        .ok_or_else(|| invalid("hook home missing"))?;
    if Path::new(hook_home).canonicalize()? != home {
        return Err(invalid("hook belongs to another Codex home"));
    }
    let backend = read_process(parent)?;
    // Preserve the backend's canonical package-directory confinement. Only
    // the controller/hook same-image check uses kernel file identity.
    if !backend
        .executable
        .canonicalize()?
        .starts_with(home.join("packages"))
        || !backend.args.iter().any(|a| a == "app-server")
        || backend
            .home
            .as_ref()
            .map(|p| p.canonicalize())
            .transpose()?
            .as_ref()
            != Some(&home)
    {
        return Err(invalid("hook parent is not this Codex backend"));
    }
    if read_peer(&home.join("app-server-control/app-server-control.sock"))? != (parent, uid) {
        return Err(invalid(
            "hook parent is not the original control socket peer",
        ));
    }
    let (node_pid, node_uid) = read_peer(&context.socket)?;
    if node_uid != uid {
        return Err(invalid("node socket belongs to another user"));
    }
    let controller = read_process(node_pid)?;
    let (controller_state, controller_socket) = node_paths(&controller);
    if !same_executable(&controller.executable_image, executable)?
        || !controller.args.iter().any(|a| a == "node")
        || controller_state.canonicalize()? != node
        || controller_socket.canonicalize()? != context.socket.canonicalize()?
    {
        return Err(invalid("unexpected node controller"));
    }
    let pass = existing_pass(&node.join("node-state.json"), uid)?;
    let current_backend = read_process(parent)?;
    let current_controller = read_process(node_pid)?;
    if current_backend.start != backend.start
        || current_controller.start != controller.start
        || !same_executable(&current_backend.executable_image, &backend.executable_image)?
        || !same_executable(
            &current_controller.executable_image,
            &controller.executable_image,
        )?
    {
        return Err(invalid("runtime changed while reading hook context"));
    }
    // Commit only after every check, without changing this process's global
    // environment. The caller continues forwarding the original input bytes.
    for key in ["AMAIL_SEAT", "AMAIL_TOKEN", "AMAIL_PROFILE"] {
        env.remove(key);
    }
    env.insert(CODEX_PASS_ENV.to_string(), pass);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[derive(Clone, Copy)]
    enum RuntimeChange {
        PidReuse(u32),
        Reexec(u32),
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        context: NodeContext,
        home: PathBuf,
        control: PathBuf,
        amail: PathBuf,
        backend: Process,
        controller: Process,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let node = dir.path().canonicalize().unwrap().join("node");
            let home = node.join("codex-home");
            let control = home.join("app-server-control/app-server-control.sock");
            let amail = node.join("bin/amail");
            let backend_exe = home.join("packages/release/bin/codex");
            let socket = node.join("run/node.sock");
            for path in [&control, &amail, &backend_exe, &socket] {
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
            let backend = Process {
                start: 11,
                executable_image: Arc::new(File::open(&backend_exe).unwrap()),
                executable: backend_exe,
                args: vec!["codex".into(), "app-server".into()],
                home: Some(home.clone()),
                cwd: node.clone(),
                directories: BTreeMap::new(),
            };
            let controller = Process {
                start: 22,
                executable_image: Arc::new(File::open(&amail).unwrap()),
                executable: amail.clone(),
                args: vec![
                    "amail".into(),
                    "node".into(),
                    format!("--state-dir={}", node.display()),
                    "--socket".into(),
                    socket.to_string_lossy().into_owned(),
                ],
                home: None,
                cwd: node.clone(),
                directories: BTreeMap::new(),
            };
            Self {
                _dir: dir,
                context: NodeContext {
                    state_dir: node,
                    socket,
                },
                home,
                control,
                amail,
                backend,
                controller,
            }
        }

        fn env(&self) -> BTreeMap<String, String> {
            BTreeMap::from([
                (
                    "CODEX_HOME".into(),
                    self.home.to_string_lossy().into_owned(),
                ),
                ("AMAIL_SEAT".into(), "must-not-select-a-seat".into()),
                ("AMAIL_TOKEN".into(), "must-not-forward".into()),
                ("AMAIL_PROFILE".into(), "must-not-use".into()),
            ])
        }

        fn apply(
            &self,
            env: &mut BTreeMap<String, String>,
            backend: Process,
            controller: Process,
            control_peer: (u32, u32),
            node_peer: (u32, u32),
            changed: Option<RuntimeChange>,
        ) -> io::Result<()> {
            let mut counts = BTreeMap::<u32, usize>::new();
            restore_with(
                &self.context,
                env,
                100,
                &File::open(&self.amail)?,
                crate::paths::uid(),
                |pid| {
                    let mut p = match pid {
                        100 => backend.clone(),
                        200 => controller.clone(),
                        _ => return Err(invalid("unknown process")),
                    };
                    let calls = counts.entry(pid).or_default();
                    *calls += 1;
                    if *calls > 1 {
                        match changed {
                            Some(RuntimeChange::PidReuse(changed_pid)) if changed_pid == pid => {
                                p.start += 1;
                            }
                            Some(RuntimeChange::Reexec(changed_pid)) if changed_pid == pid => {
                                p.executable_image = Arc::new(File::open(&self.control)?);
                            }
                            _ => {}
                        }
                    }
                    Ok(p)
                },
                |path| {
                    if path == self.control {
                        Ok(control_peer)
                    } else if path == self.context.socket {
                        Ok(node_peer)
                    } else {
                        Err(invalid("unknown endpoint"))
                    }
                },
            )
        }

        fn normal(&self, env: &mut BTreeMap<String, String>) -> io::Result<()> {
            let uid = crate::paths::uid();
            self.apply(
                env,
                self.backend.clone(),
                self.controller.clone(),
                (100, uid),
                (200, uid),
                None,
            )
        }
    }

    #[test]
    fn restores_only_existing_node_pass_for_bound_backend() {
        let f = Fixture::new();
        let mut env = f.env();
        f.normal(&mut env).unwrap();
        assert_eq!(env[CODEX_PASS_ENV], "existing-node-pass");
        assert_eq!(env["CODEX_HOME"], f.home.to_string_lossy());
        for key in [
            "AMAIL_SEAT",
            "AMAIL_TOKEN",
            "AMAIL_PROFILE",
            "AMAIL_NODE_SOCK",
        ] {
            assert!(!env.contains_key(key));
        }
    }

    #[test]
    fn node_environment_defaults_are_checked_without_changing_global_env() {
        let f = Fixture::new();
        let mut controller = f.controller.clone();
        controller.args = vec!["amail".into(), "node".into()];
        controller.directories = BTreeMap::from([
            (
                "AMAIL_STATE_DIR".into(),
                f.context.state_dir.to_string_lossy().into_owned(),
            ),
            (
                "AMAIL_NODE_SOCK".into(),
                f.context.socket.to_string_lossy().into_owned(),
            ),
        ]);
        let uid = crate::paths::uid();
        f.apply(
            &mut f.env(),
            f.backend.clone(),
            controller.clone(),
            (100, uid),
            (200, uid),
            None,
        )
        .unwrap();
        controller.directories = BTreeMap::from([("HOME".into(), "/home/example".into())]);
        assert_eq!(
            node_paths(&controller),
            (
                PathBuf::from("/home/example/.local/share/amail"),
                PathBuf::from("/home/example/.local/share/amail/run/node.sock")
            )
        );
        controller
            .directories
            .insert("XDG_DATA_HOME".into(), "/data".into());
        controller
            .directories
            .insert("XDG_RUNTIME_DIR".into(), "/runtime".into());
        assert_eq!(
            node_paths(&controller),
            (
                PathBuf::from("/data/amail"),
                PathBuf::from("/runtime/amail/node.sock")
            )
        );
    }

    #[test]
    fn wrong_home_or_missing_home_is_rejected_without_env_changes() {
        let f = Fixture::new();
        for home in [
            None,
            Some(f.context.state_dir.to_string_lossy().into_owned()),
        ] {
            let mut env = f.env();
            env.remove("CODEX_HOME");
            if let Some(home) = home {
                env.insert("CODEX_HOME".into(), home);
            }
            let before = env.clone();
            assert!(f.normal(&mut env).is_err());
            assert_eq!(env, before);
        }
    }

    #[test]
    fn foreign_backend_and_control_socket_are_rejected() {
        let f = Fixture::new();
        let uid = crate::paths::uid();
        for case in 0..5 {
            let mut backend = f.backend.clone();
            let mut peer = (100, uid);
            match case {
                0 => backend.executable = f.amail.clone(),
                1 => backend.args = vec!["codex".into(), "exec".into()],
                2 => backend.home = Some(f.context.state_dir.clone()),
                3 => peer.0 = 101,
                _ => peer.1 += 1,
            }
            let mut env = f.env();
            let before = env.clone();
            assert!(
                f.apply(
                    &mut env,
                    backend,
                    f.controller.clone(),
                    peer,
                    (200, uid),
                    None
                )
                .is_err()
            );
            assert_eq!(env, before);
        }
    }

    #[test]
    fn wrong_node_binary_directory_socket_or_owner_is_rejected() {
        let f = Fixture::new();
        let uid = crate::paths::uid();
        for case in 0..5 {
            let mut controller = f.controller.clone();
            let mut peer = (200, uid);
            match case {
                0 => {
                    controller.executable = f.backend.executable.clone();
                    controller.executable_image = f.backend.executable_image.clone();
                }
                1 => controller.args[1] = "serve".into(),
                2 => controller.args[2] = format!("--state-dir={}", f.home.display()),
                3 => controller.args[4] = f.control.to_string_lossy().into_owned(),
                _ => peer.1 += 1,
            }
            let mut env = f.env();
            let before = env.clone();
            assert!(
                f.apply(
                    &mut env,
                    f.backend.clone(),
                    controller,
                    (100, uid),
                    peer,
                    None
                )
                .is_err()
            );
            assert_eq!(env, before);
        }
    }

    #[test]
    fn private_regular_state_with_existing_pass_is_required() {
        let f = Fixture::new();
        let state = f.context.state_dir.join("node-state.json");
        for content in ["{}", r#"{"codex_pass":""}"#, "invalid-json"] {
            std::fs::write(&state, content).unwrap();
            let mut env = f.env();
            let before = env.clone();
            assert!(f.normal(&mut env).is_err());
            assert_eq!(env, before);
        }
        std::fs::write(&state, r#"{"codex_pass":"existing-node-pass"}"#).unwrap();
        assert!(existing_pass(&state, crate::paths::uid() + 1).is_err());
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(f.normal(&mut f.env()).is_err());
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
        let original = state.with_file_name("saved-state.json");
        std::fs::rename(&state, &original).unwrap();
        symlink(&original, &state).unwrap();
        assert!(f.normal(&mut f.env()).is_err());
    }

    #[test]
    fn backend_or_node_pid_reuse_is_rejected_before_env_commit() {
        let f = Fixture::new();
        let uid = crate::paths::uid();
        for pid in [100, 200] {
            let mut env = f.env();
            let before = env.clone();
            assert!(
                f.apply(
                    &mut env,
                    f.backend.clone(),
                    f.controller.clone(),
                    (100, uid),
                    (200, uid),
                    Some(RuntimeChange::PidReuse(pid))
                )
                .is_err()
            );
            assert_eq!(env, before);
        }
    }

    #[test]
    fn backend_or_node_reexec_is_rejected_before_env_commit() {
        let f = Fixture::new();
        let uid = crate::paths::uid();
        for pid in [100, 200] {
            let mut env = f.env();
            let before = env.clone();
            assert!(
                f.apply(
                    &mut env,
                    f.backend.clone(),
                    f.controller.clone(),
                    (100, uid),
                    (200, uid),
                    Some(RuntimeChange::Reexec(pid))
                )
                .is_err()
            );
            assert_eq!(env, before);
        }
    }

    #[test]
    fn reused_executable_path_does_not_authorize_a_different_image() {
        let f = Fixture::new();
        let original = f.amail.with_file_name("original-amail");
        std::fs::rename(&f.amail, &original).unwrap();
        // Even byte-identical contents at the old pathname are a different
        // file object. The controller's open image still pins the old inode.
        std::fs::copy(&original, &f.amail).unwrap();
        assert!(
            !same_executable(
                &f.controller.executable_image,
                &File::open(&f.amail).unwrap()
            )
            .unwrap()
        );
        let mut env = f.env();
        let before = env.clone();
        assert!(f.normal(&mut env).is_err());
        assert_eq!(env, before);
    }

    #[test]
    fn deleted_display_text_is_never_used_as_an_identity_override() {
        let f = Fixture::new();
        let mut controller = f.controller.clone();
        controller.executable = PathBuf::from(format!("{} (deleted)", f.amail.display()));
        controller.executable_image = f.backend.executable_image.clone();
        let uid = crate::paths::uid();
        let mut env = f.env();
        let before = env.clone();
        assert!(
            f.apply(
                &mut env,
                f.backend.clone(),
                controller,
                (100, uid),
                (200, uid),
                None
            )
            .is_err()
        );
        assert_eq!(env, before);
    }

    #[cfg(target_os = "linux")]
    struct RunningImage {
        child: std::process::Child,
        _dir: tempfile::TempDir,
        path: PathBuf,
        retained: PathBuf,
    }

    #[cfg(target_os = "linux")]
    impl RunningImage {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("controller");
            let retained = dir.path().join("retained-image");
            std::fs::copy("/bin/sleep", &path).unwrap();
            std::fs::hard_link(&path, &retained).unwrap();
            // Under a parallel test run another thread's fork can hold the
            // just-written file open for a moment, and exec then fails with
            // ETXTBSY; try again briefly.
            let child = (0..50)
                .find_map(
                    |_| match std::process::Command::new(&path).arg("60").spawn() {
                        Ok(c) => Some(c),
                        Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                            std::thread::sleep(std::time::Duration::from_millis(20));
                            None
                        }
                        Err(e) => panic!("spawning the test image: {e}"),
                    },
                )
                .expect("the test image stayed busy");
            Self {
                child,
                _dir: dir,
                path,
                retained,
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl Drop for RunningImage {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn relinked_running_image_restores_context_without_resolving_deleted_text() {
        let image = RunningImage::new();
        std::fs::remove_file(&image.path).unwrap();
        std::fs::hard_link(&image.retained, &image.path).unwrap();
        let observed = process(image.child.id()).unwrap();
        // The exe link of the unlinked binary reads " (deleted)" on most
        // kernels and file systems, not on every CI runner; the test does
        // not depend on it: the file object is what identifies the image.
        assert!(
            same_executable(
                &observed.executable_image,
                &File::open(&image.path).unwrap()
            )
            .unwrap()
        );

        let f = Fixture::new();
        std::fs::remove_file(&f.amail).unwrap();
        std::fs::hard_link(&image.retained, &f.amail).unwrap();
        let mut controller = f.controller.clone();
        controller.executable = observed.executable;
        controller.executable_image = observed.executable_image;
        let uid = crate::paths::uid();
        let mut env = f.env();
        f.apply(
            &mut env,
            f.backend.clone(),
            controller,
            (100, uid),
            (200, uid),
            None,
        )
        .unwrap();
        assert_eq!(env[CODEX_PASS_ENV], "existing-node-pass");
        assert!(!env.contains_key("AMAIL_TOKEN"));
        assert!(!env.contains_key("AMAIL_SEAT"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn replaced_running_executable_path_is_a_different_kernel_image() {
        let image = RunningImage::new();
        std::fs::remove_file(&image.path).unwrap();
        std::fs::copy(&image.retained, &image.path).unwrap();
        let observed = process(image.child.id()).unwrap();
        // Whether the exe link of the replaced binary says " (deleted)"
        // depends on the kernel and file system (it does not on the CI
        // runner); the check that matters is the one on the file objects.
        assert!(
            !same_executable(
                &observed.executable_image,
                &File::open(&image.path).unwrap()
            )
            .unwrap()
        );
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
