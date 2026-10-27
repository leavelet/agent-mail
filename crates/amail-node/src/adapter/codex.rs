//! Codex.
//!
//! - All Codex seats on a host share one dedicated `CODEX_HOME` (under the
//!   node's state directory) and its app-server daemon. Interactive sessions
//!   attach to that daemon, and hooks run inside it, so a hook cannot see the
//!   session's environment. A session is tied to its seat by a marker in its
//!   first prompt, `[amail:seat=<seat> launch=<nonce>]`, where the nonce was
//!   reserved by `amail run`; after that the hook's `session_id` identifies
//!   the seat.
//! - The daemon runs as a package Codex installs under the home; the node
//!   turns its self-update off and keeps it at the `codex` command's version
//!   (see `pin_daemon_package`), because a TUI does not attach to a daemon
//!   of another version.
//! - Hooks are defined in the home's `config.toml` and pre-trusted there
//!   (trust hashes cover the binary and the explicit node-context paths).
//! - Wake-up: `codex queue --thread <session id> --message ...`, which starts
//!   a turn on an idle thread and fires `UserPromptSubmit`.
//! - MCP: `[mcp_servers.amail]` in the same `config.toml` runs `amail mcp
//!   --node`, which finds the seat from the thread id Codex sends with each
//!   tool call (see `apply_mcp`).
//! - Launch flags are limited to ones that keep the session attached to the
//!   shared daemon (`-m`, `-a`, `-s`, `-C`).

use super::*;
use crate::wire::GitAccess;
use amail_proto::PermissionMode;
use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::sync::Mutex;

mod quiet;

/// Hook events: (config name, snake_case name used in trust keys).
const EVENTS: &[(&str, &str)] = &[
    ("SessionStart", "session_start"),
    ("UserPromptSubmit", "user_prompt_submit"),
    ("PostToolUse", "post_tool_use"),
    ("PermissionRequest", "permission_request"),
    ("Stop", "stop"),
    ("Interrupt", "interrupt"),
    ("SessionEnd", "session_end"),
    ("PreCompact", "pre_compact"),
    ("PostCompact", "post_compact"),
];

const HOOK_TIMEOUT: i64 = 5;

/// An explicit native Codex sandbox policy, instead of amail's git profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl SandboxMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    fn permissions(self) -> &'static str {
        match self {
            Self::ReadOnly => ":read-only",
            Self::WorkspaceWrite => ":workspace",
            Self::DangerFullAccess => ":danger-full-access",
        }
    }
}

impl std::str::FromStr for SandboxMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "read-only" => Ok(Self::ReadOnly),
            "workspace-write" => Ok(Self::WorkspaceWrite),
            "danger-full-access" => Ok(Self::DangerFullAccess),
            _ => Err("expected read-only, workspace-write or danger-full-access".into()),
        }
    }
}

/// The permissions profile Codex sessions run under by default (see
/// `apply_managed`); it carries the grants of `git_access = "worktree"`.
const PERMISSIONS_PROFILE: &str = "amail";
/// The profile of seats with `git_access = "full"`.
const FULL_PROFILE: &str = "amail-full";
/// The profile of seats with `git_access = "off"`.
const OFF_PROFILE: &str = "amail-off";

/// First line of the project config amail writes into the working
/// directory of a seat whose git access needs a profile other than the
/// default; the seat's name follows.
const PROJECT_MARKER: &str = "# Written by amail for seat ";

/// Directory in a repository's git directory that holds `packed-refs` for
/// repositories with `git_access = "worktree"` seats (see `link_packed_refs`).
const PACKED_DIR: &str = "amail";

/// The git directory and the common directory of a working directory.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GitDirs {
    /// `git rev-parse --git-dir`: a linked worktree's own git directory,
    /// or the repository's git directory in the main checkout.
    pub(crate) git_dir: PathBuf,
    /// `git rev-parse --git-common-dir`: the repository's git directory.
    pub(crate) common: PathBuf,
}

impl GitDirs {
    fn linked(&self) -> bool {
        self.git_dir != self.common
    }
}

fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The git directories of `cwd` (absolute), if it is in a git repository.
pub(crate) fn git_dirs(cwd: &Path) -> Option<GitDirs> {
    let out = git_out(
        cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ],
    )?;
    let mut lines = out.lines().map(str::trim).filter(|l| !l.is_empty());
    let git_dir = PathBuf::from(lines.next()?);
    let common = PathBuf::from(lines.next()?);
    let canon = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    Some(GitDirs {
        git_dir: canon(git_dir),
        common: canon(common),
    })
}

/// The main checkout of the repository `cwd` is in (none for a bare
/// repository).
fn main_worktree(cwd: &Path) -> Option<PathBuf> {
    let out = git_out(cwd, &["worktree", "list", "--porcelain"])?;
    let mut block = out.split("\n\n").next()?.lines();
    let path = block.next()?.strip_prefix("worktree ")?;
    if block.any(|l| l == "bare") {
        return None;
    }
    Some(PathBuf::from(path))
}

/// Entries of a repository's git directory that stay read-only under
/// `git_access = "full"`, where sessions may write the directory: git runs
/// `hooks/` and reads `config` and `config.worktree` as configuration
/// (`core.hooksPath`, `core.fsmonitor` and others name commands to run);
/// `modules/` holds the git directories of submodules, with their own
/// hooks and config; `worktrees/` holds those of linked worktrees, which
/// get their own rules. Codex mounts a path that does not exist as an
/// empty read-only placeholder, so these entries cannot be created either.
/// A `commondir` file cannot be kept from being created this way (git
/// cannot read an empty placeholder), which is why `full` is not the
/// default.
pub(crate) const GIT_READ_ONLY: &[&str] =
    &["hooks", "config", "config.worktree", "modules", "worktrees"];

/// Entries of a linked worktree's git directory that stay read-only:
/// `commondir` names the directory git takes config and hooks from,
/// `gitdir` names the worktree, and `config.worktree` is configuration.
const LINKED_GIT_READ_ONLY: &[&str] = &["commondir", "gitdir", "config.worktree"];

/// What kind of git directory `dir` is.
#[derive(Debug, PartialEq)]
enum GitDir {
    /// A repository's git directory (`objects/` and no `commondir`).
    Main,
    /// A linked worktree's git directory (`commondir` and no `objects/`).
    Linked,
    /// A repository's git directory with a `commondir` file, which git
    /// never writes there: git run in its main checkout would take config
    /// and hooks from the directory the file names.
    Redirected,
}

fn git_dir_kind(dir: &Path) -> Option<GitDir> {
    if !dir.join("HEAD").is_file() {
        return None;
    }
    match (dir.join("objects").is_dir(), dir.join("commondir").exists()) {
        (true, false) => Some(GitDir::Main),
        (false, true) => Some(GitDir::Linked),
        (true, true) => Some(GitDir::Redirected),
        (false, false) => None,
    }
}

