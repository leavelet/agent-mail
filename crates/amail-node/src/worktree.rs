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

/// Refuse a repository whose own git directory (`<repo>/.git`) was
/// changed in a way git itself never does (see [`check_common_dir`]).
/// Checked before amail runs git in the repository.
pub fn check_git_dir(repo: &Path) -> Result<()> {
    let dir = repo.join(".git");
    if dir.is_dir() {
        check_common_dir(&dir)?;
    }
    Ok(())
}

/// Most directory levels [`check_common_dir`] descends.
const MAX_SCAN_DEPTH: usize = 32;

/// Refuse a repository's git directory `common` (the common directory of
/// all its worktrees) if it contains something git never creates there
/// and that would make git run by a person do what a sandboxed session
/// wants:
///
/// - a `commondir` file: git run in the main checkout then takes its
///   config and hooks from the directory the file names;
/// - a symbolic link in `refs/`, `logs/`, `amail/` or a linked worktree's
///   git directory (`worktrees/<name>/` and its `logs/`): git appends
///   reflog lines and writes some files through symbolic links, so it
///   would write to the file the link names, for example a shell start-up
///   file, with text (a commit subject) a session chose.
///
/// Codex seats may write some of these directories (see the Codex
/// adapter), so the node checks this before it starts a Codex session and
/// `amail up` before it runs git.
pub fn check_common_dir(common: &Path) -> Result<()> {
    if common.join("commondir").exists() {
        bail!(
            "{} contains a commondir file, which git does not create in a \
             repository's git directory; git run in that repository would take \
             its config and hooks from the directory it names. Check it and \
             remove it first",
            common.display()
        );
    }
    let mut found = None;
    for sub in ["refs", "logs", "amail"] {
        find_symlink(&common.join(sub), MAX_SCAN_DEPTH, &mut found);
    }
    if let Ok(rd) = std::fs::read_dir(common.join("worktrees")) {
        for e in rd.flatten() {
            let wt = e.path();
            find_symlink(&wt, 0, &mut found);
            find_symlink(&wt.join("logs"), MAX_SCAN_DEPTH, &mut found);
        }
    }
    if let Some(p) = found {
        bail!(
            "{} is a symbolic link. Git does not create symbolic links there, and \
             git run by you would write through it to the file it names (for \
             example, append a reflog line that ends with a commit subject). \
             Check it and remove it first",
            p.display()
        );
    }
    Ok(())
}

/// Set `found` to the first symbolic link in `dir` (an entry of it, or
/// `dir` itself), descending at most `depth` more levels. Symbolic links
/// are not followed.
fn find_symlink(dir: &Path, depth: usize, found: &mut Option<PathBuf>) {
    if found.is_some() {
        return;
    }
    let Ok(m) = std::fs::symlink_metadata(dir) else {
        return;
    };
    if m.file_type().is_symlink() {
        *found = Some(dir.to_path_buf());
        return;
    }
    if !m.is_dir() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_symlink() {
            *found = Some(e.path());
            return;
        }
        if t.is_dir() && depth > 0 {
            find_symlink(&e.path(), depth - 1, found);
            if found.is_some() {
                return;
            }
        }
    }
}

/// Check that `repo` is a git repository with at least one commit.
pub fn check_repo(repo: &Path) -> Result<()> {
    check_git_dir(repo)?;
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
    check_git_dir(repo)?;
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

    /// A `commondir` file in the repository's own git directory would make
    /// git take config and hooks from elsewhere: refused before running git.
    #[test]
    fn redirected_git_dir_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        check_repo(&repo).unwrap();
        std::fs::write(repo.join(".git/commondir"), "/elsewhere\n").unwrap();
        let e = check_repo(&repo).unwrap_err().to_string();
        assert!(e.contains("commondir"), "{e}");
        assert!(prepare(&repo, "main", &[]).is_err());
    }

    /// A symbolic link where git appends reflog lines (or in a linked
    /// worktree's git directory) is refused before running git.
    #[test]
    fn symlinks_in_git_dir_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        init_repo(&repo);
        git(&repo, &["worktree", "add", "-q", "-b", "w", "../wt"]).unwrap();
        check_repo(&repo).unwrap();
        let target = tmp.path().join("victim");
        for link in [
            ".git/logs/refs/heads/evil",
            ".git/refs/heads/evil",
            ".git/worktrees/wt/logs/HEAD",
            ".git/worktrees/wt/ORIG_HEAD",
        ] {
            let p = repo.join(link);
            let had = std::fs::read(&p).ok();
            let _ = std::fs::remove_file(&p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, &p).unwrap();
            let e = check_repo(&repo).unwrap_err().to_string();
            assert!(e.contains("symbolic link"), "{link}: {e}");
            std::fs::remove_file(&p).unwrap();
            if let Some(b) = had {
                std::fs::write(&p, b).unwrap();
            }
            check_repo(&repo).unwrap();
        }
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
