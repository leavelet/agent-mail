//! Team files (`amail.toml`): a declarative seat tree plus how this host
//! runs it. Parsing, validation and expansion are pure: nothing here touches
//! the server, git or processes.
//!
//! ```toml
//! [team]
//! template = "staged-merge"     # built-in template, or a template directory
//! repo = "/path/to/repo"
//! workroot = "/path/to/worktrees"
//! idle_sleep = "30m"            # or "off" (default)
//! max_live = 40
//! restart = "on-failure"        # or "never"
//! local_policy = "restricted"   # can only make the node's policy stricter
//!
//! [defaults]
//! harness = "claude"
//! model = "sonnet"
//! permission_mode = "unattended"
//!
//! [[seat]]
//! name = "root"
//! role = "Overall technical lead."
//!
//! [[group]]
//! name = "backend"
//! parent = "root"
//! lead = { name = "be-lead", harness = "codex", model = "codex-model" }
//! workers = { count = 3, prefix = "be" }   # be-1 .. be-3, reporting to be-lead
//! ```

use crate::wire::GitAccess;
use amail_proto::{PermissionMode, valid_name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Names `amail up` uses for generated lists; seats and groups cannot take them.
pub const RESERVED_LISTS: &[&str] = &["leads", "all"];

const MAX_WORKERS: u32 = 200;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRaw {
    #[serde(default)]
    team: TeamRaw,
    #[serde(default)]
    defaults: SeatOpts,
    #[serde(default)]
    seat: Vec<SeatRaw>,
    #[serde(default)]
    group: Vec<GroupRaw>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TeamRaw {
    template: Option<String>,
    repo: Option<PathBuf>,
    workroot: Option<PathBuf>,
    idle_sleep: Option<String>,
    max_live: Option<u32>,
    restart: Option<String>,
    local_policy: Option<String>,
}

/// Settings a seat inherits from `[defaults]` (or a group's `workers`).
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeatOpts {
    harness: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    git_access: Option<String>,
    dsh_jail: Option<bool>,
    /// Allow the guarded terminal-paste wake-up (see the node).
    paste_fallback: Option<bool>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeatRaw {
    name: String,
    parent: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    git_access: Option<String>,
    dsh_jail: Option<bool>,
    role: Option<String>,
    role_file: Option<PathBuf>,
    cwd: Option<PathBuf>,
    display_name: Option<String>,
    paste_fallback: Option<bool>,
    /// `[[seat]]` only: join this group (as a worker of its lead, unless
    /// `parent` says otherwise).
    group: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupRaw {
    name: String,
    parent: Option<String>,
    lead: SeatRaw,
    /// `{ count, prefix, ... }`, or an array of seat tables.
    workers: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkersRaw {
    count: u32,
    prefix: Option<String>,
    harness: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    git_access: Option<String>,
    dsh_jail: Option<bool>,
    role: Option<String>,
    role_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    OnFailure,
    Never,
}

/// Lifecycle settings the node applies to the team's seats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Put a seat to sleep after it has been idle this long.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_sleep_ms: Option<u64>,
    /// Most seats with a running process on this host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_live: Option<u32>,
    pub restart: Restart,
    /// `[team] local_policy`: `restricted` makes the node use the
    /// restricted policy whatever its own setting; `open` does not loosen
    /// the node's setting (see [`amail_proto::LocalPolicy`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_policy: Option<amail_proto::LocalPolicy>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            idle_sleep_ms: None,
            max_live: None,
            restart: Restart::OnFailure,
            local_policy: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Template {
    /// Working directory from `cwd`, role from `role`/`role_file`.
    None,
    /// Built-in: one git worktree per seat, merge requests up the tree.
    StagedMerge,
    /// A directory of role texts (`root.md`, `lead.md`, `worker.md`).
    Custom(PathBuf),
}

/// Where a seat sits in the tree, for role texts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// No parent.
    Root,
    /// Has a parent and subordinates.
    Lead,
    /// Has a parent, no subordinates.
    Worker,
}

impl Position {
    pub fn as_str(self) -> &'static str {
        match self {
            Position::Root => "root",
            Position::Lead => "lead",
            Position::Worker => "worker",
        }
    }
}

/// One seat after expansion, with defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatPlan {
    pub name: String,
    pub parent: Option<String>,
    pub group: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub permission_mode: Option<PermissionMode>,
    /// `git_access` as written in the file (see [`Team::git_access`]).
    pub git_access: Option<GitAccess>,
    /// `dsh_jail` (dsh seats): `false` runs an unattended seat's host
    /// without its bwrap jail. `None`: on.
    pub dsh_jail: Option<bool>,
    /// The seat's own role text (`role` or the contents of `role_file`).
    pub role: Option<String>,
    /// Explicit working directory (absolute).
    pub cwd: Option<PathBuf>,
    pub display_name: Option<String>,
    /// When native wake-ups fail, type the nudge into the terminal (guarded).
    pub paste_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPlan {
    pub name: String,
    pub lead: String,
    pub workers: Vec<String>,
}

/// An expanded team file.
#[derive(Debug, Clone)]
pub struct Team {
    /// Directory of the team file; relative paths are resolved against it.
    pub base: PathBuf,
    pub template: Template,
    pub repo: Option<PathBuf>,
    pub workroot: Option<PathBuf>,
    pub settings: Settings,
    /// Parents come before their children.
    pub seats: Vec<SeatPlan>,
    pub groups: Vec<GroupPlan>,
}

/// A validation error, pointing at the entry it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamError(pub String);

impl std::fmt::Display for TeamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TeamError {}

fn err<T>(msg: impl Into<String>) -> Result<T, TeamError> {
    Err(TeamError(msg.into()))
}

/// Parse `"30m"`, `"1h30m"`, `"45s"`, `"2d"`; `"off"` is `None`.
pub fn parse_duration(s: &str) -> Result<Option<Duration>, String> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("off") || t == "0" {
        return Ok(None);
    }
    let mut total: u64 = 0;
    let mut num = String::new();
    let mut any = false;
    for c in t.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num
            .parse()
            .map_err(|_| format!("invalid duration {s:?} (examples: 30m, 1h30m, 45s, off)"))?;
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(format!("invalid duration {s:?}: unknown unit {c:?}")),
        };
        total = total.saturating_add(n.saturating_mul(unit));
        num.clear();
        any = true;
    }
    if !num.is_empty() || !any {
        return Err(format!(
            "invalid duration {s:?}: every number needs a unit (s, m, h, d)"
        ));
    }
    if total == 0 {
        return Ok(None);
    }
    Ok(Some(Duration::from_secs(total)))
}

