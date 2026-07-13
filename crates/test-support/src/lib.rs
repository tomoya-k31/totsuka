//! Shared helpers for totsuka's integration tests (#66).
//!
//! Consolidates the real-git-repo and scratch-dir setup that the worktree,
//! run-loop, and CLI E2E tests all need, so the git-signing workaround and the
//! bare-origin bootstrap live in one place.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run a `git` command in `cwd`, asserting success, returning trimmed stdout.
///
/// Commit/tag signing is disabled so a local signing agent (e.g. 1Password)
/// never blocks a background test run.
pub fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A fresh, empty scratch directory unique to this process and `name`.
///
/// Removed first if it already exists, so a re-run starts clean.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("totsuka-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Create a bare `origin.git` with one commit on `main` and a working `clone`
/// of it under `base`. Returns the clone path. The clone has a committer
/// identity configured so tests may commit directly.
pub fn bare_origin_and_clone(base: &Path) -> PathBuf {
    let origin = base.join("origin.git");
    git(
        base,
        &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
    );

    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    git(&seed, &["config", "user.email", "t@example.com"]);
    git(&seed, &["config", "user.name", "T"]);
    git(&seed, &["commit", "--allow-empty", "-m", "init"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "origin", "main"]);

    let clone = base.join("clone");
    git(
        base,
        &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
    );
    git(&clone, &["config", "user.email", "t@example.com"]);
    git(&clone, &["config", "user.name", "T"]);
    clone
}
