//! Team files (`amail.toml`): a declarative seat tree plus how this host
//! runs it. Parsing, validation and expansion are pure: nothing here touches
//! the server, git or processes.
//!
//! ```toml
//! [team]
//! name = "platform"             # optional: the team this file describes
//! description = "Builds the platform."
//! template = "staged-merge"     # built-in template, or a template directory
//! repo = "/path/to/repo"
//! workroot = "/path/to/worktrees"
//! idle_sleep = "30m"            # or "off" (default)
//! handover_idle = "30m"         # ask an idle seat with work for its note (default 30m; "off")
//! docs.activate_by = "human"    # or "owner" (default: each kind decides)
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
//! handover = "private"          # its notes: the seat, its lead, the user
//!
//! [[group]]
//! name = "backend"
//! parent = "root"
//! lead = { name = "be-lead", harness = "codex", model = "codex-model" }
//! workers = { count = 3, prefix = "be" }   # be-1 .. be-3, reporting to be-lead
//! description = "The backend: API and storage."
//! open = "linked"               # or ["orgname", ...]; opens the group's team
//! contacts = ["be-lead"]        # default for an open group: its lead
//! ```
//!
//! Teams: with `[team] name`, the file is one team; its `[[group]]`s are
//! sub-teams of it (nested as their parent seats are) and seats outside any
//! group are its members. Without a name, each `[[group]]` is a team, a
//! sub-team of the team of its parent seat when that seat is in a group.

use crate::wire::GitAccess;
use amail_proto::{PermissionMode, valid_name};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    /// The team this file describes (see the module documentation).
    name: Option<String>,
    description: Option<String>,
    open: Option<toml::Value>,
    contacts: Option<Vec<String>>,
    template: Option<String>,
    repo: Option<PathBuf>,
    workroot: Option<PathBuf>,
    idle_sleep: Option<String>,
    handover_idle: Option<String>,
    max_live: Option<u32>,
    restart: Option<String>,
    local_policy: Option<String>,
    docs: Option<DocsRaw>,
}

/// `docs.activate_by` of `[team]` or a `[[group]]`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocsRaw {
    activate_by: Option<String>,
}

/// Who activates the documents of a team's seats (`docs.activate_by`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocActivation {
    /// The document's kind decides (the server's `doc_activate_by: null`).
    #[default]
    Kind,
    /// Every document waits for the user (`doc_activate_by: "human"`).
    Human,
}

fn doc_activation(what: &str, d: Option<&DocsRaw>) -> Result<Option<DocActivation>, TeamError> {
    match d.and_then(|d| d.activate_by.as_deref()).map(str::trim) {
        None => Ok(None),
        Some("human") => Ok(Some(DocActivation::Human)),
        Some("owner") | Some("kind") => Ok(Some(DocActivation::Kind)),
        Some(other) => err(format!(
            "{what} docs.activate_by {other:?}: expected \"human\" or \"owner\""
        )),
    }
}

/// `handover` of a seat: `private` limits its notes to the seat, its lead
/// and the user; `org` (the default) lets every seat of the org read them.
fn handover_private(what: &str, v: Option<&String>) -> Result<Option<bool>, TeamError> {
    match v.map(|s| s.trim()) {
        None => Ok(None),
        Some("private") => Ok(Some(true)),
        Some("org") => Ok(Some(false)),
        Some(other) => err(format!(
            "{what}: handover {other:?}: expected \"private\" or \"org\""
        )),
    }
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
    /// `private` or `org`: who reads the seat's handover notes.
    handover: Option<String>,
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
    /// What the seat does, for others to find it.
    description: Option<String>,
    /// `"linked"` or `["orgname", ...]`: open the seat to these orgs.
    open: Option<toml::Value>,
    /// `[[seat]]` only: join this group (as a worker of its lead, unless
    /// `parent` says otherwise).
    group: Option<String>,
    /// An existing harness session the seat adopts (resumes).
    session: Option<String>,
    /// `private` or `org`: who reads the seat's handover notes.
    handover: Option<String>,
}