pub(crate) type Rules = Vec<(String, &'static str)>;

fn rule(v: &mut Rules, p: &Path, mode: &'static str) {
    v.push((p.to_string_lossy().into_owned(), mode));
}

fn linked_rules(v: &mut Rules, g: &Path) {
    rule(v, g, "write");
    for name in LINKED_GIT_READ_ONLY {
        rule(v, &g.join(name), "read");
    }
}

/// The `filesystem` rules of `git_access = "worktree"` for a linked
/// worktree with git directory `g` of the repository whose git directory
/// is `c` (Codex applies the most specific rule for a path, and accepts
/// only directories as write roots).
///
/// What committing, rebasing and merging in a linked worktree write: new
/// objects (`objects/`), branches and tags (`refs/`), branch reflogs
/// (`logs/refs/`), and the worktree's own state (`HEAD`, `index`,
/// `ORIG_HEAD`, `rebase-merge/`, its `logs/HEAD`, ...) in `g`. Deleting a
/// ref (git deletes `CHERRY_PICK_HEAD` and others during a rebase) also
/// takes the lock `packed-refs.lock` next to `packed-refs`, which is in
/// the root of `c`; `link_packed_refs` moves `packed-refs` into
/// `c/amail/` so that the lock can be taken there. The file itself stays
/// read-only, so that a session cannot replace it with a symbolic link.
///
/// Everything else in the root of `c` stays read-only: `config`, `hooks/`,
/// `info/`, `commondir` (which git would follow in the main checkout), the
/// main checkout's `HEAD`, `index` and `logs/HEAD`. So does the reflog of
/// the branch checked out in the main checkout, which git run there
/// appends to. In `g`, `commondir`, `gitdir` and `config.worktree` stay
/// read-only.
fn worktree_rules(g: &Path, c: &Path) -> Rules {
    let mut v = Vec::new();
    for d in ["objects", "refs", "logs/refs", PACKED_DIR] {
        rule(&mut v, &c.join(d), "write");
    }
    rule(&mut v, &c.join(PACKED_DIR).join("packed-refs"), "read");
    let head = std::fs::read_to_string(c.join("HEAD")).unwrap_or_default();
    // Only an existing file: Codex would turn the first missing directory
    // of the path into a placeholder file, and git could then not create
    // reflogs under it.
    if let Some(r) = head.trim().strip_prefix("ref: refs/heads/")
        && !r.split('/').any(|p| p.is_empty() || p == "." || p == "..")
        && c.join("logs/refs/heads").join(r).is_file()
    {
        rule(&mut v, &c.join("logs/refs/heads").join(r), "read");
    }
    linked_rules(&mut v, g);
    v
}

/// The `filesystem` rules of `git_access = "full"` for the repository
/// whose git directory is `c` and main checkout `main`: both writable,
/// except the entries of `GIT_READ_ONLY` and, in each linked worktree's
/// git directory, those of `LINKED_GIT_READ_ONLY`.
fn full_rules(c: &Path, main: Option<&Path>) -> Rules {
    let mut v = Vec::new();
    if let Some(m) = main {
        rule(&mut v, m, "write");
    }
    rule(&mut v, c, "write");
    for name in GIT_READ_ONLY {
        rule(&mut v, &c.join(name), "read");
    }
    let mut wts: Vec<PathBuf> = std::fs::read_dir(c.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| git_dir_kind(p) == Some(GitDir::Linked))
        .collect();
    wts.sort();
    for w in wts {
        linked_rules(&mut v, &w);
    }
    v
}

/// The git rules of a seat in `cwd` (git directories `dirs`) for its git
/// access, and a notice for the person when the access cannot be given
/// there. Prepares the repository as the rules need (the `logs/refs`
/// directory, `packed-refs` moved for `worktree`) and refuses a repository
/// whose git directory has been changed in a way git never does. `what`
/// names the sandbox in the notice, `docs` where to read more. Shared with the dsh adapter's jail.
pub(crate) fn seat_git_rules(
    cwd: &Path,
    dirs: Option<&GitDirs>,
    seat: &str,
    access: GitAccess,
    what: &str,
    docs: &str,
) -> Result<(Rules, Option<String>)> {
    let mut notice = None;
    let mut rules = Vec::new();
    match (dirs, access) {
        (Some(d), GitAccess::Worktree) if d.linked() => {
            crate::worktree::check_common_dir(&d.common)?;
            std::fs::create_dir_all(d.common.join("logs/refs"))?;
            link_packed_refs(&d.common)?;
            rules = worktree_rules(&d.git_dir, &d.common);
        }
        (Some(d), GitAccess::Worktree) => {
            notice = Some(format!(
                "seat {seat}: {} is the main checkout of a git repository, where the \
                 {what} keeps git read-only with git_access \"worktree\" (the \
                 default), so the session cannot commit. Give the seat a linked \
                 worktree (the staged-merge template does), or set git_access = \
                 \"full\" (see {docs}).",
                d.git_dir.parent().unwrap_or(&d.git_dir).display()
            ));
        }
        (Some(d), GitAccess::Full) => {
            crate::worktree::check_common_dir(&d.common)?;
            rules = full_rules(&d.common, main_worktree(cwd).as_deref());
        }
        _ => {}
    }
    Ok((rules, notice))
}

/// Remove from the default profile's `filesystem` table the write grants
/// on the root of a repository's git directory that older versions of
/// amail added for every Codex seat (and their read-only entries).
fn strip_root_grants(fs: &mut toml::Table) -> bool {
    let roots: Vec<String> = fs
        .iter()
        .filter(|(k, v)| {
            v.as_str() == Some("write")
                && matches!(
                    git_dir_kind(Path::new(k)),
                    Some(GitDir::Main | GitDir::Redirected)
                )
        })
        .map(|(k, _)| k.clone())
        .collect();
    for r in &roots {
        fs.remove(r);
        for name in GIT_READ_ONLY {
            fs.remove(&Path::new(r).join(name).to_string_lossy().into_owned());
        }
    }
    !roots.is_empty()
}

/// Add `rules` to `fs`; returns whether it changed.
fn add_rules(fs: &mut toml::Table, rules: Rules) -> bool {
    let mut changed = false;
    for (path, mode) in rules {
        if fs.get(&path).and_then(|v| v.as_str()) != Some(mode) {
            fs.insert(path, toml::Value::String(mode.into()));
            changed = true;
        }
    }
    changed
}

/// Repositories' git directories that `fs` grants write access in.
fn granted_repos(fs: &toml::Table) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs
        .iter()
        .filter(|(_, m)| m.as_str() == Some("write"))
        .filter_map(|(k, _)| {
            let p = Path::new(k);
            match git_dir_kind(p) {
                Some(GitDir::Main | GitDir::Redirected) => Some(p.to_path_buf()),
                Some(GitDir::Linked) => None,
                None => p
                    .parent()
                    .filter(|c| p.ends_with("objects") && git_dir_kind(c).is_some())
                    .map(Path::to_path_buf),
            }
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Make `<c>/packed-refs` a symbolic link to `amail/packed-refs` (moving
/// the file there), so that git takes `packed-refs.lock` in `<c>/amail/`,
/// which `git_access = "worktree"` sessions may write. Git resolves the
/// link when it locks, reads or rewrites `packed-refs`, so git run outside
/// the sandbox keeps working as before. To undo it, move
/// `<c>/amail/packed-refs` back over the link.
fn link_packed_refs(c: &Path) -> Result<()> {
    let link = c.join("packed-refs");
    let target = Path::new(PACKED_DIR).join("packed-refs");
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(&link)? == target {
                return Ok(());
            }
            bail!(
                "{} is a symbolic link to {}, not to {}; check it",
                link.display(),
                std::fs::read_link(&link)?.display(),
                target.display()
            );
        }
        Ok(m) if !m.is_file() => bail!("{} is not a file; check it", link.display()),
        _ => {}
    }
    std::fs::create_dir_all(c.join(PACKED_DIR))?;
    // Git's own lock: no git process rewrites packed-refs meanwhile.
    let lock = c.join("packed-refs.lock");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .with_context(|| {
            format!(
                "locking {} (if no git command is running in the repository, \
                 remove the lock file)",
                link.display()
            )
        })?;
    let r = (|| -> Result<()> {
        if link.exists() {
            let tmp = c.join(PACKED_DIR).join("packed-refs.amail-tmp");
            std::fs::copy(&link, &tmp)?;
            std::fs::File::open(&tmp)?.sync_all()?;
            std::fs::rename(&tmp, c.join(&target))?;
        }
        let tmp = c.join("packed-refs.amail-tmp");
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(&target, &tmp)?;
        std::fs::rename(&tmp, &link)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&lock);
    r.with_context(|| format!("moving {} to {}", link.display(), target.display()))
}

/// The `default_permissions` a project config sets, if any.
fn project_profile(text: &str) -> Option<String> {
    let t: toml::Table = text.parse().ok()?;
    t.get("default_permissions")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Codex takes `default_permissions` from `.codex/config.toml` in the
/// working directory or a directory above it in the project (and keeps
/// `.codex/` read-only inside the sandbox). This selects the profile of
/// a seat whose `git_access` is not the default: amail writes the file
/// into the seat's working directory `cwd` for `full` and `off` and
/// removes its own file otherwise. A project config amail did not write
/// that sets `default_permissions`, or a file amail wrote for another seat
/// with another profile, is refused: it would replace the seat's profile.
fn select_profile(cwd: &Path, top: Option<&Path>, seat: &str, want: Option<&str>) -> Result<()> {
    let dir = cwd.join(".codex");
    let file = dir.join("config.toml");
    let ours = format!("{PROJECT_MARKER}{seat}\n");
    for p in [&dir, &file] {
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("{} is a symbolic link; check it and remove it", p.display());
        }
    }
    // Directories above `cwd`, up to the top of its checkout.
    if let Some(top) = top {
        for a in cwd.ancestors().skip(1) {
            if !a.starts_with(top) {
                break;
            }
            let f = a.join(".codex/config.toml");
            if let Some(p) = std::fs::read_to_string(&f)
                .ok()
                .and_then(|t| project_profile(&t))
            {
                bail!(
                    "{} sets default_permissions = {p:?}, which would replace the Codex \
                     sandbox profile of seat {seat}; remove that setting",
                    f.display()
                );
            }
        }
    }
    match std::fs::read_to_string(&file) {
        Ok(text) => {
            let current = project_profile(&text);
            if let Some(rest) = text.strip_prefix(PROJECT_MARKER) {
                let other = rest.lines().next().unwrap_or_default();
                if other != seat && current.as_deref() != want {
                    bail!(
                        "seat {other} uses {} with another git_access; give the seats \
                         different working directories",
                        cwd.display()
                    );
                }
            } else if current.is_some() || want.is_some() {
                bail!(
                    "{} was not written by amail{}; seat {seat} needs amail to choose its \
                     Codex sandbox profile there (move the file away, or use \
                     git_access = \"worktree\")",
                    file.display(),
                    if current.is_some() {
                        " and sets default_permissions"
                    } else {
                        ""
                    }
                );
            } else {
                return Ok(());
            }
            if want.is_none() {
                std::fs::remove_file(&file)?;
                let _ = std::fs::remove_file(dir.join(".gitignore"));
                let _ = std::fs::remove_dir(&dir);
                return Ok(());
            }
        }
        Err(_) if want.is_none() => return Ok(()),
        Err(_) => {}
    }
    let Some(want) = want else { return Ok(()) };
    std::fs::create_dir_all(&dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n")?;
    }
    let text = format!(
        "{ours}# It selects the Codex sandbox profile for its git_access; amail keeps it \
         current.\ndefault_permissions = \"{want}\"\n"
    );
    if std::fs::read_to_string(&file).ok().as_deref() != Some(text.as_str()) {
        crate::persist::write_atomic(&file, text.as_bytes(), 0o644)?;
    }
    Ok(())
}

/// The profile table `name` of `t`, created as the workspace profile with
/// network access (agents' `amail` commands reach the server over
/// loopback) when missing. Returns its `filesystem` table.
fn profile_fs<'a>(t: &'a mut toml::Table, name: &str) -> &'a mut toml::Table {
    use toml::Value as V;
    let perms = t
        .entry("permissions")
        .or_insert_with(|| V::Table(toml::Table::new()));
    if !perms.is_table() {
        *perms = V::Table(toml::Table::new());
    }
    let V::Table(perms) = perms else {
        unreachable!()
    };
    let prof = perms
        .entry(name)
        .or_insert_with(|| V::Table(toml::Table::new()));
    if !prof.is_table() {
        *prof = V::Table(toml::Table::new());
    }
    let V::Table(prof) = prof else { unreachable!() };
    prof.insert("extends".into(), V::String(":workspace".into()));
    let mut net = toml::Table::new();
    net.insert("enabled".into(), V::Boolean(true));
    prof.insert("network".into(), V::Table(net));
    let fs = prof
        .entry("filesystem")
        .or_insert_with(|| V::Table(toml::Table::new()));
    if !fs.is_table() {
        *fs = V::Table(toml::Table::new());
    }
    let V::Table(fs) = fs else { unreachable!() };
    fs
}

/// Codex caps the timeout of some events at 3 s (and warns about larger ones).
fn hook_timeout(snake: &str) -> i64 {
    if matches!(snake, "session_end" | "interrupt") {
        HOOK_TIMEOUT.min(3)
    } else {
        HOOK_TIMEOUT
    }
}

/// Flags that detach the TUI from the shared daemon or bypass hook trust.
const FORBIDDEN_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-p",
    "--profile",
    "--dangerously-bypass-hook-trust",
    "--approve-for-me",
    "--remote",
];