fn abs(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn harness_name(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" | "claude_code" => Some("claude"),
        "codex" => Some("codex"),
        "dsh" | "deepseek" | "deepseek-harness" => Some("dsh"),
        _ => None,
    }
}

/// Read and expand a team file.
pub fn load(path: &Path) -> Result<Team, TeamError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| TeamError(format!("reading {}: {e}", path.display())))?;
    let base = path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    parse(&text, &base).map_err(|e| TeamError(format!("{}: {e}", path.display())))
}

/// Expand team-file text; relative paths are resolved against `base`.
pub fn parse(text: &str, base: &Path) -> Result<Team, TeamError> {
    let raw: FileRaw = toml::from_str(text).map_err(|e| TeamError(e.to_string()))?;
    let t = &raw.team;
    let template = match t.template.as_deref().map(str::trim) {
        None | Some("") | Some("none") => Template::None,
        Some("staged-merge") => Template::StagedMerge,
        Some(other) => {
            let dir = abs(base, Path::new(other));
            if !dir.is_dir() {
                return err(format!(
                    "[team] template {other:?}: not a built-in template (staged-merge) \
                     and not a directory"
                ));
            }
            Template::Custom(dir)
        }
    };
    let repo = t.repo.as_deref().map(|p| abs(base, p));
    let workroot = t.workroot.as_deref().map(|p| abs(base, p));
    let idle_sleep = match &t.idle_sleep {
        Some(s) => parse_duration(s).map_err(|e| TeamError(format!("[team] idle_sleep: {e}")))?,
        None => None,
    };
    let restart = match t.restart.as_deref().map(str::trim) {
        None | Some("on-failure") => Restart::OnFailure,
        Some("never") => Restart::Never,
        Some(other) => {
            return err(format!(
                "[team] restart {other:?}: expected \"on-failure\" or \"never\""
            ));
        }
    };
    if t.max_live == Some(0) {
        return err("[team] max_live must be at least 1");
    }
    let local_policy = match t.local_policy.as_deref().map(str::trim) {
        None => None,
        Some(s) => Some(s.parse::<amail_proto::LocalPolicy>().map_err(|_| {
            TeamError(format!(
                "[team] local_policy {s:?}: expected \"open\" or \"restricted\""
            ))
        })?),
    };
    let settings = Settings {
        idle_sleep_ms: idle_sleep.map(|d| d.as_millis() as u64),
        max_live: t.max_live,
        restart,
        local_policy,
    };

    let defaults = &raw.defaults;
    let mut seats: Vec<SeatPlan> = Vec::new();
    let mut groups: Vec<GroupPlan> = Vec::new();
    let mut where_: HashMap<String, String> = HashMap::new();

    let mut add = |what: String,
                   s: &SeatRaw,
                   parent: Option<String>,
                   group: Option<String>,
                   inherit: &SeatOpts,
                   seats: &mut Vec<SeatPlan>|
     -> Result<(), TeamError> {
        if !valid_name(&s.name) {
            return err(format!(
                "{what}: invalid seat name {:?} (must match [a-z0-9][a-z0-9._-]{{0,62}})",
                s.name
            ));
        }
        if RESERVED_LISTS.contains(&s.name.as_str()) {
            return err(format!(
                "{what}: the name {:?} is reserved for a generated list",
                s.name
            ));
        }
        if let Some(prev) = where_.get(&s.name) {
            return err(format!(
                "{what}: seat {:?} is already defined by {prev}",
                s.name
            ));
        }
        where_.insert(s.name.clone(), what.clone());
        let harness_raw = s
            .harness
            .clone()
            .or_else(|| inherit.harness.clone())
            .or_else(|| defaults.harness.clone())
            .ok_or_else(|| {
                TeamError(format!(
                    "{what}: no harness (set harness, or [defaults] harness)"
                ))
            })?;
        let harness = harness_name(&harness_raw).ok_or_else(|| {
            TeamError(format!(
                "{what}: unknown harness {harness_raw:?} (claude, codex or dsh)"
            ))
        })?;
        let pm = match s
            .permission_mode
            .clone()
            .or_else(|| inherit.permission_mode.clone())
            .or_else(|| defaults.permission_mode.clone())
        {
            None => None,
            Some(m) => Some(m.parse::<PermissionMode>().map_err(|_| {
                TeamError(format!(
                    "{what}: permission_mode {m:?} must be \"default\" or \"unattended\""
                ))
            })?),
        };
        let git_access = match s
            .git_access
            .clone()
            .or_else(|| inherit.git_access.clone())
            .or_else(|| defaults.git_access.clone())
        {
            None => None,
            Some(g) => Some(
                g.parse::<GitAccess>()
                    .map_err(|e| TeamError(format!("{what}: {e}")))?,
            ),
        };
        if s.role.is_some() && s.role_file.is_some() {
            return err(format!("{what}: give role or role_file, not both"));
        }
        let role =
            match (&s.role, &s.role_file) {
                (Some(r), _) => Some(r.clone()),
                (None, Some(f)) => {
                    let p = abs(base, f);
                    Some(std::fs::read_to_string(&p).map_err(|e| {
                        TeamError(format!("{what}: role_file {}: {e}", p.display()))
                    })?)
                }
                (None, None) => None,
            };
        let role = role.map(|r| r.trim().to_string()).filter(|r| !r.is_empty());
        seats.push(SeatPlan {
            name: s.name.clone(),
            parent,
            group,
            harness: harness.to_string(),
            model: s
                .model
                .clone()
                .or_else(|| inherit.model.clone())
                .or_else(|| defaults.model.clone()),
            permission_mode: pm,
            git_access,
            dsh_jail: s.dsh_jail.or(inherit.dsh_jail).or(defaults.dsh_jail),
            role,
            cwd: s.cwd.as_deref().map(|p| abs(base, p)),
            display_name: s.display_name.clone(),
            paste_fallback: s
                .paste_fallback
                .or(inherit.paste_fallback)
                .or(defaults.paste_fallback)
                .unwrap_or(false),
        });
        Ok(())
    };

    // Seats that join a group are added after the groups (their lead must
    // be known).
    let mut joining = Vec::new();
    for (i, s) in raw.seat.iter().enumerate() {
        let what = format!("[[seat]] #{} {:?}", i + 1, s.name);
        if s.group.is_some() {
            joining.push((what, s));
            continue;
        }
        add(
            what,
            s,
            s.parent.clone(),
            None,
            &SeatOpts::default(),
            &mut seats,
        )?;
    }
    let mut group_names = BTreeSet::new();
    for (i, g) in raw.group.iter().enumerate() {
        let what = format!("[[group]] #{} {:?}", i + 1, g.name);
        if !valid_name(&g.name) {
            return err(format!(
                "{what}: invalid group name (must match [a-z0-9][a-z0-9._-]{{0,62}})"
            ));
        }
        if RESERVED_LISTS.contains(&g.name.as_str()) {
            return err(format!(
                "{what}: the name {:?} is reserved for a generated list",
                g.name
            ));
        }
        if !group_names.insert(g.name.clone()) {
            return err(format!("{what}: group {:?} is defined twice", g.name));
        }
        if g.lead.parent.is_some() {
            return err(format!(
                "{what}: set the group's parent, not the lead's (lead.parent)"
            ));
        }
        if g.lead.group.is_some() {
            return err(format!(
                "{what}: lead.group is not allowed (the lead is in this group)"
            ));
        }
        add(
            format!("{what} lead"),
            &g.lead,
            g.parent.clone(),
            Some(g.name.clone()),
            &SeatOpts::default(),
            &mut seats,
        )?;
        let mut workers = Vec::new();
        match &g.workers {
            None => {}
            Some(toml::Value::Table(t)) => {
                let w: WorkersRaw = toml::Value::Table(t.clone())
                    .try_into()
                    .map_err(|e| TeamError(format!("{what} workers: {e}")))?;
                if w.count > MAX_WORKERS {
                    return err(format!(
                        "{what}: workers.count {} is more than {MAX_WORKERS}",
                        w.count
                    ));
                }
                let prefix = w.prefix.clone().unwrap_or_else(|| g.name.clone());
                let inherit = SeatOpts {
                    harness: w.harness.clone(),
                    model: w.model.clone(),
                    permission_mode: w.permission_mode.clone(),
                    git_access: w.git_access.clone(),
                    dsh_jail: w.dsh_jail,
                    paste_fallback: None,
                };
                for n in 1..=w.count {
                    let s = SeatRaw {
                        name: format!("{prefix}-{n}"),
                        role: w.role.clone(),
                        role_file: w.role_file.clone(),
                        ..Default::default()
                    };
                    add(
                        format!("{what} worker {:?}", s.name),
                        &s,
                        Some(g.lead.name.clone()),
                        Some(g.name.clone()),
                        &inherit,
                        &mut seats,
                    )?;
                    workers.push(s.name);
                }
            }
            Some(toml::Value::Array(items)) => {
                if items.len() > MAX_WORKERS as usize {
                    return err(format!("{what}: more than {MAX_WORKERS} workers"));
                }
                for (j, item) in items.iter().enumerate() {
                    let s: SeatRaw = item
                        .clone()
                        .try_into()
                        .map_err(|e| TeamError(format!("{what} workers[{j}]: {e}")))?;
                    let wwhat = format!("{what} workers[{j}] {:?}", s.name);
                    if s.group.is_some() {
                        return err(format!("{wwhat}: group is not allowed here"));
                    }
                    let parent = s.parent.clone().unwrap_or_else(|| g.lead.name.clone());
                    add(
                        wwhat,
                        &s,
                        Some(parent),
                        Some(g.name.clone()),
                        &SeatOpts::default(),
                        &mut seats,
                    )?;
                    workers.push(s.name);
                }
            }
            Some(_) => {
                return err(format!(
                    "{what}: workers must be {{ count = N, ... }} or an array of seat tables"
                ));
            }
        }
        groups.push(GroupPlan {
            name: g.name.clone(),
            lead: g.lead.name.clone(),
            workers,
        });
    }
    for (what, s) in joining {
        let gname = s.group.clone().expect("joining seats have a group");
        let Some(g) = groups.iter_mut().find(|g| g.name == gname) else {
            return err(format!("{what}: unknown group {gname:?}"));
        };
        let parent = s.parent.clone().unwrap_or_else(|| g.lead.clone());
        add(
            what,
            s,
            Some(parent),
            Some(gname),
            &SeatOpts::default(),
            &mut seats,
        )?;
        g.workers.push(s.name.clone());
    }
    for g in &groups {
        if where_.contains_key(&g.name) {
            return err(format!(
                "[[group]] {:?}: a seat has the same name (seats and lists share names)",
                g.name
            ));
        }
    }
    if seats.is_empty() {
        return err("the team file defines no seats ([[seat]] or [[group]])");
    }

    // Parents exist; no cycles; order parents first.
    let names: BTreeSet<&str> = seats.iter().map(|s| s.name.as_str()).collect();
    for s in &seats {
        if let Some(p) = &s.parent
            && !names.contains(p.as_str())
        {
            return err(format!(
                "{}: unknown parent {p:?}",
                where_.get(&s.name).cloned().unwrap_or_default()
            ));
        }
    }
    let parent_of: HashMap<&str, Option<&str>> = seats
        .iter()
        .map(|s| (s.name.as_str(), s.parent.as_deref()))
        .collect();
    for s in &seats {
        let mut cur = s.parent.as_deref();
        let mut steps = 0;
        while let Some(p) = cur {
            if p == s.name || steps > seats.len() {
                return err(format!(
                    "{}: its parent chain loops back to {:?}",
                    where_.get(&s.name).cloned().unwrap_or_default(),
                    s.name
                ));
            }
            cur = parent_of.get(p).copied().flatten();
            steps += 1;
        }
    }
    let mut ordered: Vec<SeatPlan> = Vec::with_capacity(seats.len());
    let mut placed: BTreeSet<String> = BTreeSet::new();
    while ordered.len() < seats.len() {
        for s in &seats {
            if placed.contains(&s.name) {
                continue;
            }
            if s.parent.as_ref().is_none_or(|p| placed.contains(p)) {
                placed.insert(s.name.clone());
                ordered.push(s.clone());
            }
        }
    }

    let team = Team {
        base: base.to_path_buf(),
        template,
        repo,
        workroot,
        settings,
        seats: ordered,
        groups,
    };
    team.check_workdirs()?;
    Ok(team)
}

