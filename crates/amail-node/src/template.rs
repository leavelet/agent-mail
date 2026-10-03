//! Role texts generated from a seat's place in the team tree.
//!
//! The role text goes into the system prompt of every session start, so it
//! is kept short. The mail instructions and "who is your lead" line are
//! added separately at launch ([`crate::inject::seat_intro`]).
//!
//! Custom templates are directories with one text file per position
//! (`root.md`, `lead.md`, `worker.md`) and an optional `template.toml`
//! (`workdir = "worktree"` or `"none"`). Placeholders: `{seat}`, `{parent}`,
//! `{subordinates}`, `{group}`, `{workdir}`, `{branch}`. Templates are text
//! only; nothing in them is executed.

use crate::team::{Position, Team, Template};
use std::path::Path;

/// `workdir` of a custom template: `"worktree"` or `"none"` (default).
pub fn custom_workdir(dir: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(dir.join("template.toml")) else {
        return "none".into();
    };
    text.parse::<toml::Table>()
        .ok()
        .and_then(|t| {
            t.get("workdir")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|w| w == "worktree")
        .unwrap_or_else(|| "none".into())
}

fn quoted_list(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The full role text of `seat`: the template's part, then the seat's own
/// `role`. `main_branch` is the repository's main branch (worktree
/// templates).
pub fn role_text(team: &Team, seat: &str, main_branch: &str) -> String {
    let own = team.seat(seat).and_then(|s| s.role.clone());
    let generated = match &team.template {
        Template::StagedMerge => staged_merge(team, seat, main_branch),
        Template::None => generic(team, seat),
        Template::Custom(dir) => custom(team, seat, dir, main_branch),
    };
    match own {
        Some(r) if !generated.is_empty() => format!("{generated}\n\n{r}"),
        Some(r) => r,
        None => generated,
    }
}

/// "Who is above and below you", for teams without a template.
fn generic(team: &Team, seat: &str) -> String {
    let mut out = Vec::new();
    if let Some(p) = team.seat(seat).and_then(|s| s.parent.as_deref()) {
        out.push(format!("You report to `{p}`."));
    }
    let kids = team.children(seat);
    if !kids.is_empty() {
        out.push(format!(
            "Your team: {} (they report to you).",
            quoted_list(&kids)
        ));
    }
    out.join(" ")
}

/// The built-in staged-merge workflow: every seat works on its own branch
/// in its own worktree and sends merge requests to its maintainer (parent),
/// who merges them into an integration branch and sends that up in turn.
fn staged_merge(team: &Team, seat: &str, main_branch: &str) -> String {
    let repo = team
        .repo
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let dir = team.workdir(seat).display().to_string();
    let branch = team.branch(seat);
    let pos = team.position(seat);
    let parent = team.seat(seat).and_then(|s| s.parent.clone());
    let kids = team.children(seat);
    let mut out = vec![format!(
        "Workflow: staged merge. Your worktree is {dir} on branch `{branch}`; \
         commit your work there. All worktrees share one repository ({repo}), \
         so branches are visible to everyone without fetching."
    )];
    if !kids.is_empty() {
        let int = team.integration_branch(seat, main_branch);
        let how = if pos == Position::Root {
            format!(
                "merge it into `{int}` in the main checkout (`git -C {repo} merge --no-ff \
                 <commit>`), run the tests"
            )
        } else {
            let up = parent
                .as_deref()
                .map(|p| team.integration_branch(p, main_branch))
                .unwrap_or_else(|| main_branch.to_string());
            format!(
                "`git switch {int}`, `git merge --no-ff <commit>`, run the tests, then \
                 `git switch {branch}`. Never rebase `{int}`: your team builds on it and the \
                 commits named in their requests must stay reachable; to catch up with \
                 `{up}`, merge it into `{int}`"
            )
        };
        let list = team
            .groups
            .iter()
            .find(|g| g.lead == seat)
            .map(|g| format!(" (or the list `{}`)", g.name))
            .unwrap_or_default();
        out.push(format!(
            "You maintain {}: they mail you merge requests (meta repo, branch, commit). \
             Read each request in full (`amail read <id>`). Merge exactly the commit named in \
             its `commit` meta, not the branch tip (the branch may have moved since); if that \
             commit does not exist, reply \"changes needed: resubmit\". \
             Review with `git log -p {int}..<commit>`; to accept, {how}. Reply to each request \
             with \"merged as <merge commit>\", \"changes needed: ...\" or \"rejected: ...\". \
             When you get a requirement or interface change, forward it to every member of \
             your team it affects{list} and wait until each has confirmed; let them make \
             the changes in their own branches.",
            quoted_list(&kids)
        ));
    }
    if let Some(p) = &parent {
        let target = team.integration_branch(p, main_branch);
        let prepare = if kids.is_empty() {
            format!("rebase `{branch}` onto `{target}`")
        } else {
            let int = team.integration_branch(seat, main_branch);
            format!("merge `{target}` into `{int}` (do not rebase it)")
        };
        let submit = if kids.is_empty() {
            branch.clone()
        } else {
            team.integration_branch(seat, main_branch)
        };
        out.push(format!(
            "Your maintainer is `{p}`. To submit: {prepare} and run the tests, then \
             `amail send --to {p} -s \"merge: <summary>\" --meta repo={repo} \
             --meta branch={submit} --meta commit=$(git rev-parse {submit}) -m \"<what and why>\"`. \
             Ask `{p}` first when you are stuck."
        ));
    }
    out.join("\n")
}

fn custom(team: &Team, seat: &str, dir: &Path, main_branch: &str) -> String {
    let pos = team.position(seat);
    let text = ["md", "txt"]
        .iter()
        .find_map(|ext| std::fs::read_to_string(dir.join(format!("{}.{ext}", pos.as_str()))).ok());
    let Some(text) = text else {
        return generic(team, seat);
    };
    let s = team.seat(seat);
    let kids = team.children(seat);
    let branch = if team.uses_worktrees() {
        team.branch(seat)
    } else {
        String::new()
    };
    let _ = main_branch;
    text.trim()
        .replace("{seat}", seat)
        .replace(
            "{parent}",
            s.and_then(|s| s.parent.as_deref()).unwrap_or("(none)"),
        )
        .replace(
            "{subordinates}",
            &if kids.is_empty() {
                "(none)".to_string()
            } else {
                kids.join(", ")
            },
        )
        .replace(
            "{group}",
            s.and_then(|s| s.group.as_deref()).unwrap_or("(none)"),
        )
        .replace("{workdir}", &team.workdir(seat).display().to_string())
        .replace("{branch}", &branch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::parse;

    const TEAM: &str = r#"
[team]
template = "staged-merge"
repo = "/repo"
workroot = "/wt"
[defaults]
harness = "claude"
[[seat]]
name = "root"
role = "Overall technical lead."
[[group]]
name = "backend"
parent = "root"
lead = { name = "be-lead" }
workers = { count = 2, prefix = "be" }
"#;

    #[test]
    fn staged_merge_texts_for_every_position() {
        let t = parse(TEAM, Path::new("/teams")).unwrap();
        let root = role_text(&t, "root", "main");
        assert!(root.contains("/wt/root on branch `amail/root`"), "{root}");
        assert!(root.contains("You maintain `be-lead`"), "{root}");
        assert!(root.contains("merge it into `main`"), "{root}");
        assert!(
            root.contains("git -C /repo merge --no-ff <commit>"),
            "{root}"
        );
        assert!(!root.contains("Your maintainer"), "{root}");
        assert!(root.ends_with("\n\nOverall technical lead."), "{root}");

        let lead = role_text(&t, "be-lead", "main");
        assert!(
            lead.contains("/wt/be-lead on branch `amail/be-lead`"),
            "{lead}"
        );
        assert!(lead.contains("You maintain `be-1`, `be-2`"), "{lead}");
        assert!(lead.contains("git switch amail/be-lead-next"), "{lead}");
        assert!(lead.contains("Never rebase `amail/be-lead-next`"), "{lead}");
        assert!(lead.contains("to catch up with `main`, merge it"), "{lead}");
        assert!(lead.contains("forward it to every member"), "{lead}");
        assert!(lead.contains("(or the list `backend`)"), "{lead}");
        assert!(lead.contains("merged as <merge commit>"), "{lead}");
        assert!(lead.contains("git merge --no-ff <commit>"), "{lead}");
        assert!(lead.contains("not the branch tip"), "{lead}");
        assert!(lead.contains("changes needed: resubmit"), "{lead}");
        assert!(lead.contains("Your maintainer is `root`"), "{lead}");
        assert!(
            lead.contains("merge `main` into `amail/be-lead-next` (do not rebase it)"),
            "{lead}"
        );
        assert!(!lead.contains("rebase `amail/be-lead-next` onto"), "{lead}");
        assert!(lead.contains("--meta branch=amail/be-lead-next"), "{lead}");

        let w = role_text(&t, "be-2", "main");
        assert!(w.contains("/wt/be-2 on branch `amail/be-2`"), "{w}");
        assert!(!w.contains("You maintain"), "{w}");
        assert!(w.contains("Your maintainer is `be-lead`"), "{w}");
        assert!(
            w.contains("rebase `amail/be-2` onto `amail/be-lead-next`"),
            "{w}"
        );
        assert!(w.contains("amail send --to be-lead"), "{w}");
        assert!(
            w.contains("--meta repo=/repo --meta branch=amail/be-2"),
            "{w}"
        );
        // Short: it goes into every system prompt.
        assert!(lead.len() < 1500, "{}", lead.len());
    }

    #[test]
    fn no_template_says_who_is_above_and_below() {
        let t = parse(
            "[team]\nworkroot = \"/w\"\n[defaults]\nharness = \"claude\"\n[[seat]]\nname = \"a\"\nrole = \"Boss.\"\n\
             [[seat]]\nname = \"b\"\nparent = \"a\"\n",
            Path::new("/x"),
        )
        .unwrap();
        assert_eq!(
            role_text(&t, "a", "main"),
            "Your team: `b` (they report to you).\n\nBoss."
        );
        assert_eq!(role_text(&t, "b", "main"), "You report to `a`.");
    }

    #[test]
    fn custom_template_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("worker.md"),
            "I am {seat} in {group}; boss {parent}; dir {workdir}; branch {branch}.\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("lead.md"), "Lead {seat} of {subordinates}.").unwrap();
        std::fs::write(dir.path().join("template.toml"), "workdir = \"worktree\"\n").unwrap();
        let text = format!(
            "[team]\ntemplate = {:?}\nrepo = \"/r\"\nworkroot = \"/w\"\n[defaults]\nharness = \"codex\"\n\
             [[seat]]\nname = \"top\"\n[[group]]\nname = \"g\"\nparent = \"top\"\nlead = {{ name = \"l\" }}\n\
             workers = {{ count = 2 }}\n",
            dir.path().display().to_string()
        );
        let t = parse(&text, Path::new("/x")).unwrap();
        assert!(t.uses_worktrees());
        assert_eq!(
            role_text(&t, "g-1", "main"),
            "I am g-1 in g; boss l; dir /w/g-1; branch amail/g-1."
        );
        assert_eq!(role_text(&t, "l", "main"), "Lead l of g-1, g-2.");
        // No root.md: the generic text.
        assert_eq!(
            role_text(&t, "top", "main"),
            "Your team: `l` (they report to you)."
        );
    }
}