const UNSET: &[&str] = &[
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "AMAIL_PROFILE",
    // A seat token in the TUI's environment would reach a daemon the TUI
    // starts, and so every thread on it (see `launch`).
    "AMAIL_TOKEN",
];

pub struct Codex {
    env: AdapterEnv,
    /// Serializes edits of `config.toml` and daemon start-up.
    lock: Mutex<()>,
}

impl Codex {
    pub fn new(env: AdapterEnv) -> Self {
        Codex {
            env,
            lock: Mutex::new(()),
        }
    }

    pub fn home(&self) -> PathBuf {
        home_in(&self.env.state_dir)
    }

    fn config_path(&self) -> PathBuf {
        self.home().join("config.toml")
    }

    // Codex uses the canonical source path in hook keys, including when
    // CODEX_HOME was supplied through a short symlink for Unix sockets.
    fn hook_config_path(&self) -> PathBuf {
        self.config_path().canonicalize().unwrap_or_else(|_| {
            self.home()
                .canonicalize()
                .unwrap_or_else(|_| self.home())
                .join("config.toml")
        })
    }

    pub fn control_socket(&self) -> PathBuf {
        self.home()
            .join("app-server-control")
            .join("app-server-control.sock")
    }

    pub fn hook_command(&self) -> String {
        let state =
            std::path::absolute(&self.env.state_dir).unwrap_or_else(|_| self.env.state_dir.clone());
        let socket = std::path::absolute(&self.env.node_socket)
            .unwrap_or_else(|_| self.env.node_socket.clone());
        format!(
            "{} hook codex --node-state-dir {} --node-socket {}",
            shell_quote(&self.env.amail_exe.to_string_lossy()),
            shell_quote(&state.to_string_lossy()),
            shell_quote(&socket.to_string_lossy())
        )
    }

    fn read_config(&self) -> Result<toml::Table> {
        match std::fs::read_to_string(self.config_path()) {
            Ok(s) => s
                .parse::<toml::Table>()
                .with_context(|| format!("parsing {}", self.config_path().display())),
            Err(_) => Ok(toml::Table::new()),
        }
    }

    fn write_config(&self, t: &toml::Table) -> Result<()> {
        let text = format!(
            "# Managed by amail. Keys amail does not manage are kept.\n{}",
            toml::to_string(t)?
        );
        crate::persist::write_atomic(&self.config_path(), text.as_bytes(), 0o600)
    }

    /// The trust key and hash Codex computes for our handler of `snake`.
    fn trust_entry(&self, snake: &str) -> (String, String) {
        let timeout = hook_timeout(snake);
        let canonical = serde_json::json!({
            "event_name": snake,
            "hooks": [{"async": false, "command": self.hook_command(), "timeout": timeout, "type": "command"}],
        });
        // serde_json maps are sorted, and to_string is compact.
        let hash = hex::encode(Sha256::digest(canonical.to_string().as_bytes()));
        (
            format!("{}:{snake}:0:0", self.hook_config_path().display()),
            format!("sha256:{hash}"),
        )
    }

    /// Set the amail-managed keys of `config.toml`.
    fn apply_managed(&self, t: &mut toml::Table, trust: &BTreeMap<String, String>) {
        use toml::Value as V;
        t.insert(
            "developer_instructions".into(),
            V::String(crate::inject::MAIL_INSTRUCTIONS.to_string()),
        );
        let mut hooks = toml::Table::new();
        for (name, snake) in EVENTS {
            let mut handler = toml::Table::new();
            handler.insert("type".into(), V::String("command".into()));
            handler.insert("command".into(), V::String(self.hook_command()));
            handler.insert("timeout".into(), V::Integer(hook_timeout(snake)));
            let mut group = toml::Table::new();
            group.insert("hooks".into(), V::Array(vec![V::Table(handler)]));
            hooks.insert(name.to_string(), V::Array(vec![V::Table(group)]));
        }
        let mut state = toml::Table::new();
        for (key, hash) in trust {
            let mut e = toml::Table::new();
            e.insert("trusted_hash".into(), V::String(hash.clone()));
            state.insert(key.clone(), V::Table(e));
        }
        hooks.insert("state".into(), V::Table(state));
        t.insert("hooks".into(), V::Table(hooks));
        self.apply_mcp(t);
        // Agents' `amail` commands reach the server over loopback. Without
        // network access the Codex sandbox refuses `socket(AF_INET)` and also
        // `connect()` on Unix sockets (EPERM, even inside the workspace), so
        // going through the node socket instead would not help.
        let sww = t
            .entry("sandbox_workspace_write")
            .or_insert_with(|| V::Table(toml::Table::new()));
        if let V::Table(sww) = sww {
            sww.insert("network_access".into(), V::Boolean(true));
        }
        // Sessions run under the `amail` permissions profile: the workspace
        // profile plus network access (as above) plus the git grants of
        // `git_access = "worktree"` seats. Seats with another `git_access`
        // select `amail-full` or `amail-off` through a project config (see
        // `grant_git`).
        t.insert(
            "default_permissions".into(),
            V::String(
                self.env
                    .codex_sandbox
                    .map(SandboxMode::permissions)
                    .unwrap_or(PERMISSIONS_PROFILE)
                    .into(),
            ),
        );
        for name in [PERMISSIONS_PROFILE, FULL_PROFILE, OFF_PROFILE] {
            profile_fs(t, name);
        }
    }

    /// Register `amail mcp --node` in `[mcp_servers.amail]` (other servers
    /// are kept). The app-server daemon starts it for each thread with only
    /// the variables Codex passes to MCP servers: `env` names the node
    /// socket, and `env_vars` passes on the node's Codex pass from the
    /// daemon's environment, so the file holds no secret. Codex puts the
    /// thread id in the `_meta` of every tool call; the server asks the
    /// node for that thread's seat with the pass. The hook trust hashes
    /// cover only the hooks, so this entry does not change them.
    fn apply_mcp(&self, t: &mut toml::Table) {
        use toml::Value as V;
        let socket = std::path::absolute(&self.env.node_socket)
            .unwrap_or_else(|_| self.env.node_socket.clone());
        let mut env = toml::Table::new();
        env.insert(
            crate::paths::SOCK_ENV.into(),
            V::String(socket.to_string_lossy().into_owned()),
        );
        let mut server = toml::Table::new();
        server.insert(
            "command".into(),
            V::String(self.env.amail_exe.to_string_lossy().into_owned()),
        );
        server.insert(
            "args".into(),
            V::Array(vec![V::String("mcp".into()), V::String("--node".into())]),
        );
        server.insert("env".into(), V::Table(env));
        server.insert(
            "env_vars".into(),
            V::Array(vec![V::String(crate::wire::CODEX_PASS_ENV.into())]),
        );
        let servers = t
            .entry("mcp_servers")
            .or_insert_with(|| V::Table(toml::Table::new()));
        if !servers.is_table() {
            *servers = V::Table(toml::Table::new());
        }
        if let V::Table(servers) = servers {
            servers.insert(MCP_SERVER.into(), V::Table(server));
        }
    }

