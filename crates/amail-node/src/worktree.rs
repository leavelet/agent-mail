//! Git worktrees for worktree-based team templates. Idempotent: existing
//! worktrees and branches are left as they are (a maintainer may have
//! switched its worktree to its integration branch).

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Check that `repo` is a git repository with at least one commit.
pub fn check_repo(repo: &Path) -> Result<()> {
    git(repo, &["rev-parse", "--verify", "HEAD"])
        .with_context(|| format!("{} is not a git repository with a commit", repo.display()))?;
    Ok(())
}

/// The branch checked out in the main worktree (`main` if detached).
pub fn main_branch(repo: &Path) -> Result<String> {
    Ok(
        git(repo, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .unwrap_or_else(|_| "main".into()),
    )
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// Worktree path -> checked-out branch (None when detached).
fn worktrees(repo: &Path) -> Result<HashMap<PathBuf, Option<String>>> {
    let text = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut out = HashMap::new();
    let mut cur: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            let p = PathBuf::from(p);
            out.insert(p.clone(), None);
            cur = Some(p);
        } else if let Some(b) = line.strip_prefix("branch ")
            && let Some(p) = &cur
        {
            out.insert(
                p.clone(),
                Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string()),
            );
        }
    }
    Ok(out)
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// What [`prepare`] should create.
pub struct WorktreeSpec {
    pub path: PathBuf,
    pub branch: String,
    /// An integration branch to create (no worktree), if any.
    pub integration: Option<String>,
}

/// Create missing worktrees and branches from `base` (the main branch).
/// Returns one line per change made.
pub fn prepare(repo: &Path, base: &str, specs: &[WorktreeSpec]) -> Result<Vec<String>> {
    let mut done = Vec::new();
    let existing = worktrees(repo)?;
    let existing: HashMap<PathBuf, Option<String>> =
        existing.into_iter().map(|(p, b)| (canon(&p), b)).collect();
    for s in specs {
        if let Some(int) = &s.integration
            && !branch_exists(repo, int)
        {
            git(repo, &["branch", int, base])?;
            done.push(format!("created branch {int} from {base}"));
        }
        if existing.contains_key(&canon(&s.path)) {
            continue;
        }
        if s.path.exists() {
            let empty = std::fs::read_dir(&s.path)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false);
            if !empty {
                bail!(
                    "{} exists and is not a worktree of {}; move it away or remove it",
                    s.path.display(),
                    repo.display()
                );
            }
        }
        if let Some(parent) = s.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let path = s.path.to_string_lossy();
        if branch_exists(repo, &s.branch) {
            git(repo, &["worktree", "add", &path, &s.branch])?;
            done.push(format!(
                "added worktree {path} on existing branch {}",
                s.branch
            ));
        } else {
            git(repo, &["worktree", "add", "-b", &s.branch, &path, base])?;
            done.push(format!(
                "added worktree {path} on new branch {} from {base}",
                s.branch
            ));
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .unwrap();
            assert!(ok.status.success(), "{args:?}: {ok:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README"), "x\n").unwrap();
        run(&["add", "README"]);
        run(&["commit", "-q", "-m", "init"]);
    }

    #[test]
    fn worktrees_and_branches_are_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        check_repo(&repo).unwrap();
        assert_eq!(main_branch(&repo).unwrap(), "main");
        let wt = tmp.path().join("wt");
        let specs = vec![
            WorktreeSpec {
                path: wt.join("lead"),
                branch: "amail/lead".into(),
                integration: Some("amail/lead-next".into()),
            },
            WorktreeSpec {
                path: wt.join("w-1"),
                branch: "amail/w-1".into(),
                integration: None,
            },
        ];
        let first = prepare(&repo, "main", &specs).unwrap();
        assert_eq!(first.len(), 3, "{first:?}");
        assert!(wt.join("lead/README").exists());
        assert!(branch_exists(&repo, "amail/lead-next"));
        assert_eq!(
            git(&wt.join("w-1"), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "amail/w-1"
        );
        // Again: nothing to do, even after a maintainer switched branches.
        git(&wt.join("lead"), &["switch", "-q", "amail/lead-next"]).unwrap();
        assert!(prepare(&repo, "main", &specs).unwrap().is_empty());
        // A removed worktree comes back on its existing branch.
        git(
            &repo,
            &["worktree", "remove", &wt.join("w-1").to_string_lossy()],
        )
        .unwrap();
        let again = prepare(&repo, "main", &specs).unwrap();
        assert_eq!(again.len(), 1);
        assert!(again[0].contains("existing branch amail/w-1"), "{again:?}");
        // A non-empty directory in the way is an error, not overwritten.
        let blocker = WorktreeSpec {
            path: wt.join("busy"),
            branch: "amail/busy".into(),
            integration: None,
        };
        std::fs::create_dir_all(wt.join("busy")).unwrap();
        std::fs::write(wt.join("busy/file"), "keep").unwrap();
        assert!(prepare(&repo, "main", &[blocker]).is_err());
        assert_eq!(
            std::fs::read_to_string(wt.join("busy/file")).unwrap(),
            "keep"
        );
    }
}