impl Team {
    pub fn seat(&self, name: &str) -> Option<&SeatPlan> {
        self.seats.iter().find(|s| s.name == name)
    }

    /// Direct subordinates of `name`, in file order.
    pub fn children(&self, name: &str) -> Vec<&str> {
        self.seats
            .iter()
            .filter(|s| s.parent.as_deref() == Some(name))
            .map(|s| s.name.as_str())
            .collect()
    }

    pub fn position(&self, name: &str) -> Position {
        match self.seat(name).and_then(|s| s.parent.as_ref()) {
            None => Position::Root,
            Some(_) if !self.children(name).is_empty() => Position::Lead,
            Some(_) => Position::Worker,
        }
    }

    /// Whether working directories are git worktrees.
    pub fn uses_worktrees(&self) -> bool {
        match &self.template {
            Template::StagedMerge => true,
            Template::Custom(dir) => crate::template::custom_workdir(dir) == "worktree",
            Template::None => false,
        }
    }

    /// The seat's git access: the file's `git_access`, else `full` for the
    /// root of a staged-merge team that has subordinates (it merges into
    /// the main branch in the main checkout), else `worktree`.
    pub fn git_access(&self, name: &str) -> GitAccess {
        if let Some(g) = self.seat(name).and_then(|s| s.git_access) {
            return g;
        }
        if self.template == Template::StagedMerge
            && self.position(name) == Position::Root
            && !self.children(name).is_empty()
        {
            return GitAccess::Full;
        }
        GitAccess::Worktree
    }