    /// Set up the sandbox profile of a session of `seat` in `cwd` for its
    /// git access (see `GitAccess`), and return a notice for the person
    /// if the access cannot be given there.
    ///
    /// The workspace sandbox keeps `.git` read-only, and a linked
    /// worktree's git directory lies outside the worktree, so without a
    /// grant a session cannot commit. With `worktree`, the default profile
    /// gets the rules of `worktree_rules` for the seat's linked worktree;
    /// a seat in a main checkout gets no grant (committing there writes the
    /// root of the git directory). With `full`, the `amail-full` profile
    /// gets the rules of `full_rules`. With `off`, nothing is granted.
    ///
    /// All Codex seats of a host share one Codex home. The default profile
    /// holds the `worktree` grants of every seat; seats with `full` or
    /// `off` select their own profile through `select_profile`.
    fn grant_git(&self, cwd: &str, seat: &str, access: GitAccess) -> Result<Option<String>> {
        // An explicit native policy controls access itself. Do not rewrite
        // the repository or select a project profile that overrides it.
        if self.env.codex_sandbox.is_some() {
            return Ok(None);
        }
        let cwd = Path::new(cwd);
        let dirs = git_dirs(cwd);
        let top = git_out(cwd, &["rev-parse", "--show-toplevel"]).map(|s| PathBuf::from(s.trim()));
        select_profile(
            cwd,
            top.as_deref(),
            seat,
            match access {
                GitAccess::Worktree => None,
                GitAccess::Full => Some(FULL_PROFILE),
                GitAccess::Off => Some(OFF_PROFILE),
            },
        )?;
        let mut t = self.read_config()?;
        let mut changed = false;
        let (mut rules, notice) = seat_git_rules(
            cwd,
            dirs.as_ref(),
            seat,
            access,
            "Codex sandbox",
            "the Codex section of docs/harnesses.md",
        )?;
        // The default profile: no grant on the root of a repository's git
        // directory (older versions granted one), plus this seat's rules.
        let fs = profile_fs(&mut t, PERMISSIONS_PROFILE);
        changed |= strip_root_grants(fs);
        if let (Some(d), GitAccess::Worktree) = (&dirs, access)
            && !rules.is_empty()
        {
            // The branch checked out in the main checkout may have changed.
            let heads = d.common.join("logs/refs/heads");
            let old: Vec<String> = fs
                .iter()
                .filter(|(k, v)| v.as_str() == Some("read") && Path::new(k).starts_with(&heads))
                .map(|(k, _)| k.clone())
                .collect();
            for k in old {
                fs.remove(&k);
                changed = true;
            }
            changed |= add_rules(fs, std::mem::take(&mut rules));
        }
        let mut repos = granted_repos(fs);
        let fs = profile_fs(&mut t, FULL_PROFILE);
        changed |= add_rules(fs, rules);
        repos.extend(granted_repos(fs));
        // Every repository a session could write in: refuse if one of them
        // has been changed in a way git never does.
        repos.sort();
        repos.dedup();
        for r in &repos {
            crate::worktree::check_common_dir(r)?;
        }
        if changed {
            self.write_config(&t)?;
        }
        Ok(notice)
    }

    /// Ask a throwaway app-server which of our hooks it trusts. Returns the
    /// key and current hash of every hook defined in our `config.toml`.
    async fn hook_status(&self) -> Result<Vec<(String, String, String)>> {
        let home = self.home();
        let r = crate::codex_rpc::stdio_calls(
            &self.env.codex_bin,
            &home,
            &[(
                "hooks/list",
                serde_json::json!({"cwds": [home.to_string_lossy()]}),
            )],
        )
        .await?;
        self.parse_hook_status(&r[0])
    }

    fn parse_hook_status(&self, response: &Value) -> Result<Vec<(String, String, String)>> {
        let mut out = Vec::new();
        let config = self.hook_config_path();
        for entry in response["data"].as_array().into_iter().flatten() {
            for h in entry["hooks"].as_array().into_iter().flatten() {
                let src = h["sourcePath"].as_str().unwrap_or_default();
                if Path::new(src) != config {
                    continue;
                }
                out.push((
                    h["key"].as_str().unwrap_or_default().to_string(),
                    h["currentHash"].as_str().unwrap_or_default().to_string(),
                    h["trustStatus"].as_str().unwrap_or_default().to_string(),
                ));
            }
        }
        if out.len() != EVENTS.len() {
            bail!(
                "codex reported {} managed hooks from {}, expected {}",
                out.len(),
                config.display(),
                EVENTS.len()
            );
        }
        Ok(out)
    }

