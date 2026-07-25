//! Integration test for worktree lifecycle against a real git repo, using a
//! local bare repo as `origin` (F-20–F-25).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::paths::Paths;
use orchestrator_core::worktree::{
    CleanupOutcome, CleanupPolicy, CreateRequest, DEFAULT_BRANCH_TEMPLATE, WorktreeManager,
    default_location_template,
};

use test_support::{bare_origin_and_clone as setup, git, scratch};

/// An operator-written location template with a `${ENV}` reference. The
/// built-in default no longer has this shape (it is pre-resolved from
/// [`Paths`]), but user config still supports it, so the lifecycle tests keep
/// exercising the expansion path.
const ENV_LOCATION_TEMPLATE: &str = "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{branch}";

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
        location_template: ENV_LOCATION_TEMPLATE,
        base_branch: None,
        env,
    }
}

/// The built-in default must create a real worktree on a machine that does not
/// set `XDG_STATE_HOME` — the macOS norm. Before the default was pre-resolved
/// from [`Paths`], `expand_env` rejected the unset variable and every dispatch
/// failed at worktree creation.
#[test]
fn default_location_creates_a_worktree_without_xdg_state_home() {
    let base = scratch("default-location-no-xdg");
    let clone = setup(&base);
    // `HOME` only: no XDG_STATE_HOME anywhere, in `Paths` or in the render env.
    let home = base.join("home");
    let paths = Paths::from_env(|k| match k {
        "HOME" => Some(home.display().to_string()),
        _ => None,
    })
    .unwrap();
    let template = default_location_template(&paths);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr
        .create(&CreateRequest {
            repo_path: &clone,
            repo_name: "myrepo",
            source: "slack",
            task_id: "C0ABCDEF12:1720000000.123456",
            branch_template: DEFAULT_BRANCH_TEMPLATE,
            location_template: &template,
            base_branch: None,
            env: &HashMap::new(),
        })
        .unwrap();

    assert_eq!(wt.branch, "agent/slack-C0ABCDEF12-1720000000.123456");
    assert!(wt.path.is_dir(), "worktree dir must exist");
    assert_eq!(
        wt.path,
        home.join(".local/state/totsuka/worktrees/myrepo")
            .join("agent-slack-C0ABCDEF12-1720000000.123456")
    );
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

/// After a full cleanup (directory *and* branch gone) the very same request
/// must produce the very same worktree again (#254). This is the path a task
/// takes when it is dispatched a second time — `task retry`, or a follow-up
/// message in the same conversation — under `plan_cleanup = "immediate"`.
#[test]
fn recreates_a_cleaned_up_worktree_at_the_same_path() {
    let base = scratch("recreate-clean");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let first = mgr.create(&request(&clone, "42", &env)).unwrap();
    mgr.remove(&clone, &first.path, &first.branch).unwrap();
    assert!(!first.path.exists());

    let second = mgr.create(&request(&clone, "42", &env)).unwrap();
    assert_eq!(second.path, first.path, "same task → same path");
    assert_eq!(second.branch, first.branch, "same task → same branch");
    assert!(second.path.is_dir());

    let _ = std::fs::remove_dir_all(&base);
}

/// The branch routinely outlives its directory: `remove` deletes it only
/// best-effort, and `branch -d` refuses a branch with unmerged commits — which
/// is precisely the branch worth keeping. Re-creation must check that branch
/// out (no `-b`) and must **not** reset it back to `origin/{default}`, or the
/// agent's committed work would be destroyed by the recovery path (#254).
#[test]
fn recreates_over_a_surviving_branch_without_losing_its_commits() {
    let base = scratch("recreate-branch");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let first = mgr.create(&request(&clone, "43", &env)).unwrap();
    git(
        &first.path,
        &["commit", "--allow-empty", "-m", "agent work"],
    );
    let agent_commit = git(&first.path, &["rev-parse", "HEAD"]);
    // Drop the directory only — `git worktree remove` leaves the branch.
    git(
        &clone,
        &["worktree", "remove", &first.path.display().to_string()],
    );
    assert!(!first.path.exists());
    assert!(
        !git(&clone, &["branch", "--list", &first.branch]).is_empty(),
        "the branch must survive for this test to mean anything"
    );

    let second = mgr.create(&request(&clone, "43", &env)).unwrap();
    assert_eq!(second.branch, first.branch);
    assert_eq!(
        git(&second.path, &["rev-parse", "HEAD"]),
        agent_commit,
        "re-creation must keep the branch's commits, not reset it to origin"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A directory removed *without* `git worktree remove` (a manual `rm -rf`, or a
/// crash mid-cleanup) leaves the registration behind, and git then refuses to
/// add at that path. Re-creation prunes the stale entry and proceeds (#254).
#[test]
fn recreates_after_a_manual_directory_removal_leaves_a_stale_registration() {
    let base = scratch("recreate-stale");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let first = mgr.create(&request(&clone, "44", &env)).unwrap();
    std::fs::remove_dir_all(&first.path).unwrap();
    assert!(
        git(&clone, &["worktree", "list", "--porcelain"]).contains("prunable"),
        "the registration must still be there for this test to mean anything"
    );

    let second = mgr.create(&request(&clone, "44", &env)).unwrap();
    assert_eq!(second.path, first.path);
    assert!(second.path.is_dir());

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