    /// The seat's working directory.
    pub fn workdir(&self, name: &str) -> PathBuf {
        let s = self.seat(name);
        if self.uses_worktrees()
            && let Some(w) = &self.workroot
        {
            return w.join(name);
        }
        if let Some(c) = s.and_then(|s| s.cwd.clone()) {
            return c;
        }
        match &self.workroot {
            Some(w) => w.join(name),
            None => self.base.clone(),
        }
    }

    /// The seat's own branch (worktree templates).
    pub fn branch(&self, name: &str) -> String {
        format!("amail/{name}")
    }

    /// Where the seat merges its subordinates' work: its own integration
    /// branch; for a root, the repository's main branch (`main_branch`).
    pub fn integration_branch(&self, name: &str, main_branch: &str) -> String {
        match self.position(name) {
            Position::Root => main_branch.to_string(),
            _ => format!("amail/{name}-next"),
        }
    }

    /// Seats whose integration branch is created by `amail up`.
    pub fn needs_integration_branch(&self, name: &str) -> bool {
        self.position(name) == Position::Lead
    }

    fn check_workdirs(&self) -> Result<(), TeamError> {
        if self.uses_worktrees() {
            let what = match &self.template {
                Template::StagedMerge => "the staged-merge template".to_string(),
                Template::Custom(d) => format!("template {}", d.display()),
                Template::None => unreachable!(),
            };
            if self.repo.is_none() {
                return err(format!("[team] repo is required by {what}"));
            }
            if self.workroot.is_none() {
                return err(format!("[team] workroot is required by {what}"));
            }
            // `amail/<x>-next` must not collide with another seat's branch.
            for s in &self.seats {
                if self.needs_integration_branch(&s.name) {
                    let clash = format!("{}-next", s.name);
                    if self.seat(&clash).is_some() {
                        return err(format!(
                            "seat {clash:?}: its branch would collide with the integration \
                             branch of {:?} (amail/{clash})",
                            s.name
                        ));
                    }
                }
            }
            return Ok(());
        }
        for s in &self.seats {
            if s.cwd.is_none() && self.workroot.is_none() {
                return err(format!(
                    "seat {:?}: no working directory (set cwd, or [team] workroot)",
                    s.name
                ));
            }
        }
        Ok(())
    }

