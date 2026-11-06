//! 让我来: the agent works on a task in its own git worktree and branch, so the
//! user's working copy is never touched and nothing is pushed until the user
//! taps 推送.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub struct Workspace {
    /// The repository's top-level directory (the user's working copy).
    pub repo: String,
    pub worktree: String,
    pub branch: String,
    /// The user's branch when the takeover started.
    pub base_branch: String,
    /// Commit the agent's work starts from (includes the session's uncommitted changes).
    pub base: String,
    /// `.git` directory shared by all worktrees (must be writable for commits).
    pub git_dir: String,
    /// The session's uncommitted changes were carried over.
    pub carried_changes: bool,
}

fn git(dir: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).output().context("git 不可用")?;
    if !out.status.success() {
        bail!("git {}：{}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn slug(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    let s: String = s.chars().take(30).collect();
    if s.is_empty() { "task".into() } else { s.trim_end_matches('-').to_string() }
}

pub fn branch_name(title: &str, now: chrono::DateTime<chrono::Local>) -> String {
    format!("loopme/{}-{}", slug(title), now.format("%m%d-%H%M"))
}

/// Creates `<data>/worktrees/<branch>` on a new branch from the user's HEAD,
/// carrying over uncommitted changes to tracked files.
pub fn prepare(cwd: &str, data_dir: &Path, title: &str) -> Result<Workspace> {
    let repo = git(cwd, &["rev-parse", "--show-toplevel"]).map_err(|_| anyhow!("{cwd} 不是 git 仓库，没法在独立分支上接手"))?;
    let base_branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|_| "HEAD".into());
    let common = git(&repo, &["rev-parse", "--git-common-dir"])?;
    let git_dir = if Path::new(&common).is_absolute() { PathBuf::from(&common) } else { Path::new(&repo).join(&common) };
    let branch = branch_name(title, chrono::Local::now());
    let worktree = data_dir.join("worktrees").join(branch.replace('/', "_"));
    std::fs::create_dir_all(worktree.parent().unwrap())?;
    let wt = worktree.display().to_string();
    git(&repo, &["worktree", "add", "-b", &branch, &wt, "HEAD"])?;

    // Carry the session's uncommitted (tracked) changes over as a first commit.
    let diff = Command::new("git").args(["-C", &repo, "diff", "HEAD", "--binary"]).output()?;
    let mut carried = false;
    if diff.status.success() && !diff.stdout.is_empty() {
        let mut child = Command::new("git").args(["-C", &wt, "apply", "--index", "-"]).stdin(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(&diff.stdout)?;
        }
        if child.wait()?.success() {
            git(&wt, &["-c", "user.name=LoopMe", "-c", "user.email=loopme@localhost", "commit", "-q", "--no-verify", "-m", "LoopMe：带上会话里未提交的改动"])?;
            carried = true;
        }
    }
    let base = git(&wt, &["rev-parse", "HEAD"])?;
    Ok(Workspace {
        repo,
        worktree: wt,
        branch,
        base_branch,
        base,
        git_dir: git_dir.display().to_string(),
        carried_changes: carried,
    })
}

/// Commits whatever the agent left uncommitted. Returns the `--stat` summary of its work.
pub fn finish(worktree: &str, base: &str, title: &str) -> Result<String> {
    if !git(worktree, &["status", "--porcelain"])?.is_empty() {
        git(worktree, &["add", "-A"])?;
        let msg = format!("LoopMe：{title}");
        if git(worktree, &["commit", "-q", "--no-verify", "-m", &msg]).is_err() {
            git(worktree, &["-c", "user.name=LoopMe", "-c", "user.email=loopme@localhost", "commit", "-q", "--no-verify", "-m", &msg])?;
        }
    }
    git(worktree, &["diff", "--stat", &format!("{base}..HEAD")])
}

/// The agent's changes as a patch (truncated for the phone).
pub fn diff(worktree: &str, base: &str, max_bytes: usize) -> Result<String> {
    let mut d = git(worktree, &["diff", &format!("{base}..HEAD")])?;
    if d.len() > max_bytes {
        let mut cut = max_bytes;
        while !d.is_char_boundary(cut) {
            cut -= 1;
        }
        d.truncate(cut);
        d.push_str("\n…（太长，已截断）");
    }
    Ok(d)
}

pub fn push(worktree: &str, branch: &str) -> Result<()> {
    git(worktree, &["push", "-u", "origin", branch]).map(|_| ())
}

/// Throws the work away: removes the worktree and the branch.
pub fn discard(repo: &str, worktree: &str, branch: &str) -> Result<()> {
    let _ = git(repo, &["worktree", "remove", "--force", worktree]);
    let _ = std::fs::remove_dir_all(worktree);
    let _ = git(repo, &["worktree", "prune"]);
    git(repo, &["branch", "-D", branch]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn worktree_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&repo, &["init", "-q", "-b", "main"]);
        sh(&repo, &["config", "user.name", "t"]);
        sh(&repo, &["config", "user.email", "t@t"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-q", "-m", "init"]);
        // Uncommitted work in the user's copy.
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();

        let data = tmp.path().join("data");
        let ws = prepare(repo.to_str().unwrap(), &data, "Fix login tests").unwrap();
        assert!(ws.branch.starts_with("loopme/fix-login-tests-"));
        assert!(ws.carried_changes);
        assert_eq!(ws.base_branch, "main");
        assert_eq!(std::fs::read_to_string(Path::new(&ws.worktree).join("a.txt")).unwrap(), "one\ntwo\n");

        // The agent edits; the user's copy is untouched.
        std::fs::write(Path::new(&ws.worktree).join("b.txt"), "fix\n").unwrap();
        let stat = finish(&ws.worktree, &ws.base, "修测试").unwrap();
        assert!(stat.contains("b.txt"), "{stat}");
        assert!(diff(&ws.worktree, &ws.base, 10_000).unwrap().contains("+fix"));
        assert!(!repo.join("b.txt").exists());
        assert_eq!(git(repo.to_str().unwrap(), &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(), "main");

        discard(&ws.repo, &ws.worktree, &ws.branch).unwrap();
        assert!(!Path::new(&ws.worktree).exists());
        assert!(git(repo.to_str().unwrap(), &["rev-parse", "--verify", &ws.branch]).is_err());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("修 fix-login 的测试"), "fix-login");
        assert_eq!(slug("中文"), "task");
    }
}