    /// Keep the daemon at the `codex` command's version. Codex installs the
    /// daemon as a package under the home (`packages/app-server-daemon`)
    /// and, by default, updates it on its own; a TUI refuses to attach to a
    /// daemon of another version ("Background server has incompatible
    /// feature settings") and offers to restart it from the session. So:
    /// the daemon's `settings.json` turns the self-update off, and a
    /// package whose version differs from the command's is removed before
    /// the start, which installs the command's own version again. Best
    /// effort: a failure here is logged and the start goes ahead.
    async fn pin_daemon_package(&self) {
        let dir = self.home().join("app-server-daemon");
        let settings = dir.join("settings.json");
        let mut v: serde_json::Value = std::fs::read_to_string(&settings)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .filter(|v: &serde_json::Value| v.is_object())
            .unwrap_or_else(|| serde_json::json!({}));
        let updater = v
            .as_object_mut()
            .unwrap()
            .entry("updater")
            .or_insert_with(|| serde_json::json!({}));
        if !updater.is_object() {
            *updater = serde_json::json!({});
        }
        if updater.get("autoUpdateEnabled") != Some(&serde_json::Value::Bool(false)) {
            updater["autoUpdateEnabled"] = serde_json::Value::Bool(false);
            let text = serde_json::to_string_pretty(&v).unwrap_or_default();
            if let Err(e) = std::fs::create_dir_all(&dir)
                .map_err(anyhow::Error::from)
                .and_then(|()| crate::persist::write_atomic(&settings, text.as_bytes(), 0o600))
            {
                tracing::warn!("codex daemon settings {}: {e:#}", settings.display());
            }
        }
        let version = tokio::process::Command::new(&self.env.codex_bin)
            .args(["app-server", "daemon", "version"])
            .env("CODEX_HOME", self.home())
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), version).await else {
            return;
        };
        // The last line of the output is JSON with `cliVersion` and, once a
        // package is installed, `managedCodexVersion`.
        let text = String::from_utf8_lossy(&out.stdout);
        let Some(v) = text
            .lines()
            .rev()
            .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        else {
            return;
        };
        let (Some(cli), Some(managed)) = (
            v.get("cliVersion").and_then(|x| x.as_str()),
            v.get("managedCodexVersion").and_then(|x| x.as_str()),
        ) else {
            return;
        };
        if cli == managed {
            return;
        }
        let packages = self.home().join("packages").join("app-server-daemon");
        match std::fs::remove_dir_all(&packages) {
            Ok(()) => tracing::warn!(
                "codex daemon package {managed} differs from the codex command ({cli}): removed {} \
                 so the start installs {cli} again",
                packages.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("removing {}: {e}", packages.display()),
        }
    }

    /// Whether the daemon answers on its control socket (`Err`: why not).
    async fn daemon_up(&self) -> std::io::Result<()> {
        crate::paths::connect_unix(&self.control_socket())
            .await
            .map(drop)
    }

    /// Start the home's app-server daemon if it is not running. The daemon
    /// is the environment hooks and agent tool commands run in, so it gets
    /// the node socket, the server URL and the node's Codex pass, and none
    /// of a seat's variables.
    pub async fn ensure_daemon(&self) -> Result<()> {
        let _g = self.lock.lock().await;
        if self.daemon_up().await.is_ok() {
            return Ok(());
        }
        self.pin_daemon_package().await;
        let out = tokio::process::Command::new(&self.env.codex_bin)
            .args(["app-server", "daemon", "start"])
            .env("CODEX_HOME", self.home())
            .env(crate::paths::SOCK_ENV, &self.env.node_socket)
            .env("AMAIL_URL", &self.env.server_url)
            .env("PATH", self.env.path_with_amail())
            .env(crate::wire::CODEX_PASS_ENV, &self.env.codex_pass)
            .env_remove("AMAIL_SEAT")
            .env_remove("AMAIL_TOKEN")
            .env_remove("AMAIL_PROFILE")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        let out = tokio::time::timeout(Duration::from_secs(60), out)
            .await
            .context("codex daemon start timed out")?
            .with_context(|| format!("running {} app-server daemon start", self.env.codex_bin))?;
        if !out.status.success() {
            bail!(
                "codex app-server daemon start failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let mut last = None;
        for _ in 0..100 {
            match self.daemon_up().await {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let path = self.control_socket();
        match last {
            Some(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                "the codex daemon started but its control socket {} did not appear",
                path.display()
            ),
            Some(e) => bail!(
                "cannot connect to the codex daemon's control socket {}: {e}",
                path.display()
            ),
            None => bail!("codex daemon is not reachable at {}", path.display()),
        }
    }

    /// Mark `cwd` as a trusted project so the TUI starts without asking.
    fn trust_project(&self, cwd: &str) -> Result<()> {
        let mut t = self.read_config()?;
        let projects = t
            .entry("projects")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let toml::Value::Table(projects) = projects else {
            bail!("config.toml: projects is not a table");
        };
        let already = projects
            .get(cwd)
            .and_then(|p| p.get("trust_level"))
            .and_then(|v| v.as_str())
            == Some("trusted");
        if already {
            return Ok(());
        }
        let mut p = toml::Table::new();
        p.insert("trust_level".into(), toml::Value::String("trusted".into()));
        projects.insert(cwd.to_string(), toml::Value::Table(p));
        self.write_config(&t)
    }

    fn check_extra(extra: &[String]) -> Result<()> {
        for a in extra {
            let flag = a.split('=').next().unwrap_or(a);
            if FORBIDDEN_FLAGS.contains(&flag)
                || (a.starts_with("-c") && a.len() > 2 && !a.starts_with("--"))
            {
                bail!(
                    "codex flag {a:?} is not allowed: it would detach the session from the shared daemon \
                     or bypass hook trust (allowed: -m, -a, -s, -C and other flags that keep the daemon)"
                );
            }
        }
        Ok(())
    }

    pub fn marker(seat: &str, nonce: &str) -> String {
        format!("[amail:seat={seat} launch={nonce}]")
    }

    /// The seat's settings for `thread/settings/update` (see
    /// [`Codex::apply_settings_on_resume`]): model, approval policy and
    /// permissions profile.
    pub fn resume_settings(
        thread: &str,
        model: Option<&str>,
        mode: PermissionMode,
        git_access: GitAccess,
    ) -> Vec<Value> {
        let mut first = json!({
            "threadId": thread,
            "approvalPolicy": if mode == PermissionMode::Unattended { "never" } else { "on-request" },
        });
        if let Some(m) = model.filter(|m| !m.is_empty()) {
            first["model"] = json!(m);
        }
        let profile = match git_access {
            GitAccess::Worktree => PERMISSIONS_PROFILE,
            GitAccess::Full => FULL_PROFILE,
            GitAccess::Off => OFF_PROFILE,
        };
        vec![first, json!({"threadId": thread, "permissions": profile})]
    }

    fn selected_resume_settings(
        &self,
        thread: &str,
        model: Option<&str>,
        mode: PermissionMode,
        git_access: GitAccess,
        effort: Option<&str>,
    ) -> Vec<Value> {
        let mut calls = Self::resume_settings(thread, model, mode, git_access);
        if let Some(effort) = effort {
            calls[0]["effort"] = json!(effort);
        }
        if let Some(sandbox) = self.env.codex_sandbox {
            calls[1]["permissions"] = json!(sandbox.permissions());
        }
        calls
    }

    fn check_effort(effort: &str) -> Result<()> {
        if !matches!(
            effort,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ) {
            bail!("unsupported managed Codex effort {effort:?}");
        }
        Ok(())
    }

    /// Why a wake-up must not be queued now, from the daemon: the thread
    /// already holds one in its queue (it is busy, or has no terminal and
    /// runs its queue when resumed), or a turn is running (the `Stop` hook
    /// checks for mail at its end). `codex queue` on a busy thread adds a
    /// turn that runs after the current one, when the mail has been shown
    /// already; a second nudge would be another such turn.
    async fn wake_held(&self, target: &str) -> Result<Option<&'static str>> {
        let r = crate::codex_rpc::socket_calls(
            &self.control_socket(),
            &[
                (
                    "thread/read",
                    json!({"threadId": target, "includeTurns": false}),
                ),
                ("thread/queue/list", json!({"threadId": target})),
            ],
        )
        .await?;
        let queued = r
            .get(1)
            .and_then(|q| q["data"].as_array())
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["input"].as_array().is_some_and(|input| {
                        input.iter().any(|part| {
                            part["text"]
                                .as_str()
                                .is_some_and(crate::inject::is_wake_text)
                        })
                    })
                })
            });
        if queued {
            return Ok(Some("a wake-up is already queued on the thread"));
        }
        let status = r
            .first()
            .and_then(|t| t["thread"]["status"]["type"].as_str());
        if status == Some("active") {
            return Ok(Some(
                "the thread runs a turn; the Stop hook checks for mail at its end",
            ));
        }
        Ok(None)
    }

    /// The node persists the intended effort with a seat's resume ID.
    /// Read it at wake time too: a controller restart does not relaunch
    /// live seats, and must not discard this per-thread setting.
    fn resume_effort(&self, thread: &str) -> Result<Option<String>> {
        let path = self.env.state_dir.join("node-state.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let state: Value = serde_json::from_str(&text)?;
        let mut efforts = Vec::new();
        for seat in state["managed"]
            .as_object()
            .into_iter()
            .flat_map(|m| m.values())
        {
            if seat["resume"].as_str() == Some(thread)
                && let Some(effort) = seat["codex_effort"].as_str()
            {
                Self::check_effort(effort)?;
                efforts.push(effort.to_string());
            }
        }
        if efforts.len() > 1 {
            bail!("multiple managed seats specify the effort of Codex thread {thread}");
        }
        Ok(efforts.pop())
    }

    /// A resumed session may attach to a thread the shared daemon still
    /// has loaded (its previous session ended a moment ago, e.g. when the
    /// node starts a seat again right after `amail sleep`). Codex then
    /// keeps the thread's earlier settings and ignores `-m`. So the node
    /// sets the seat's settings on the thread itself: once now (a thread
    /// still loaded takes them; one that is not loaded is loaded with the
    /// launch flags instead) and again after the session has attached.
    fn apply_settings_on_resume(&self, thread: &str, ctx: &LaunchCtx<'_>) {
        let calls = self.selected_resume_settings(
            thread,
            ctx.info.model.as_deref(),
            ctx.permission_mode,
            ctx.git_access,
            ctx.codex_effort,
        );
        let socket = self.control_socket();
        let (thread, seat) = (thread.to_string(), ctx.info.seat.clone());
        tokio::spawn(async move {
            for delay in [0u64, 3, 10] {
                tokio::time::sleep(Duration::from_secs(delay)).await;
                for params in &calls {
                    match crate::codex_rpc::socket_calls(
                        &socket,
                        &[("thread/settings/update", params.clone())],
                    )
                    .await
                    {
                        Ok(_) => {
                            tracing::debug!("seat {seat}: set {params} on codex thread {thread}")
                        }
                        Err(e) => tracing::debug!(
                            "seat {seat}: setting {params} on codex thread {thread}: {e:#}"
                        ),
                    }
                }
            }
        });
    }
}

/// The dedicated Codex home in a node state directory.
pub fn home_in(state_dir: &Path) -> PathBuf {
    state_dir.join("codex-home")
}

/// Processes of this user whose environment contains `entry` exactly.
fn processes_with_env(entry: &str) -> Vec<u32> {
    let me = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ"))
                .map(|env| env.split(|b| *b == 0).any(|kv| kv == entry.as_bytes()))
                .unwrap_or(false)
        })
        .collect()
}

/// Parse `[amail:seat=<seat> launch=<nonce>]` anywhere in `prompt`.
pub fn parse_marker(prompt: &str) -> Option<(String, String)> {
    let start = prompt.find("[amail:seat=")?;
    let rest = &prompt[start + "[amail:seat=".len()..];
    let end = rest.find(']')?;
    let (seat, nonce) = rest[..end].split_once(" launch=")?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    (ok(seat) && ok(nonce)).then(|| (seat.to_string(), nonce.to_string()))
}

/// The turns of a `thread/turns/list` answer, newest first as listed.
/// Unknown statuses count as in progress.
pub fn parse_turns(v: &Value) -> Vec<TurnInfo> {
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| TurnInfo {
            status: match t["status"].as_str() {
                Some("completed") => TurnStatus::Completed,
                Some("interrupted") => TurnStatus::Interrupted,
                Some("failed") => TurnStatus::Failed(
                    t["error"]["message"]
                        .as_str()
                        .unwrap_or("the turn failed")
                        .to_string(),
                ),
                _ => TurnStatus::InProgress,
            },
            started_at: t["startedAt"].as_i64(),
            id: t["id"].as_str().map(str::to_string),
        })
        .collect()
}