    /// The generated lists: `(name, members, post_policy_user_only)`. One
    /// per group (lead and workers), `leads` (every seat with
    /// subordinates) and `all` (every seat; only the user may post).
    pub fn lists(&self) -> Vec<(String, Vec<String>, bool)> {
        let mut out = Vec::new();
        for g in &self.groups {
            let mut m = vec![g.lead.clone()];
            m.extend(g.workers.iter().cloned());
            out.push((g.name.clone(), m, false));
        }
        let leads: Vec<String> = self
            .seats
            .iter()
            .filter(|s| !self.children(&s.name).is_empty())
            .map(|s| s.name.clone())
            .collect();
        if !leads.is_empty() {
            out.push(("leads".into(), leads, false));
        }
        out.push((
            "all".into(),
            self.seats.iter().map(|s| s.name.clone()).collect(),
            true,
        ));
        out
    }

    /// The seats `--only` selects: a group's lead and workers, or one seat.
    pub fn select(&self, only: &[String]) -> Result<Vec<String>, TeamError> {
        if only.is_empty() {
            return Ok(self.seats.iter().map(|s| s.name.clone()).collect());
        }
        let mut out = BTreeSet::new();
        for o in only {
            if let Some(g) = self.groups.iter().find(|g| &g.name == o) {
                out.insert(g.lead.clone());
                out.extend(g.workers.iter().cloned());
            } else if self.seat(o).is_some() {
                out.insert(o.clone());
            } else {
                return err(format!(
                    "--only {o:?}: no such group or seat in the team file"
                ));
            }
        }
        // Keep the team's order (parents first).
        Ok(self
            .seats
            .iter()
            .filter(|s| out.contains(&s.name))
            .map(|s| s.name.clone())
            .collect())
    }

