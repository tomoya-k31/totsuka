//! Integration test for worktree lifecycle against a real git repo, using a
//! local bare repo as `origin` (F-20–F-25).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::worktree::{
    CleanupOutcome, CleanupPolicy, CreateRequest, DEFAULT_BRANCH_TEMPLATE,
    DEFAULT_LOCATION_TEMPLATE, WorktreeManager,
};

/// Run a git command, asserting success, returning trimmed stdout.
fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Set up `origin.git` (bare) with one commit on `main`, and a clone of it.
fn setup(base: &Path) -> PathBuf {
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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("totsuka-wt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn env(state_dir: &Path) -> HashMap<String, String> {
    HashMap::from([(
        "XDG_STATE_HOME".to_string(),
        state_dir.display().to_string(),
    )])
}

fn request<'a>(
    clone: &'a Path,
    task_id: &'a str,
    env: &'a HashMap<String, String>,
) -> CreateRequest<'a> {
    CreateRequest {
        repo_path: clone,
        repo_name: "myrepo",
        source: "github",
        task_id,
        branch_template: DEFAULT_BRANCH_TEMPLATE,
        location_template: DEFAULT_LOCATION_TEMPLATE,
        base_branch: None,
        env,
    }
}

#[test]
fn create_cleanup_and_orphan_detection() {
    let base = scratch("lifecycle");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    // Create.
    let wt = mgr.create(&request(&clone, "123", &env)).unwrap();
    assert_eq!(wt.branch, "agent/github-123");
    assert!(wt.path.is_dir(), "worktree dir must exist");
    assert_eq!(
        wt.path,
        state.join("totsuka/worktrees/myrepo/agent-github-123")
    );
    // It is based on origin/main.
    let head = git(&wt.path, &["rev-parse", "HEAD"]);
    let origin_main = git(&clone, &["rev-parse", "origin/main"]);
    assert_eq!(head, origin_main);

    // Orphan detection: this worktree is unknown → reported; the main working
    // tree is never reported.
    let orphans = mgr.detect_orphans(&clone, &HashSet::new()).unwrap();
    assert!(orphans.iter().any(|p| canon(p) == canon(&wt.path)));
    assert!(
        !orphans.iter().any(|p| canon(p) == canon(&clone)),
        "the main worktree must never be an orphan"
    );
    // ...but not when it is known. Pass the raw (non-canonical) path that
    // `create()` returned to confirm detection canonicalizes both sides.
    let known: HashSet<PathBuf> = [wt.path.clone()].into_iter().collect();
    let orphans = mgr.detect_orphans(&clone, &known).unwrap();
    assert!(
        orphans.is_empty(),
        "known worktree must not be an orphan: {orphans:?}"
    );

    // Cleanup (clean worktree) removes it and the branch.
    let outcome = mgr
        .cleanup(
            &clone,
            &wt.path,
            &wt.branch,
            CleanupPolicy::Immediate,
            None,
            "2026-07-12T00:00:00Z",
        )
        .unwrap();
    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!wt.path.exists(), "worktree dir must be gone");
    let branches = git(&clone, &["branch", "--list", "agent/github-123"]);
    assert!(branches.is_empty(), "branch must be deleted");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn branches_from_origin_even_with_stale_local_default() {
    let base = scratch("stale");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let origin_main = git(&clone, &["rev-parse", "origin/main"]);
    // Advance the *local* main so it diverges from origin/main.
    git(&clone, &["commit", "--allow-empty", "-m", "local-only"]);
    let local_main = git(&clone, &["rev-parse", "main"]);
    assert_ne!(local_main, origin_main);

    let wt = mgr.create(&request(&clone, "9", &env)).unwrap();
    let head = git(&wt.path, &["rev-parse", "HEAD"]);
    assert_eq!(
        head, origin_main,
        "must branch from origin/main, not stale local main"
    );
    assert_ne!(head, local_main);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn dirty_worktree_is_not_removed() {
    let base = scratch("dirty");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr.create(&request(&clone, "7", &env)).unwrap();
    // Leave an uncommitted change.
    std::fs::write(wt.path.join("scratch.txt"), b"work in progress").unwrap();

    let outcome = mgr
        .cleanup(
            &clone,
            &wt.path,
            &wt.branch,
            CleanupPolicy::Immediate,
            None,
            "2026-07-12T00:00:00Z",
        )
        .unwrap();
    assert_eq!(outcome, CleanupOutcome::DirtySkipped);
    assert!(wt.path.is_dir(), "dirty worktree must be preserved");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn retain_policies_do_not_remove() {
    let base = scratch("retain");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    // Manual: never auto-remove.
    let wt = mgr.create(&request(&clone, "m", &env)).unwrap();
    let outcome = mgr
        .cleanup(
            &clone,
            &wt.path,
            &wt.branch,
            CleanupPolicy::Manual,
            Some("2026-07-01T00:00:00Z"),
            "2026-07-12T00:00:00Z",
        )
        .unwrap();
    assert_eq!(outcome, CleanupOutcome::Retained);
    assert!(wt.path.is_dir(), "manual policy must keep the worktree");

    // RetentionDays not yet elapsed: keep.
    let wt2 = mgr.create(&request(&clone, "r", &env)).unwrap();
    let outcome = mgr
        .cleanup(
            &clone,
            &wt2.path,
            &wt2.branch,
            CleanupPolicy::RetentionDays(30),
            Some("2026-07-11T00:00:00Z"),
            "2026-07-12T00:00:00Z",
        )
        .unwrap();
    assert_eq!(outcome, CleanupOutcome::Retained);
    assert!(
        wt2.path.is_dir(),
        "retention-not-elapsed must keep the worktree"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parallel_creation_does_not_deadlock() {
    let base = scratch("parallel");
    let clone = setup(&base);
    let state = base.join("state");
    let mgr = WorktreeManager::new(SystemGitRunner);

    let handles: Vec<_> = (0..6)
        .map(|i| {
            let clone = clone.clone();
            let state = state.clone();
            let mgr = mgr.clone();
            std::thread::spawn(move || {
                let env = env(&state);
                let task_id = format!("p{i}");
                mgr.create(&request(&clone, &task_id, &env))
                    .map(|w| w.branch)
            })
        })
        .collect();

    let mut branches: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().unwrap().expect("parallel create failed"))
        .collect();
    branches.sort();
    branches.dedup();
    assert_eq!(
        branches.len(),
        6,
        "all 6 parallel creations must succeed uniquely"
    );

    let _ = std::fs::remove_dir_all(&base);
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