#[async_trait::async_trait]
impl Adapter for Codex {
    fn harness(&self) -> Harness {
        Harness::Codex
    }

    fn parse(&self, input: &Value) -> Option<Hook> {
        let name = str_field(input, "hook_event_name")?.to_string();
        let event = match name.as_str() {
            "SessionStart" => Event::SessionStart {
                source: str_field(input, "source").unwrap_or("startup").to_string(),
            },
            "UserPromptSubmit" => Event::UserPromptSubmit {
                prompt: str_field(input, "prompt").unwrap_or_default().to_string(),
            },
            "PostToolUse" => Event::PostToolUse,
            "Stop" => Event::Stop,
            "Interrupt" => Event::Interrupt,
            "PermissionRequest" => Event::Awaiting,
            "SessionEnd" => Event::SessionEnd,
            other => compact_event(other, input)?,
        };
        Some(Hook {
            name,
            event,
            session_id: str_field(input, "session_id").map(str::to_string),
            transcript_path: str_field(input, "transcript_path")
                .filter(|p| !p.is_empty())
                .map(str::to_string),
            facts: HookFacts::from_input(input),
        })
    }

    fn seat_from_env(&self, _env: &BTreeMap<String, String>) -> Option<String> {
        // Hooks run in the shared daemon: its environment names no seat.
        None
    }

    fn registers_sessions(&self) -> bool {
        true
    }

    fn registration_marker(&self, prompt: &str) -> Option<(String, String)> {
        parse_marker(prompt)
    }

    fn session_hint(&self, _env: &BTreeMap<String, String>, hook: &Hook) -> SessionHint {
        SessionHint {
            pid: None,
            wake_target: hook.session_id.clone(),
        }
    }

    fn session_start_state(&self, _source: &str) -> Option<SeatState> {
        // SessionStart fires lazily, when the first turn starts.
        Some(SeatState::Busy)
    }

    fn inject_budget(&self) -> usize {
        // Stay well under Codex's hook context limit (about 2,500 tokens).
        6_000
    }

    fn claim_limit(&self) -> u32 {
        5
    }

    /// Codex hooks have no event for a failed turn (0.160: `Stop` and
    /// `Interrupt` only), so the node asks the shared app-server.
    fn checks_turns(&self) -> bool {
        true
    }

    async fn last_turns(&self, target: &str) -> Result<Vec<TurnInfo>> {
        let r = crate::codex_rpc::socket_calls(
            &self.control_socket(),
            &[(
                "thread/turns/list",
                json!({"threadId": target, "limit": 3, "sortDirection": "desc",
                       "itemsView": "notLoaded"}),
            )],
        )
        .await?;
        Ok(parse_turns(r.first().unwrap_or(&Value::Null)))
    }

    async fn completed_quiet_turn(
        &self,
        probe: &QuietTurnContext,
        history_error: Option<&anyhow::Error>,
    ) -> Result<Option<QuietTurnProof>> {
        quiet::recover(self, probe, history_error).await
    }