    /// The zellij session suffix of a seat: its group, else `main`.
    pub fn session_group(&self, name: &str) -> String {
        self.seat(name)
            .and_then(|s| s.group.clone())
            .unwrap_or_else(|| "main".into())
    }

    /// `(seat, session group, tab)` for every seat: each session holds the
    /// seats of one group (ungrouped seats share `main`), one tab per seat,
    /// named after the seat (see [`crate::mux`] for why not several panes
    /// per tab).
    pub fn placements(&self) -> Vec<(String, String, String)> {
        self.seats
            .iter()
            .map(|s| (s.name.clone(), self.session_group(&s.name), s.name.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(text: &str) -> Result<Team, TeamError> {
        parse(text, Path::new("/teams"))
    }

    const EXAMPLE: &str = r#"
[team]
workroot = "/work"
idle_sleep = "30m"
max_live = 40

[defaults]
harness = "claude"
model = "sonnet"
permission_mode = "unattended"

[[seat]]
name = "root"
role = "Overall technical lead."

[[group]]
name = "backend"
parent = "root"
lead = { name = "be-lead", harness = "codex", model = "codex-model" }
workers = { count = 3, prefix = "be" }

[[group]]
name = "db"
parent = "be-lead"
lead = { name = "db-lead" }
workers = { count = 1, model = "opus", role = "Owns the schema." }

[[seat]]
name = "docs"
parent = "root"
cwd = "docs"
"#;

    #[test]
    fn expands_groups_defaults_and_nesting() {
        let t = team(EXAMPLE).unwrap();
        let names: Vec<&str> = t.seats.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "root", "docs", "be-lead", "be-1", "be-2", "be-3", "db-lead", "db-1"
            ]
        );
        let be = t.seat("be-lead").unwrap();
        assert_eq!(be.harness, "codex");
        assert_eq!(be.model.as_deref(), Some("codex-model"));
        assert_eq!(be.parent.as_deref(), Some("root"));
        assert_eq!(be.group.as_deref(), Some("backend"));
        assert_eq!(be.permission_mode, Some(PermissionMode::Unattended));
        let w = t.seat("be-2").unwrap();
        assert_eq!(
            (w.harness.as_str(), w.model.as_deref(), w.parent.as_deref()),
            ("claude", Some("sonnet"), Some("be-lead"))
        );
        let dbl = t.seat("db-lead").unwrap();
        assert_eq!(dbl.parent.as_deref(), Some("be-lead"));
        assert_eq!(dbl.group.as_deref(), Some("db"));
        let d1 = t.seat("db-1").unwrap();
        assert_eq!(d1.model.as_deref(), Some("opus"));
        assert_eq!(d1.role.as_deref(), Some("Owns the schema."));
        assert_eq!(
            t.seat("docs").unwrap().cwd,
            Some(PathBuf::from("/teams/docs"))
        );
        assert_eq!(t.seat("root").unwrap().group, None);
        assert_eq!(t.settings.idle_sleep_ms, Some(30 * 60 * 1000));
        assert_eq!(t.settings.max_live, Some(40));
        assert_eq!(t.settings.restart, Restart::OnFailure);
        assert_eq!(t.position("root"), Position::Root);
        assert_eq!(t.position("be-lead"), Position::Lead);
        assert_eq!(t.position("db-lead"), Position::Lead);
        assert_eq!(t.position("be-1"), Position::Worker);
        assert_eq!(
            t.children("be-lead"),
            vec!["be-1", "be-2", "be-3", "db-lead"]
        );
        assert_eq!(t.workdir("be-1"), PathBuf::from("/work/be-1"));
        assert_eq!(t.workdir("docs"), PathBuf::from("/teams/docs"));

        let lists = t.lists();
        assert_eq!(
            lists[0],
            (
                "backend".to_string(),
                vec![
                    "be-lead".into(),
                    "be-1".into(),
                    "be-2".into(),
                    "be-3".into()
                ],
                false
            )
        );
        assert_eq!(
            lists[1],
            (
                "db".to_string(),
                vec!["db-lead".into(), "db-1".into()],
                false
            )
        );
        assert_eq!(
            lists[2],
            (
                "leads".to_string(),
                vec!["root".into(), "be-lead".into(), "db-lead".into()],
                false
            )
        );
        assert_eq!(lists[3].0, "all");
        assert_eq!(lists[3].1.len(), 8);
        assert!(lists[3].2);

        assert_eq!(
            t.select(&["backend".into()]).unwrap(),
            vec!["be-lead", "be-1", "be-2", "be-3"]
        );
        assert_eq!(t.select(&["docs".into()]).unwrap(), vec!["docs"]);
        assert!(t.select(&["nope".into()]).is_err());

        let p = t.placements();
        assert!(p.contains(&("root".into(), "main".into(), "root".into())));
        assert!(p.contains(&("docs".into(), "main".into(), "docs".into())));
        assert!(p.contains(&("be-3".into(), "backend".into(), "be-3".into())));
        assert!(p.contains(&("db-1".into(), "db".into(), "db-1".into())));
    }

    #[test]
    fn local_policy_setting() {
        let base = "[team]\nworkroot = \"/w\"\n";
        let seat = "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n";
        let t = team(&format!("{base}{seat}")).unwrap();
        assert_eq!(t.settings.local_policy, None);
        let t = team(&format!("{base}local_policy = \"restricted\"\n{seat}")).unwrap();
        assert_eq!(
            t.settings.local_policy,
            Some(amail_proto::LocalPolicy::Restricted)
        );
        let e = team(&format!("{base}local_policy = \"loose\"\n{seat}")).unwrap_err();
        assert!(e.to_string().contains("local_policy"), "{e}");
    }

    #[test]
    fn mixed_groups_two_ways() {
        let t = team(
            r#"
[team]
workroot = "/w"
[defaults]
harness = "claude"
model = "sonnet"
[[seat]]
name = "root"
[[group]]
name = "core"
parent = "root"
lead = { name = "core-lead" }
workers = [
  { name = "core-1" },
  { name = "core-2", harness = "codex", model = "codex-model" },
]
[[group]]
name = "app"
parent = "root"
lead = { name = "app-lead", harness = "codex" }
[[seat]]
name = "app-1"
group = "app"
[[seat]]
name = "app-2"
group = "app"
harness = "codex"
"#,
        )
        .unwrap();
        let c2 = t.seat("core-2").unwrap();
        assert_eq!(
            (
                c2.harness.as_str(),
                c2.parent.as_deref(),
                c2.group.as_deref()
            ),
            ("codex", Some("core-lead"), Some("core"))
        );
        assert_eq!(t.seat("core-1").unwrap().harness, "claude");
        let a2 = t.seat("app-2").unwrap();
        assert_eq!(
            (
                a2.harness.as_str(),
                a2.parent.as_deref(),
                a2.group.as_deref()
            ),
            ("codex", Some("app-lead"), Some("app"))
        );
        let lists = t.lists();
        let members = |n: &str| lists.iter().find(|l| l.0 == n).unwrap().1.clone();
        assert_eq!(members("core"), vec!["core-lead", "core-1", "core-2"]);
        assert_eq!(members("app"), vec!["app-lead", "app-1", "app-2"]);
        assert_eq!(t.session_group("app-1"), "app");
        assert_eq!(t.session_group("root"), "main");
        // Parents come first.
        let pos = |n: &str| t.seats.iter().position(|s| s.name == n).unwrap();
        assert!(pos("app-lead") < pos("app-1"));
        assert!(pos("root") < pos("core-lead"));
    }

    #[test]
    fn errors_point_at_the_entry() {
        let cases: &[(&str, &str)] = &[
            (
                "[[seat]]\nname = \"a\"\nharness = \"claude\"\nparent = \"rot\"\ncwd = \"/x\"\n",
                "[[seat]] #1 \"a\": unknown parent \"rot\"",
            ),
            (
                "[[seat]]\nname = \"a\"\ncwd = \"/x\"\n",
                "[[seat]] #1 \"a\": no harness",
            ),
            (
                "[[seat]]\nname = \"a\"\nharness = \"gpt\"\ncwd = \"/x\"\n",
                "unknown harness \"gpt\"",
            ),
            (
                "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\n",
                "[[seat]] #2 \"a\": seat \"a\" is already defined by [[seat]] #1 \"a\"",
            ),
            (
                "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"Bad Name\"\ncwd = \"/x\"\n",
                "invalid seat name",
            ),
            (
                "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"all\"\ncwd = \"/x\"\n",
                "reserved",
            ),
            (
                "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\nparent = \"b\"\ncwd = \"/x\"\n\
                 [[seat]]\nname = \"b\"\nparent = \"a\"\ncwd = \"/x\"\n",
                "loops back",
            ),
            (
                "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n",
                "seat \"a\": no working directory",
            ),
            (
                "[team]\ntemplate = \"staged-merge\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n",
                "[team] repo is required",
            ),
            ("[team]\nidle_sleep = \"30 minutes\"\n", "[team] idle_sleep"),
            (
                "[team]\nrestart = \"always\"\n",
                "[team] restart \"always\"",
            ),
            (
                "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[group]]\nname = \"g\"\n\
                 lead = { name = \"g\" }\n",
                "a seat has the same name",
            ),
            (
                "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[group]]\nname = \"g\"\n\
                 lead = { name = \"l\", parent = \"x\" }\n",
                "set the group's parent",
            ),
            (
                "[[seat]]\nname = \"a\"\nharnes = \"claude\"\n",
                "unknown field `harnes`",
            ),
            (
                "[team]\ntemplate = \"staged-merge\"\nrepo = \"/r\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n\
                 [[seat]]\nname = \"a\"\n[[seat]]\nname = \"b\"\nparent = \"a\"\n[[seat]]\nname = \"c\"\nparent = \"b\"\n\
                 [[seat]]\nname = \"b-next\"\nparent = \"a\"\n",
                "collide with the integration branch of \"b\"",
            ),
            (
                "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\ngroup = \"nope\"\n",
                "[[seat]] #1 \"a\": unknown group \"nope\"",
            ),
            (
                "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[group]]\nname = \"g\"\n\
                 lead = { name = \"l\" }\nworkers = [{ name = \"w\", harnes = \"x\" }]\n",
                "[[group]] #1 \"g\" workers[0]: unknown field `harnes`",
            ),
            (
                "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[group]]\nname = \"g\"\n\
                 lead = { name = \"l\" }\nworkers = 3\n",
                "workers must be",
            ),
            ("[team]\n", "defines no seats"),
        ];
        for (text, want) in cases {
            let e = team(text).expect_err(text).0;
            assert!(e.contains(want), "{text:?}: got {e:?}, want {want:?}");
        }
    }

    #[test]
    fn dsh_jail_setting() {
        let t = team(
            "[defaults]\nharness = \"dsh\"\ndsh_jail = false\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\n\
             [[seat]]\nname = \"b\"\ncwd = \"/y\"\ndsh_jail = true\n[[seat]]\nname = \"c\"\ncwd = \"/z\"\n",
        )
        .unwrap();
        assert_eq!(t.seat("a").unwrap().dsh_jail, Some(false));
        assert_eq!(t.seat("b").unwrap().dsh_jail, Some(true));
        assert_eq!(t.seat("c").unwrap().dsh_jail, Some(false));
        let t =
            team("[defaults]\nharness = \"dsh\"\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\n").unwrap();
        assert_eq!(t.seat("a").unwrap().dsh_jail, None);
    }

    #[test]
    fn git_access_defaults_and_overrides() {
        let t = team(EXAMPLE).unwrap();
        // No staged-merge template: everyone gets the default.
        assert!(
            t.seats
                .iter()
                .all(|s| t.git_access(&s.name) == GitAccess::Worktree)
        );
        let t = team(
            r#"
[team]
template = "staged-merge"
repo = "/repo"
workroot = "/wt"

[defaults]
harness = "codex"

[[seat]]
name = "root"

[[group]]
name = "g"
parent = "root"
lead = { name = "lead" }
workers = [{ name = "w1" }, { name = "w2", git_access = "off" }]
"#,
        )
        .unwrap();
        // The staged-merge root merges in the main checkout.
        assert_eq!(t.git_access("root"), GitAccess::Full);
        assert_eq!(t.git_access("lead"), GitAccess::Worktree);
        assert_eq!(t.git_access("w1"), GitAccess::Worktree);
        assert_eq!(t.git_access("w2"), GitAccess::Off);
        let t = team(
            r#"
[team]
template = "staged-merge"
repo = "/repo"
workroot = "/wt"

[defaults]
harness = "codex"
git_access = "worktree"

[[seat]]
name = "root"

[[seat]]
name = "w"
parent = "root"
"#,
        )
        .unwrap();
        // An explicit setting (here from [defaults]) wins.
        assert_eq!(t.git_access("root"), GitAccess::Worktree);
        let e =
            team("[defaults]\nharness = \"codex\"\ngit_access = \"all\"\n[[seat]]\nname = \"a\"\n")
                .unwrap_err();
        assert!(e.0.contains("git_access"), "{e:?}");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("off").unwrap(), None);
        assert_eq!(
            parse_duration("30m").unwrap(),
            Some(Duration::from_secs(1800))
        );
        assert_eq!(
            parse_duration("1h30m").unwrap(),
            Some(Duration::from_secs(5400))
        );
        assert_eq!(
            parse_duration("45s").unwrap(),
            Some(Duration::from_secs(45))
        );
        assert!(parse_duration("30").is_err());
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("3x").is_err());
    }

    #[test]
    fn role_file_is_read_relative_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("roles")).unwrap();
        std::fs::write(dir.path().join("roles/docs.md"), "Writes the docs.\n").unwrap();
        let path = dir.path().join("amail.toml");
        std::fs::write(
            &path,
            "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"docs\"\ncwd = \".\"\nrole_file = \"roles/docs.md\"\n",
        )
        .unwrap();
        let t = load(&path).unwrap();
        let s = t.seat("docs").unwrap();
        assert_eq!(s.role.as_deref(), Some("Writes the docs."));
        assert_eq!(
            s.cwd.as_deref().map(|p| p.canonicalize().unwrap()),
            Some(dir.path().canonicalize().unwrap())
        );
        std::fs::write(
            &path,
            "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"docs\"\ncwd = \".\"\nrole_file = \"roles/nope.md\"\n",
        )
        .unwrap();
        assert!(load(&path).unwrap_err().0.contains("role_file"));
    }
}