/// A session id is used in file names: letters, digits and `-_.:` only,
/// not starting with a dot, at most 128 characters.
pub fn check_session_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "session {id:?} is not a session id (letters, digits and -_.: only, at most 128 \
             characters)"
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupRaw {
    name: String,
    parent: Option<String>,
    description: Option<String>,
    open: Option<toml::Value>,
    contacts: Option<Vec<String>>,
    lead: SeatRaw,
    /// `{ count, prefix, ... }`, or an array of seat tables.
    workers: Option<toml::Value>,
    docs: Option<DocsRaw>,
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
    description: Option<String>,
    handover: Option<String>,
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
    /// Ask a seat idle this long with work in progress for its handover
    /// note (default: 30 minutes; 0: never).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover_idle_ms: Option<u64>,
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
            handover_idle_ms: None,
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
    /// The seat's team: its group, or (with `[team] name`) the file's team.
    pub group: Option<String>,
    pub description: Option<String>,
    /// Audiences the file opens the seat to: `linked` or org names.
    pub open: Vec<String>,
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
    /// An existing session of the seat's harness that the seat adopts: the
    /// node resumes it (see `docs/teams.md`).
    pub session: Option<String>,
    /// `handover = "private"`: the seat's handover notes are read only by
    /// the seat, its lead and the user.
    pub handover_private: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupPlan {
    pub name: String,
    pub lead: String,
    pub workers: Vec<String>,
}

/// A team the file declares: the file's own (`[team] name`) or a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamPlan {
    pub name: String,
    pub description: Option<String>,
    /// The team's root seat: a group's lead; for the file's team, its only
    /// seat without a parent (if there is just one).
    pub root: Option<String>,
    /// The parent team.
    pub parent: Option<String>,
    /// Member seats, in file order (lead first).
    pub members: Vec<String>,
    /// The team's contacts; `None`: the file does not say (whatever the
    /// server has stays).
    pub contacts: Option<Vec<String>>,
    /// Audiences the file opens the team to: `linked` or org names.
    pub open: Vec<String>,
    /// `docs.activate_by` (the group's, else `[team]`'s; default: the
    /// kind decides).
    pub doc_activation: DocActivation,
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
    /// `[team] name`: the team this file describes.
    pub name: Option<String>,
    /// The teams the file declares, parents first.
    pub teams: Vec<TeamPlan>,
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

/// A description from the file: trimmed, `None` when empty, checked like
/// the server checks it.
fn description(what: &str, d: Option<&String>) -> Result<Option<String>, TeamError> {
    let Some(d) = d.map(|d| d.trim()).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    amail_proto::check_description(d).map_err(|e| TeamError(format!("{what}: {e}")))?;
    Ok(Some(d.to_string()))
}

/// `open = "linked"` or `open = ["orgname", ...]` (`"linked"` may be in the
/// list): the audiences, sorted, without duplicates.
fn open_list(what: &str, v: Option<&toml::Value>) -> Result<Vec<String>, TeamError> {
    let bad = |x: &str| {
        TeamError(format!(
            "{what}: open {x}: expected \"linked\" (every linked org) or a list of org names, \
             e.g. open = [\"other-org\"]"
        ))
    };
    let items: Vec<String> = match v {
        None => return Ok(Vec::new()),
        Some(toml::Value::String(s)) if s.trim() == "linked" => vec!["linked".into()],
        Some(toml::Value::String(s)) => return Err(bad(&format!("{s:?}"))),
        Some(toml::Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                toml::Value::String(s) => Ok(s.trim().to_string()),
                other => Err(bad(&other.to_string())),
            })
            .collect::<Result<_, _>>()?,
        Some(other) => return Err(bad(&other.to_string())),
    };
    let mut out = BTreeSet::new();
    for a in items {
        if a != "linked" && !valid_name(&a) {
            return Err(TeamError(format!(
                "{what}: open: {a:?} is not an org name (must match [a-z0-9][a-z0-9._-]{{0,62}})"
            )));
        }
        out.insert(a);
    }
    Ok(out.into_iter().collect())
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
    let handover_idle = match &t.handover_idle {
        Some(s) => {
            match parse_duration(s).map_err(|e| TeamError(format!("[team] handover_idle: {e}")))? {
                Some(d) => Some(d),
                // `off`: idle seats are not asked.
                None => Some(Duration::ZERO),
            }
        }
        None => None,
    };
    doc_activation("[team]", t.docs.as_ref())?;
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
        handover_idle_ms: handover_idle.map(|d| d.as_millis() as u64),
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
        let desc = description(&what, s.description.as_ref())?;
        let open = open_list(&what, s.open.as_ref())?;
        if let Some(id) = &s.session {
            check_session_id(id).map_err(|e| TeamError(format!("{what}: {e}")))?;
        }
        let private = match handover_private(&what, s.handover.as_ref())? {
            Some(p) => p,
            None => handover_private(&what, inherit.handover.as_ref())?
                .or(handover_private("[defaults]", defaults.handover.as_ref())?)
                .unwrap_or(false),
        };
        seats.push(SeatPlan {
            name: s.name.clone(),
            parent,
            group,
            description: desc,
            open,
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
            session: s.session.clone(),
            handover_private: private,
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
                    handover: w.handover.clone(),
                };
                for n in 1..=w.count {
                    let s = SeatRaw {
                        name: format!("{prefix}-{n}"),
                        role: w.role.clone(),
                        role_file: w.role_file.clone(),
                        description: w.description.clone(),
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
    let mut sessions: BTreeMap<&str, &str> = BTreeMap::new();
    for s in &seats {
        if let Some(id) = s.session.as_deref()
            && let Some(first) = sessions.insert(id, &s.name)
        {
            return err(format!(
                "seats {first:?} and {:?} both have session {id:?}; a session can be the \
                 session of one seat only",
                s.name
            ));
        }
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

    let (teams, name) = teams_of(&raw, &mut ordered, &groups)?;
    let team = Team {
        base: base.to_path_buf(),
        template,
        repo,
        workroot,
        settings,
        seats: ordered,
        groups,
        name,
        teams,
    };
    team.check_workdirs()?;
    Ok(team)
}

/// A team's contacts as the file gives them: checked against its members;
/// an open team without `contacts` gets `default` (its lead or root).
fn contacts_of(
    what: &str,
    given: Option<&Vec<String>>,
    members: &[String],
    open: &[String],
    default: Option<&String>,
) -> Result<Option<Vec<String>>, TeamError> {
    match given {
        Some(cs) => {
            let mut out: Vec<String> = Vec::new();
            for c in cs {
                if !members.contains(c) {
                    return err(format!(
                        "{what}: contact {c:?} is not a member of the team (members: {})",
                        members.join(", ")
                    ));
                }
                if !out.contains(c) {
                    out.push(c.clone());
                }
            }
            if out.is_empty() && !open.is_empty() {
                return err(format!(
                    "{what}: an open team needs at least one contact (contacts = [...])"
                ));
            }
            Ok(Some(out))
        }
        None if open.is_empty() => Ok(None),
        None => match default {
            Some(d) => Ok(Some(vec![d.clone()])),
            None => err(format!(
                "{what}: an open team needs contacts: name them with contacts = [...]"
            )),
        },
    }
}

/// The teams of a file (parents first) and its own team's name; sets the
/// team of seats outside any group to the file's team.
fn teams_of(
    raw: &FileRaw,
    seats: &mut [SeatPlan],
    groups: &[GroupPlan],
) -> Result<(Vec<TeamPlan>, Option<String>), TeamError> {
    let t = &raw.team;
    let own = t.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let mut teams: Vec<TeamPlan> = Vec::new();
    match own {
        None => {
            for (field, set) in [
                ("description", t.description.is_some()),
                ("open", t.open.is_some()),
                ("contacts", t.contacts.is_some()),
            ] {
                if set {
                    return err(format!(
                        "[team] {field}: give the file's team a name first ([team] name = \"...\")"
                    ));
                }
            }
        }
        Some(n) => {
            if !valid_name(n) {
                return err(format!(
                    "[team] name {n:?}: invalid team name (must match [a-z0-9][a-z0-9._-]{{0,62}})"
                ));
            }
            if RESERVED_LISTS.contains(&n) {
                return err(format!(
                    "[team] name {n:?}: the name is reserved for a generated list"
                ));
            }
            if seats.iter().any(|s| s.name == n) {
                return err(format!(
                    "[team] name {n:?}: a seat has the same name (seats and teams share names)"
                ));
            }
            if groups.iter().any(|g| g.name == n) {
                return err(format!("[team] name {n:?}: a [[group]] has the same name"));
            }
            for s in seats.iter_mut().filter(|s| s.group.is_none()) {
                s.group = Some(n.to_string());
            }
            let members: Vec<String> = seats
                .iter()
                .filter(|s| s.group.as_deref() == Some(n))
                .map(|s| s.name.clone())
                .collect();
            let tops: Vec<&SeatPlan> = seats.iter().filter(|s| s.parent.is_none()).collect();
            let root = match tops.as_slice() {
                [one] if members.contains(&one.name) => Some(one.name.clone()),
                _ => None,
            };
            let open = open_list("[team]", t.open.as_ref())?;
            let contacts = contacts_of(
                "[team]",
                t.contacts.as_ref(),
                &members,
                &open,
                root.as_ref(),
            )?;
            teams.push(TeamPlan {
                name: n.to_string(),
                description: description("[team]", t.description.as_ref())?,
                root,
                parent: None,
                members,
                contacts,
                open,
                doc_activation: doc_activation("[team]", t.docs.as_ref())?.unwrap_or_default(),
            });
        }
    }
    let team_of: HashMap<String, Option<String>> = seats
        .iter()
        .map(|s| (s.name.clone(), s.group.clone()))
        .collect();
    for (i, g) in raw.group.iter().enumerate() {
        let what = format!("[[group]] #{} {:?}", i + 1, g.name);
        let plan = groups
            .iter()
            .find(|x| x.name == g.name)
            .expect("every group was expanded");
        let mut members = vec![plan.lead.clone()];
        members.extend(plan.workers.iter().cloned());
        let parent = match &g.parent {
            Some(p) => team_of.get(p).cloned().flatten(),
            None => own.map(str::to_string),
        };
        let open = open_list(&what, g.open.as_ref())?;
        let contacts = contacts_of(
            &what,
            g.contacts.as_ref(),
            &members,
            &open,
            Some(&plan.lead),
        )?;
        let doc_activation = match doc_activation(&what, g.docs.as_ref())? {
            Some(a) => a,
            None => doc_activation("[team]", t.docs.as_ref())?.unwrap_or_default(),
        };
        teams.push(TeamPlan {
            name: g.name.clone(),
            description: description(&what, g.description.as_ref())?,
            root: Some(plan.lead.clone()),
            parent,
            members,
            contacts,
            open,
            doc_activation,
        });
    }
    // Team parents follow seat parents across groups, which can loop even
    // when the seats do not (a worker with its own `parent`).
    let parent_of: HashMap<&str, Option<&str>> = teams
        .iter()
        .map(|t| (t.name.as_str(), t.parent.as_deref()))
        .collect();
    for t in &teams {
        let mut cur = t.parent.as_deref();
        let mut steps = 0;
        while let Some(p) = cur {
            if p == t.name || steps > teams.len() {
                return err(format!(
                    "[[group]] {:?}: its parent seat puts the team inside itself (team \
                     {p:?} is both above and below it)",
                    t.name
                ));
            }
            cur = parent_of.get(p).copied().flatten();
            steps += 1;
        }
    }
    let mut ordered: Vec<TeamPlan> = Vec::with_capacity(teams.len());
    while ordered.len() < teams.len() {
        for t in &teams {
            if ordered.iter().any(|o| o.name == t.name) {
                continue;
            }
            if t.parent
                .as_ref()
                .is_none_or(|p| ordered.iter().any(|o| &o.name == p))
            {
                ordered.push(t.clone());
            }
        }
    }
    Ok((ordered, own.map(str::to_string)))
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

    /// The member list of each team (named after it): `(name, members,
    /// false)`. A team without members of its own has none.
    pub fn team_lists(&self) -> Vec<(String, Vec<String>, bool)> {
        self.teams
            .iter()
            .filter(|t| !t.members.is_empty())
            .map(|t| (t.name.clone(), t.members.clone(), false))
            .collect()
    }

    /// The generated lists: `(name, members, post_policy_user_only)`. One
    /// per team ([`Team::team_lists`]), `leads` (every seat with
    /// subordinates) and `all` (every seat; only the user may post).
    pub fn lists(&self) -> Vec<(String, Vec<String>, bool)> {
        let mut out = self.team_lists();
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

    /// The seats `--only` selects: a group's lead and workers, the members
    /// of the file's team, or one seat.
    pub fn select(&self, only: &[String]) -> Result<Vec<String>, TeamError> {
        if only.is_empty() {
            return Ok(self.seats.iter().map(|s| s.name.clone()).collect());
        }
        let mut out = BTreeSet::new();
        for o in only {
            if let Some(g) = self.groups.iter().find(|g| &g.name == o) {
                out.insert(g.lead.clone());
                out.extend(g.workers.iter().cloned());
            } else if let Some(t) = self.teams.iter().find(|t| &t.name == o) {
                out.extend(t.members.iter().cloned());
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

    /// The zellij session suffix of a seat: its team, else `main`.
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
    fn document_and_handover_settings() {
        let t = team(
            r#"
[team]
name = "plat"
workroot = "/w"
handover_idle = "10m"
docs.activate_by = "human"
[defaults]
harness = "claude"
handover = "private"
[[seat]]
name = "root"
[[seat]]
name = "open"
parent = "root"
handover = "org"
[[group]]
name = "api"
parent = "root"
lead = { name = "api-lead" }
docs.activate_by = "owner"
workers = { count = 1, prefix = "w", handover = "org" }
[[group]]
name = "ops"
parent = "root"
lead = { name = "ops-lead" }
"#,
        )
        .unwrap();
        assert_eq!(t.settings.handover_idle_ms, Some(600_000));
        let private = |n: &str| t.seat(n).unwrap().handover_private;
        assert!(private("root") && private("api-lead"));
        assert!(!private("open") && !private("w-1"));
        let act = |n: &str| t.teams.iter().find(|x| x.name == n).unwrap().doc_activation;
        assert_eq!(act("plat"), DocActivation::Human);
        assert_eq!(act("api"), DocActivation::Kind);
        assert_eq!(act("ops"), DocActivation::Human);
        let base = "[team]\nworkroot = \"/w\"\n";
        let seat = "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n";
        let t = team(&format!("{base}{seat}")).unwrap();
        assert_eq!(t.settings.handover_idle_ms, None);
        assert!(!t.seat("a").unwrap().handover_private);
        let t = team(&format!("{base}handover_idle = \"off\"\n{seat}")).unwrap();
        assert_eq!(t.settings.handover_idle_ms, Some(0));
        for (text, want) in [
            (
                format!("{base}docs.activate_by = \"bot\"\n{seat}"),
                "docs.activate_by",
            ),
            (format!("{base}docs.other = 1\n{seat}"), "other"),
            (format!("{base}{seat}handover = \"secret\"\n"), "handover"),
            (
                format!("{base}handover_idle = \"10x\"\n{seat}"),
                "handover_idle",
            ),
        ] {
            let e = team(&text).unwrap_err();
            assert!(e.to_string().contains(want), "{want}: {e}");
        }
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
    fn groups_become_teams_nested_as_written() {
        let t = team(EXAMPLE).unwrap();
        assert_eq!(t.name, None);
        let names: Vec<&str> = t.teams.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["backend", "db"]);
        let be = &t.teams[0];
        assert_eq!(be.parent, None);
        assert_eq!(be.root.as_deref(), Some("be-lead"));
        assert_eq!(be.members, vec!["be-lead", "be-1", "be-2", "be-3"]);
        assert_eq!(be.contacts, None);
        assert!(be.open.is_empty());
        // db's parent seat is in backend: a sub-team of it.
        assert_eq!(t.teams[1].parent.as_deref(), Some("backend"));
        // Seats outside groups are in no team.
        assert_eq!(t.seat("root").unwrap().group, None);
        assert_eq!(t.session_group("docs"), "main");
    }

    const NAMED: &str = r#"
[team]
name = "platform"
description = "Builds the platform."
workroot = "/w"
open = ["beta", "linked", "beta"]

[defaults]
harness = "claude"

[[seat]]
name = "root"
description = "  Runs the platform team.  "
open = "linked"

[[seat]]
name = "docs"
parent = "root"

[[group]]
name = "api"
parent = "root"
description = "The public API."
open = ["beta"]
lead = { name = "api-lead", description = "Leads the API." }
workers = { count = 2, prefix = "api", description = "Writes API code." }

[[group]]
name = "store"
parent = "api-1"
contacts = ["store-2"]
lead = { name = "store-lead" }
workers = [{ name = "store-1" }, { name = "store-2", description = "On call.", open = ["gamma"] }]
"#;

    #[test]
    fn a_named_file_is_one_team_with_sub_teams() {
        let t = team(NAMED).unwrap();
        assert_eq!(t.name.as_deref(), Some("platform"));
        let names: Vec<&str> = t.teams.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["platform", "api", "store"]);
        let pl = &t.teams[0];
        assert_eq!(pl.description.as_deref(), Some("Builds the platform."));
        assert_eq!(pl.parent, None);
        assert_eq!(pl.root.as_deref(), Some("root"));
        assert_eq!(pl.members, vec!["root", "docs"]);
        assert_eq!(pl.open, vec!["beta", "linked"]);
        // Open without contacts: its root.
        assert_eq!(pl.contacts, Some(vec!["root".to_string()]));
        let api = &t.teams[1];
        assert_eq!(api.parent.as_deref(), Some("platform"));
        assert_eq!(api.description.as_deref(), Some("The public API."));
        assert_eq!(api.open, vec!["beta"]);
        assert_eq!(api.contacts, Some(vec!["api-lead".to_string()]));
        // store's parent seat is a worker of api.
        let st = &t.teams[2];
        assert_eq!(st.parent.as_deref(), Some("api"));
        assert_eq!(st.contacts, Some(vec!["store-2".to_string()]));
        assert!(st.open.is_empty());
        // Seats: team, description, open.
        let root = t.seat("root").unwrap();
        assert_eq!(root.group.as_deref(), Some("platform"));
        assert_eq!(root.description.as_deref(), Some("Runs the platform team."));
        assert_eq!(root.open, vec!["linked"]);
        assert_eq!(t.seat("docs").unwrap().group.as_deref(), Some("platform"));
        assert_eq!(t.seat("docs").unwrap().description, None);
        assert_eq!(
            t.seat("api-lead").unwrap().description.as_deref(),
            Some("Leads the API.")
        );
        assert_eq!(
            t.seat("api-2").unwrap().description.as_deref(),
            Some("Writes API code.")
        );
        let s2 = t.seat("store-2").unwrap();
        assert_eq!(s2.description.as_deref(), Some("On call."));
        assert_eq!(s2.open, vec!["gamma"]);
        // Sessions and lists follow the teams.
        assert_eq!(t.session_group("docs"), "platform");
        let lists = t.team_lists();
        assert_eq!(
            lists[0],
            (
                "platform".to_string(),
                vec!["root".to_string(), "docs".to_string()],
                false
            )
        );
        assert_eq!(lists.len(), 3);
        assert_eq!(
            t.select(&["platform".into()]).unwrap(),
            vec!["root", "docs"]
        );
    }

    #[test]
    #[allow(clippy::useless_format)] // the cases read alike
    fn new_fields_are_checked() {
        let base = "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n";
        let long = "x".repeat(501);
        let cases: Vec<(String, &str)> = vec![
            (
                format!("{base}[[seat]]\nname = \"a\"\nopen = \"beta\"\n"),
                "[[seat]] #1 \"a\": open \"beta\": expected \"linked\"",
            ),
            (
                format!("{base}[[seat]]\nname = \"a\"\nopen = [\"Bad Org\"]\n"),
                "[[seat]] #1 \"a\": open: \"Bad Org\" is not an org name",
            ),
            (
                format!("{base}[[seat]]\nname = \"a\"\nopen = true\n"),
                "open true",
            ),
            (
                format!("{base}[[seat]]\nname = \"a\"\ndescription = \"{long}\"\n"),
                "[[seat]] #1 \"a\": description",
            ),
            (
                format!("{base}[[seat]]\nname = \"a\"\ndescription = \"one\\n\\ntwo\"\n"),
                "[[seat]] #1 \"a\": description",
            ),
            (
                format!(
                    "{base}[[group]]\nname = \"g\"\nlead = {{ name = \"l\" }}\ncontacts = [\"x\"]\n"
                ),
                "[[group]] #1 \"g\": contact \"x\" is not a member of the team",
            ),
            (
                format!(
                    "{base}[[group]]\nname = \"g\"\nlead = {{ name = \"l\" }}\nopen = \"linked\"\ncontacts = []\n"
                ),
                "[[group]] #1 \"g\": an open team needs at least one contact",
            ),
            (
                format!(
                    "{base}[[group]]\nname = \"g\"\nlead = {{ name = \"l\" }}\ndescription = \"{long}\"\n"
                ),
                "[[group]] #1 \"g\": description",
            ),
            (
                format!(
                    "[team]\ndescription = \"x\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n"
                ),
                "[team] description: give the file's team a name first",
            ),
            (
                format!(
                    "[team]\nopen = \"linked\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n"
                ),
                "[team] open: give the file's team a name first",
            ),
            (
                format!(
                    "[team]\nname = \"a\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n"
                ),
                "[team] name \"a\": a seat has the same name",
            ),
            (
                format!(
                    "[team]\nname = \"g\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[group]]\nname = \"g\"\nlead = {{ name = \"l\" }}\n"
                ),
                "[team] name \"g\": a [[group]] has the same name",
            ),
            (
                format!(
                    "[team]\nname = \"all\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n"
                ),
                "reserved",
            ),
            (
                format!(
                    "[team]\nname = \"Big Team\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n"
                ),
                "invalid team name",
            ),
            (
                // Two seats without a parent: no root to default the contacts to.
                format!(
                    "[team]\nname = \"t\"\nopen = \"linked\"\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n\
                         [[seat]]\nname = \"a\"\n[[seat]]\nname = \"b\"\n"
                ),
                "[team]: an open team needs contacts",
            ),
            (
                format!(
                    "[team]\nname = \"t\"\ncontacts = [\"x\"]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\n"
                ),
                "[team]: contact \"x\" is not a member",
            ),
            (
                // Team parents loop although seat parents do not.
                format!(
                    "{base}[[seat]]\nname = \"top\"\n\
                     [[group]]\nname = \"a\"\nparent = \"bw\"\nlead = {{ name = \"la\" }}\nworkers = [{{ name = \"aw\" }}]\n\
                     [[group]]\nname = \"b\"\nparent = \"aw\"\nlead = {{ name = \"lb\" }}\nworkers = [{{ name = \"bw\", parent = \"top\" }}]\n"
                ),
                "puts the team inside itself",
            ),
        ];
        for (text, want) in &cases {
            let e = team(text).expect_err(text).0;
            assert!(e.contains(want), "{text:?}: got {e:?}, want {want:?}");
        }
        // Valid: an empty list closes; contacts without open are kept.
        let t = team(&format!(
            "{base}[[group]]\nname = \"g\"\nopen = []\ncontacts = [\"w\"]\nlead = {{ name = \"l\" }}\nworkers = [{{ name = \"w\" }}]\n"
        ))
        .unwrap();
        assert!(t.teams[0].open.is_empty());
        assert_eq!(t.teams[0].contacts, Some(vec!["w".to_string()]));
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
    fn adopted_sessions() {
        let t = team(
            "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\nsession = \"0b6a1c9e-2f3d\"\n\
             [[group]]\nname = \"g\"\nparent = \"a\"\nlead = { name = \"l\", session = \"t-1\" }\n\
             workers = [{ name = \"w\", session = \"t-2\" }]\n",
        )
        .unwrap();
        assert_eq!(
            t.seat("a").unwrap().session.as_deref(),
            Some("0b6a1c9e-2f3d")
        );
        assert_eq!(t.seat("l").unwrap().session.as_deref(), Some("t-1"));
        assert_eq!(t.seat("w").unwrap().session.as_deref(), Some("t-2"));
        let e = team(
            "[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\nsession = \"s\"\n\
             [[seat]]\nname = \"b\"\ncwd = \"/y\"\nsession = \"s\"\n",
        )
        .unwrap_err();
        assert!(e.0.contains("both have session"), "{e}");
        let e = team("[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\ncwd = \"/x\"\nsession = \"../x\"\n").unwrap_err();
        assert!(e.0.contains("is not a session id"), "{e}");
    }

    #[test]
    fn session_ids() {
        for ok in [
            "0b6a1c9e-2f3d-4e5f-8a9b-0c1d2e3f4a5b",
            "session-1",
            "a:b.c_d",
        ] {
            assert!(check_session_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "a b", "../x", &"x".repeat(129)] {
            assert!(check_session_id(bad).is_err(), "{bad}");
        }
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