    async fn wake(&self, target: &str, text: &str) -> Result<()> {
        // The TUI can apply the shared home's default effort on attach.
        // Restore this seat's intended value before submitting any nudge,
        // so its next model turn cannot race the delayed resume settings.
        if let Some(effort) = self.resume_effort(target)? {
            crate::codex_rpc::socket_calls(
                &self.control_socket(),
                &[(
                    "thread/settings/update",
                    json!({"threadId": target, "effort": effort}),
                )],
            )
            .await
            .context("restoring Codex effort before queueing wake-up")?;
        }
        match self.wake_held(target).await {
            Ok(Some(why)) => {
                tracing::info!("codex thread {target}: not queueing a wake-up: {why}");
                return Ok(());
            }
            Ok(None) => {}
            // Without an answer the nudge is queued as before.
            Err(e) => {
                tracing::debug!("codex thread {target}: asking the daemon before queueing: {e:#}")
            }
        }
        let out = tokio::process::Command::new(&self.env.codex_bin)
            .args(["queue", "--thread", target, "--message", text])
            .env("CODEX_HOME", self.home())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output();
        let out = tokio::time::timeout(Duration::from_secs(15), out)
            .await
            .context("codex queue timed out")??;
        if !out.status.success() {
            bail!(
                "codex queue failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    async fn prepare(&self) -> Result<()> {
        let _g = self.lock.lock().await;
        let home = self.home();
        std::fs::create_dir_all(&home)?;
        std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let auth = home.join("auth.json");
        if std::fs::symlink_metadata(&auth).is_err() {
            std::os::unix::fs::symlink(&self.env.codex_auth, &auth)
                .with_context(|| format!("linking {}", auth.display()))?;
        }
        let trust: BTreeMap<String, String> = EVENTS
            .iter()
            .map(|(_, snake)| self.trust_entry(snake))
            .collect();
        let mut t = self.read_config()?;
        self.apply_managed(&mut t, &trust);
        self.write_config(&t)?;
        // Check with Codex itself; adopt its keys and hashes if ours differ.
        match self.hook_status().await {
            Ok(status) => {
                if status.iter().all(|(_, _, s)| s == "trusted") && status.len() == EVENTS.len() {
                    return Ok(());
                }
                let trust: BTreeMap<String, String> = status
                    .iter()
                    .map(|(k, h, _)| (k.clone(), h.clone()))
                    .collect();
                self.apply_managed(&mut t, &trust);
                self.write_config(&t)?;
                let status = self.hook_status().await?;
                if let Some((k, _, s)) = status.iter().find(|(_, _, s)| s != "trusted") {
                    bail!("codex hook {k} is {s} after trusting it");
                }
            }
            Err(e) => return Err(e.context("could not verify codex hook trust")),
        }
        Ok(())
    }

    async fn launch(&self, ctx: &LaunchCtx<'_>) -> Result<LaunchPlan> {
        Self::check_extra(ctx.extra)?;
        if let Some(effort) = ctx.codex_effort {
            Self::check_effort(effort)?;
        }
        let notice;
        {
            let _g = self.lock.lock().await;
            self.trust_project(ctx.cwd)?;
            notice = self.grant_git(ctx.cwd, &ctx.info.seat, ctx.git_access)?;
        }
        if let Err(e) = self.ensure_daemon().await {
            // The TUI starts a daemon itself; it inherits the launch
            // environment, which carries the node socket, so hooks still work.
            tracing::warn!("{e:#}");
        }
        let info = ctx.info;
        let mut args = Vec::new();
        if ctx.resume.is_some() {
            // Queued messages for the thread run when it is resumed.
            args.push("resume".to_string());
        }
        if let Some(m) = &info.model {
            args.extend(["-m".to_string(), m.clone()]);
        }
        if ctx.permission_mode == PermissionMode::Unattended {
            // No `-s`: the sandbox comes from the `amail` permissions profile
            // (workspace + network + the seat's git directories).
            args.extend(["-a", "never"].map(String::from));
        }
        if let Some(sandbox) = self.env.codex_sandbox {
            args.extend(["-s".to_string(), sandbox.as_str().to_string()]);
        }
        args.extend(ctx.extra.iter().cloned());
        if let Some(id) = ctx.resume {
            args.push(id.to_string());
            // A resumed thread keeps the instructions of its first prompt,
            // so an adopted one is told here that it is now a seat
            // (`codex resume [SESSION_ID] [PROMPT]`). Its hooks are tied to
            // the seat by the registration the node made when adopting.
            if ctx.adopted {
                args.push(adopted_prompt(info));
            }
        } else {
            let start = if ctx.context_lost {
                CONTEXT_LOST
            } else {
                "This is the start of your session. Handle any new mail shown to you. \
                 If there is none, say that you are ready and stop."
            };
            args.push(format!(
                "{}\n{}\n\n{start}",
                Self::marker(&info.seat, ctx.nonce),
                crate::inject::seat_intro(info)
            ));
        }
        if let Some(id) = ctx.resume {
            self.apply_settings_on_resume(id, ctx);
        }
        let env = BTreeMap::from([
            (
                "CODEX_HOME".to_string(),
                self.home().to_string_lossy().into_owned(),
            ),
            ("AMAIL_SEAT".to_string(), info.seat.clone()),
            ("AMAIL_URL".to_string(), self.env.server_url.clone()),
            // No `AMAIL_TOKEN`: tool commands and the MCP server find the
            // seat through the node by thread id. The TUI starts (or, when
            // Codex asks it to, restarts) the shared daemon with this
            // environment, and with a token here every thread on that
            // daemon would act as this seat.
            (
                crate::paths::SOCK_ENV.to_string(),
                self.env.node_socket.to_string_lossy().into_owned(),
            ),
            // For a daemon the TUI starts itself (see `ensure_daemon`).
            (
                crate::wire::CODEX_PASS_ENV.to_string(),
                self.env.codex_pass.clone(),
            ),
        ]);
        Ok(LaunchPlan {
            program: self.env.codex_bin.clone(),
            args,
            env,
            unset_env: UNSET.iter().map(|s| s.to_string()).collect(),
            path_prepend: self.env.amail_dir().into_iter().collect(),
            notice,
        })
    }

    fn resume_is_lazy(&self) -> bool {
        true
    }

    async fn shutdown(&self) {
        let _g = self.lock.lock().await;
        let out = tokio::process::Command::new(&self.env.codex_bin)
            .args(["app-server", "daemon", "stop"])
            .env("CODEX_HOME", self.home())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = tokio::time::timeout(Duration::from_secs(20), out).await;
        // `daemon stop` leaves helpers (the pid-update loop) running: end
        // every remaining process that runs with our CODEX_HOME.
        let want = format!("CODEX_HOME={}", self.home().display());
        for pid in processes_with_env(&want) {
            tracing::info!("stopping leftover codex process {pid}");
            // SAFETY: plain signal to a process of ours.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }

    async fn registered(&self, session_id: &str, seat: &str) {
        let r = crate::codex_rpc::socket_calls(
            &self.control_socket(),
            &[(
                "thread/name/set",
                serde_json::json!({"threadId": session_id, "name": seat}),
            )],
        )
        .await;
        if let Err(e) = r {
            tracing::warn!("naming codex thread {session_id} as {seat}: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker() {
        let m = Codex::marker("w-1", "ab12");
        assert_eq!(
            parse_marker(&format!("{m}\nYou are...")),
            Some(("w-1".into(), "ab12".into()))
        );
        assert_eq!(parse_marker("hello"), None);
        assert_eq!(parse_marker("[amail:seat=a b launch=x]"), None);
        assert_eq!(parse_marker("[amail:seat=a launch=]"), None);
    }

    #[test]
    fn extra_flags() {
        assert!(Codex::check_extra(&["-m".into(), "x".into(), "-C".into(), "/w".into()]).is_ok());
        for bad in [
            "-c",
            "-ckey=v",
            "--config=x",
            "-p",
            "--remote",
            "--approve-for-me",
        ] {
            assert!(Codex::check_extra(&[bad.into()]).is_err(), "{bad}");
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
    }

    /// A repository `repo` (branch `main`) with linked worktrees `wt` and
    /// `wt2`, packed refs, and a Codex adapter whose profile still has the
    /// grant of an older amail on the root of `repo/.git`.
    fn setup() -> (tempfile::TempDir, PathBuf, Codex) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a"), "a\n").unwrap();
        git(&repo, &["add", "a"]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        git(&repo, &["worktree", "add", "-q", "-b", "s1", "../wt"]);
        git(&repo, &["worktree", "add", "-q", "-b", "s2", "../wt2"]);
        git(&repo, &["branch", "packed"]);
        git(&repo, &["pack-refs", "--all"]);
        let codex = Codex::new(AdapterEnv {
            state_dir: root.join("state"),
            amail_exe: "/bin/amail".into(),
            node_socket: root.join("node.sock"),
            server_url: "http://127.0.0.1:1".into(),
            claude_bin: "claude".into(),
            codex_bin: "codex".into(),
            codex_sandbox: None,
            dsh_bin: "dsh".into(),
            dsh_env: BTreeMap::new(),
            codex_auth: root.join("auth.json"),
            codex_pass: String::new(),
        });
        std::fs::create_dir_all(codex.home()).unwrap();
        let mut t = toml::Table::new();
        codex.apply_managed(&mut t, &BTreeMap::new());
        let fs = profile_fs(&mut t, PERMISSIONS_PROFILE);
        let common = repo.join(".git");
        fs.insert(common.to_string_lossy().into(), "write".into());
        fs.insert(common.join("hooks").to_string_lossy().into(), "read".into());
        codex.write_config(&t).unwrap();
        (tmp, root, codex)
    }

    #[test]
    fn managed_effort_survives_controller_recreation_and_stays_per_thread() {
        let (_tmp, _root, codex) = setup();
        assert_eq!(codex.resume_effort("system").unwrap(), None);
        let path = codex.env.state_dir.join("node-state.json");
        let mut state = json!({"managed": {
            "system": {"resume": "system-id", "codex_effort": "xhigh"},
            "worker": {"resume": "worker-id", "codex_effort": "max"}
        }});
        std::fs::write(&path, state.to_string()).unwrap();
        assert_eq!(
            codex.resume_effort("system-id").unwrap().as_deref(),
            Some("xhigh")
        );
        assert_eq!(
            codex.resume_effort("worker-id").unwrap().as_deref(),
            Some("max")
        );
        assert_eq!(codex.resume_effort("unmanaged-id").unwrap(), None);
        state["managed"]["system"]["codex_effort"] = json!("high");
        std::fs::write(&path, state.to_string()).unwrap();
        assert_eq!(
            codex.resume_effort("system-id").unwrap().as_deref(),
            Some("high")
        );
        state["managed"]["worker"]["resume"] = json!("system-id");
        std::fs::write(&path, state.to_string()).unwrap();
        assert!(codex.resume_effort("system-id").is_err());
        assert!(Codex::check_effort("arbitrary-config=true").is_err());
    }

    #[test]
    fn managed_hooks_keep_context_locator_on_regeneration_without_secrets() {
        let (_tmp, root, mut codex) = setup();
        codex.env.node_socket = root.join("socket with ' quote.sock");
        codex.env.codex_pass = "must-never-be-in-config".into();
        let expected = format!(
            "{} hook codex --node-state-dir {} --node-socket {}",
            shell_quote(&codex.env.amail_exe.to_string_lossy()),
            shell_quote(&codex.env.state_dir.to_string_lossy()),
            shell_quote(&codex.env.node_socket.to_string_lossy()),
        );
        let trust: BTreeMap<_, _> = EVENTS
            .iter()
            .map(|(_, snake)| codex.trust_entry(snake))
            .collect();
        let mut config = toml::Table::new();
        codex.apply_managed(&mut config, &trust);
        let once = config.clone();
        codex.apply_managed(&mut config, &trust);
        assert_eq!(config, once);
        for (event, snake) in EVENTS {
            assert_eq!(
                config["hooks"][event][0]["hooks"][0]["command"].as_str(),
                Some(expected.as_str())
            );
            let (key, hash) = codex.trust_entry(snake);
            assert_eq!(
                config["hooks"]["state"][&key]["trusted_hash"].as_str(),
                Some(hash.as_str())
            );
        }
        assert!(
            !toml::to_string(&config)
                .unwrap()
                .contains(&codex.env.codex_pass)
        );
    }

    #[test]
    fn hook_trust_resolves_symlinked_home_and_rejects_missing_hooks() {
        let (_tmp, root, mut codex) = setup();
        let physical_config = codex.config_path();
        let alias = root.join("short-state");
        std::os::unix::fs::symlink(&codex.env.state_dir, &alias).unwrap();
        codex.env.state_dir = alias;

        let hooks: Vec<_> = EVENTS
            .iter()
            .map(|(_, snake)| {
                let (key, hash) = codex.trust_entry(snake);
                assert_eq!(key, format!("{}:{snake}:0:0", physical_config.display()));
                json!({"sourcePath": physical_config, "key": key,
                       "currentHash": hash, "trustStatus": "trusted"})
            })
            .collect();
        let response = json!({"data": [{"hooks": hooks}]});
        let status = codex.parse_hook_status(&response).unwrap();
        assert_eq!(status.len(), EVENTS.len());
        assert!(status.iter().all(|(_, _, s)| s == "trusted"));
        assert!(codex.parse_hook_status(&json!({"data": []})).is_err());
        let mut partial = response;
        partial["data"][0]["hooks"].as_array_mut().unwrap().pop();
        assert!(codex.parse_hook_status(&partial).is_err());
    }

    #[test]
    fn explicit_sandbox_survives_prepare_and_resume_without_git_changes() {
        let (_tmp, root, mut codex) = setup();
        let repo = root.join("repo");
        let packed = std::fs::read(repo.join(".git/packed-refs")).unwrap();
        let mut config = codex.read_config().unwrap();
        assert_eq!(
            config["default_permissions"].as_str(),
            Some(PERMISSIONS_PROFILE)
        );

        codex.env.codex_sandbox = Some(SandboxMode::DangerFullAccess);
        for _ in 0..2 {
            codex.apply_managed(&mut config, &BTreeMap::new());
            codex.write_config(&config).unwrap();
            assert_eq!(
                config["default_permissions"].as_str(),
                Some(":danger-full-access")
            );
            assert_eq!(
                codex
                    .grant_git(repo.to_str().unwrap(), "seat", GitAccess::Full)
                    .unwrap(),
                None
            );
            assert_eq!(
                codex
                    .grant_git(
                        root.join("wt").to_str().unwrap(),
                        "seat",
                        GitAccess::Worktree
                    )
                    .unwrap(),
                None
            );
            let calls = codex.selected_resume_settings(
                "existing-thread",
                Some("kept-model"),
                PermissionMode::Unattended,
                GitAccess::Full,
                Some("xhigh"),
            );
            assert_eq!(calls[0]["model"], "kept-model");
            assert_eq!(calls[0]["approvalPolicy"], "never");
            assert_eq!(calls[0]["effort"], "xhigh");
            assert_eq!(calls[1]["permissions"], ":danger-full-access");
        }
        assert!(!repo.join(".codex/config.toml").exists());
        assert!(!root.join("wt/.codex/config.toml").exists());
        assert!(!repo.join(".git/packed-refs").is_symlink());
        assert_eq!(
            std::fs::read(repo.join(".git/packed-refs")).unwrap(),
            packed
        );
    }

    fn rules(codex: &Codex, profile: &str) -> BTreeMap<String, String> {
        let t = codex.read_config().unwrap();
        t["permissions"][profile]["filesystem"]
            .as_table()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
            .collect()
    }

    fn project_config(dir: &Path) -> Option<String> {
        std::fs::read_to_string(dir.join(".codex/config.toml")).ok()
    }

    /// `worktree` (the default): no write on the root of the repository's
    /// git directory, so `commondir`, `config`, `hooks/` and `info/` cannot
    /// be created or changed; only objects, refs, branch reflogs, the
    /// packed-refs lock directory and the worktree's own git directory.
    #[test]
    fn worktree_grant_keeps_git_dir_root_read_only() {
        let (_tmp, root, codex) = setup();
        let c = root.join("repo/.git");
        let notice = codex
            .grant_git(
                &root.join("wt").to_string_lossy(),
                "s1",
                GitAccess::Worktree,
            )
            .unwrap();
        assert_eq!(notice, None);
        let got = rules(&codex, PERMISSIONS_PROFILE);
        let p = |s: &str| c.join(s).to_string_lossy().into_owned();
        let want: BTreeMap<String, String> = [
            (p("objects"), "write"),
            (p("refs"), "write"),
            (p("logs/refs"), "write"),
            (p("amail"), "write"),
            (p("amail/packed-refs"), "read"),
            (p("logs/refs/heads/main"), "read"),
            (p("worktrees/wt"), "write"),
            (p("worktrees/wt/commondir"), "read"),
            (p("worktrees/wt/gitdir"), "read"),
            (p("worktrees/wt/config.worktree"), "read"),
        ]
        .into_iter()
        .map(|(k, v)| (k, v.to_string()))
        .collect();
        assert_eq!(got, want);
        assert!(rules(&codex, FULL_PROFILE).is_empty());
        // packed-refs now lives in amail/, behind a symbolic link; git
        // outside the sandbox sees the same refs and can still rewrite it.
        assert_eq!(
            std::fs::read_link(c.join("packed-refs")).unwrap(),
            Path::new("amail/packed-refs")
        );
        let refs = git_out(&root.join("repo"), &["show-ref"]).unwrap();
        assert!(refs.contains("refs/heads/packed"), "{refs}");
        git(&root.join("repo"), &["branch", "-D", "packed"]);
        git(&root.join("repo"), &["pack-refs", "--all"]);
        assert!(c.join("packed-refs").is_symlink());
        assert!(c.join("amail/packed-refs").is_file());
        // A second seat adds its own worktree; nothing is written into the
        // worktrees themselves.
        codex
            .grant_git(
                &root.join("wt2").to_string_lossy(),
                "s2",
                GitAccess::Worktree,
            )
            .unwrap();
        let got = rules(&codex, PERMISSIONS_PROFILE);
        assert_eq!(
            got.get(&p("worktrees/wt2")).map(|s| s.as_str()),
            Some("write")
        );
        assert!(!got.contains_key(&p("")) && !got.contains_key(&c.to_string_lossy().into_owned()));
        assert_eq!(project_config(&root.join("wt")), None);
        assert_eq!(project_config(&root.join("wt2")), None);
        // A seat in the main checkout gets no grant, and a notice.
        let notice = codex
            .grant_git(
                &root.join("repo").to_string_lossy(),
                "m",
                GitAccess::Worktree,
            )
            .unwrap();
        assert!(notice.unwrap().contains("main checkout"));
        assert!(
            rules(&codex, PERMISSIONS_PROFILE)
                .iter()
                .all(|(k, v)| v != "write" || Path::new(k) != c),
        );
    }

    /// `full` and `off` select their own profile through a project config
    /// in the seat's working directory; `full` grants the git directory
    /// (minus hooks and config) and the main checkout.
    #[test]
    fn full_and_off_use_their_own_profile() {
        let (_tmp, root, codex) = setup();
        let c = root.join("repo/.git");
        let wt = root.join("wt");
        let wts = wt.to_string_lossy().into_owned();
        codex.grant_git(&wts, "root", GitAccess::Full).unwrap();
        let text = project_config(&wt).unwrap();
        assert!(
            text.starts_with("# Written by amail for seat root\n"),
            "{text}"
        );
        assert_eq!(project_profile(&text).as_deref(), Some(FULL_PROFILE));
        assert_eq!(
            std::fs::read_to_string(wt.join(".codex/.gitignore")).unwrap(),
            "*\n"
        );
        let full = rules(&codex, FULL_PROFILE);
        let mode = |p: &Path| full.get(&*p.to_string_lossy()).map(|s| s.as_str());
        assert_eq!(mode(&root.join("repo")), Some("write"));
        assert_eq!(mode(&c), Some("write"));
        for name in GIT_READ_ONLY {
            assert_eq!(mode(&c.join(name)), Some("read"), "{name}");
        }
        for w in ["wt", "wt2"] {
            assert_eq!(mode(&c.join("worktrees").join(w)), Some("write"));
            for name in LINKED_GIT_READ_ONLY {
                assert_eq!(mode(&c.join("worktrees").join(w).join(name)), Some("read"));
            }
        }
        // The default profile lost the old grant on the root and got nothing.
        assert!(rules(&codex, PERMISSIONS_PROFILE).is_empty());
        // Another seat with another git access cannot share the directory.
        let e = codex.grant_git(&wts, "w", GitAccess::Worktree).unwrap_err();
        assert!(e.to_string().contains("seat root uses"), "{e:#}");
        // Back to the default: amail removes its file.
        codex.grant_git(&wts, "root", GitAccess::Worktree).unwrap();
        assert_eq!(project_config(&wt), None);
        assert!(!wt.join(".codex").exists());
        codex.grant_git(&wts, "root", GitAccess::Off).unwrap();
        assert_eq!(
            project_profile(&project_config(&wt).unwrap()).as_deref(),
            Some(OFF_PROFILE)
        );
        // A project config amail did not write that picks a profile is
        // refused, here and in a directory above.
        let wt2 = root.join("wt2");
        std::fs::create_dir_all(wt2.join(".codex")).unwrap();
        std::fs::write(
            wt2.join(".codex/config.toml"),
            "default_permissions = \"amail-full\"\n",
        )
        .unwrap();
        let e = codex
            .grant_git(&wt2.to_string_lossy(), "s2", GitAccess::Worktree)
            .unwrap_err();
        assert!(e.to_string().contains("not written by amail"), "{e:#}");
        std::fs::create_dir(wt2.join("sub")).unwrap();
        let e = codex
            .grant_git(
                &wt2.join("sub").to_string_lossy(),
                "s2",
                GitAccess::Worktree,
            )
            .unwrap_err();
        assert!(e.to_string().contains("default_permissions"), "{e:#}");
    }

    /// Launches stop when a repository a session could write has a
    /// `commondir` file or a symbolic link in its reflogs.
    #[test]
    fn tampered_git_dir_stops_launches() {
        let (_tmp, root, codex) = setup();
        let c = root.join("repo/.git");
        let wts = root.join("wt").to_string_lossy().into_owned();
        codex.grant_git(&wts, "s1", GitAccess::Worktree).unwrap();
        std::os::unix::fs::symlink(root.join("victim"), c.join("logs/refs/heads/x")).unwrap();
        let e = codex
            .grant_git(&wts, "s1", GitAccess::Worktree)
            .unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e:#}");
        // Also for a seat in another directory: the profile is shared.
        let other = root.join("other");
        std::fs::create_dir(&other).unwrap();
        let e = codex
            .grant_git(&other.to_string_lossy(), "o", GitAccess::Worktree)
            .unwrap_err();
        assert!(e.to_string().contains("symbolic link"), "{e:#}");
        std::fs::remove_file(c.join("logs/refs/heads/x")).unwrap();
        std::fs::write(c.join("commondir"), "/elsewhere\n").unwrap();
        let e = codex.grant_git(&wts, "s1", GitAccess::Full).unwrap_err();
        assert!(e.to_string().contains("commondir"), "{e:#}");
    }
}
