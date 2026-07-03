//! Wraps `git worktree` subcommands. spec §11.10: subprocess + sync fs go
//! through async tokio::process (non-blocking) and `spawn_blocking` for raw
//! `std::fs` paths.

use std::path::PathBuf;
use tokio::process::Command;

use crate::error::AdapterError;
use crate::repo::RepoEntry;

#[derive(Debug, Clone)]
pub struct WorktreeRecord {
    pub path: PathBuf,
    pub branch: Option<String>,
}

#[derive(Default)]
pub struct WorktreeManager;

impl WorktreeManager {
    pub fn new() -> Self {
        Self
    }

    /// `detached: true` checks out a detached HEAD and creates no branch —
    /// for phases that must not touch the branch namespace (design, QA).
    pub async fn create(
        &self,
        repo: &RepoEntry,
        branch: &str,
        detached: bool,
    ) -> Result<PathBuf, AdapterError> {
        let target = repo.worktree_root.join(sanitize_branch(branch));
        // git worktree add -B <branch> <path>; -B forces branch creation/reuse.
        let mut cmd = Command::new("git");
        cmd.current_dir(&repo.repo_path).arg("worktree").arg("add");
        if detached {
            cmd.arg("--detach");
        } else {
            cmd.arg("-B").arg(branch);
        }
        cmd.arg(&target);
        let out = cmd
            .output()
            .await
            .map_err(|e| AdapterError::Internal(format!("git spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if stderr.contains("already used by worktree at")
                || stderr.contains("already checked out")
            {
                return Err(AdapterError::WorktreeInUse(branch.to_string()));
            }
            return Err(AdapterError::Internal(format!(
                "git worktree add failed: {stderr}"
            )));
        }
        Ok(target)
    }

    pub async fn remove(&self, repo: &RepoEntry, branch: &str) -> Result<(), AdapterError> {
        let target = repo.worktree_root.join(sanitize_branch(branch));
        let out = Command::new("git")
            .current_dir(&repo.repo_path)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(&target)
            .output()
            .await
            .map_err(|e| AdapterError::Internal(format!("git spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(AdapterError::Internal(format!(
                "git worktree remove failed: {stderr}"
            )));
        }
        // Branch delete is best-effort; orchestrator chooses lifetime policy.
        let _ = Command::new("git")
            .current_dir(&repo.repo_path)
            .arg("branch")
            .arg("-D")
            .arg(branch)
            .output()
            .await;
        Ok(())
    }

    pub async fn list(&self, repo: &RepoEntry) -> Result<Vec<WorktreeRecord>, AdapterError> {
        let out = Command::new("git")
            .current_dir(&repo.repo_path)
            .arg("worktree")
            .arg("list")
            .arg("--porcelain")
            .output()
            .await
            .map_err(|e| AdapterError::Internal(format!("git spawn: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(AdapterError::Internal(format!(
                "git worktree list failed: {stderr}"
            )));
        }
        let s = String::from_utf8_lossy(&out.stdout);
        Ok(parse_worktree_list(&s))
    }
}

fn sanitize_branch(branch: &str) -> String {
    // worktree dir name: replace '/' with '__' so we get one flat dir under
    // worktree_root rather than nested ones.
    branch.replace('/', "__")
}

fn parse_worktree_list(out: &str) -> Vec<WorktreeRecord> {
    // porcelain output groups: blank-line-separated, lines like
    //   worktree /path
    //   HEAD abc123
    //   branch refs/heads/foo
    let mut records = Vec::new();
    let mut cur_path: Option<PathBuf> = None;
    let mut cur_branch: Option<String> = None;
    for line in out.lines() {
        if line.is_empty() {
            if let Some(p) = cur_path.take() {
                records.push(WorktreeRecord {
                    path: p,
                    branch: cur_branch.take(),
                });
            }
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            cur_path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            cur_branch = Some(b.to_string());
        }
    }
    if let Some(p) = cur_path {
        records.push(WorktreeRecord {
            path: p,
            branch: cur_branch,
        });
    }
    records
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn parses_porcelain_list() {
        let raw = "\
worktree /repo
HEAD abc
branch refs/heads/main

worktree /repo/.worktree/totsuka__x__design
HEAD def
branch refs/heads/totsuka/x/design

";
        let rec = parse_worktree_list(raw);
        assert_eq!(rec.len(), 2);
        assert_eq!(rec[1].branch.as_deref(), Some("totsuka/x/design"));
    }
}
