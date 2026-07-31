//! Integration test for worktree lifecycle against a real git repo, using a
//! local bare repo as `origin` (F-20–F-25).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::paths::Paths;
use orchestrator_core::worktree::{
    CleanupDecision, CleanupOutcome, CleanupPolicy, CleanupRequest, CreateRequest,
    DEFAULT_WORKTREE_NAME_TEMPLATE, WorktreeManager, default_location_template,
};

use test_support::{bare_origin_and_clone as setup, git, scratch};

/// An operator-written location template with a `${ENV}` reference. The
/// built-in default no longer has this shape (it is pre-resolved from
/// [`Paths`]), but user config still supports it, so the lifecycle tests keep
/// exercising the expansion path.
const ENV_LOCATION_TEMPLATE: &str =
    "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{worktree_name}";

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
        existing_branch: None,
        name_template: DEFAULT_WORKTREE_NAME_TEMPLATE,
        location_template: ENV_LOCATION_TEMPLATE,
        base_branch: None,
        env,
    }
}

/// A re-creation request: the task is known to have been on `branch`.
fn resume<'a>(
    clone: &'a Path,
    task_id: &'a str,
    branch: &'a str,
    env: &'a HashMap<String, String>,
) -> CreateRequest<'a> {
    CreateRequest {
        existing_branch: Some(branch),
        ..request(clone, task_id, env)
    }
}

/// Stand in for the agent: name a branch and switch to it, exactly as the
/// `branch_convention` prompt asks (no start-point argument).
fn agent_branches(worktree: &Path, name: &str) -> String {
    git(worktree, &["switch", "-c", name]);
    name.to_string()
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
            existing_branch: None,
            name_template: DEFAULT_WORKTREE_NAME_TEMPLATE,
            location_template: &template,
            base_branch: None,
            env: &HashMap::new(),
        })
        .unwrap();

    // Handed over detached: naming is the agent's, because the convention it
    // has to follow is written inside the repository.
    assert_eq!(wt.branch, None);
    assert_eq!(mgr.head_branch(&wt.path), None);
    assert!(wt.path.is_dir(), "worktree dir must exist");
    // The directory is named from `(source, task_id)`, not from the branch —
    // and the `:` a Slack task id carries never reaches the filesystem.
    assert_eq!(
        wt.path,
        home.join(".local/state/totsuka/worktrees/myrepo")
            .join("slack-C0ABCDEF12-1720000000.123456")
    );
    // The base commit is reported so cleanup can later prove the branch it is
    // about to delete descends from this worktree's starting point.
    let head = git(&clone, &["rev-parse", "origin/main"]);
    assert_eq!(wt.base_commit, head.trim());
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
    assert_eq!(wt.branch, None, "created detached");
    assert!(wt.path.is_dir(), "worktree dir must exist");
    assert_eq!(wt.path, state.join("totsuka/worktrees/myrepo/github-123"));
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

    // The agent names its branch; that is how the orchestrator learns it.
    let branch = agent_branches(&wt.path, "feat/add-widget");
    assert_eq!(mgr.head_branch(&wt.path).as_deref(), Some(branch.as_str()));

    // Cleanup (clean worktree) removes it and the branch.
    let outcome = mgr
        .cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap();
    assert_eq!(outcome, CleanupOutcome::Removed);
    assert!(!wt.path.exists(), "worktree dir must be gone");
    let branches = git(&clone, &["branch", "--list", &branch]);
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
    let branch = agent_branches(&first.path, "fix/flaky-test");
    mgr.remove(&clone, &first.path, Some(&branch), Some(&first.base_commit))
        .unwrap();
    assert!(!first.path.exists());

    // Nothing of the branch survived (it held nothing origin did not), so the
    // agent names the work again — but the path is still a pure function of
    // the task, which is what keeps the agent session attached to it.
    let second = mgr.create(&request(&clone, "42", &env)).unwrap();
    assert_eq!(second.path, first.path, "same task → same path");
    assert_eq!(second.branch, None);
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
    let branch = agent_branches(&first.path, "feat/keep-my-commits");
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
        !git(&clone, &["branch", "--list", &branch]).is_empty(),
        "the branch must survive for this test to mean anything"
    );

    let second = mgr.create(&resume(&clone, "43", &branch, &env)).unwrap();
    assert_eq!(second.branch.as_deref(), Some(branch.as_str()));
    assert_eq!(
        git(&second.path, &["rev-parse", "HEAD"]),
        agent_commit,
        "re-creation must keep the branch's commits, not reset it to origin"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The nastiest re-creation case, and the one a `pull_request` workflow hits
/// every time: once `push_branch` has set an upstream, `git branch -d` *does*
/// delete the branch (it is merged into its upstream), so cleanup succeeds on
/// exactly the branches whose commits matter. Re-creating from
/// `origin/{default}` there would strand the published work and make the next
/// `push -u` a non-fast-forward rejection (#254).
#[test]
fn recreates_from_the_remote_branch_after_a_published_branch_was_cleaned_up() {
    let base = scratch("recreate-published");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let first = mgr.create(&request(&clone, "45", &env)).unwrap();
    let branch = agent_branches(&first.path, "feat/published");
    git(
        &first.path,
        &["commit", "--allow-empty", "-m", "published work"],
    );
    let published = git(&first.path, &["rev-parse", "HEAD"]);
    mgr.push_branch(&first.path, &branch).unwrap();

    mgr.remove(&clone, &first.path, Some(&branch), Some(&first.base_commit))
        .unwrap();
    assert!(
        git(&clone, &["branch", "--list", &branch]).is_empty(),
        "the local branch really is deleted once published — that is the hazard"
    );

    let second = mgr.create(&resume(&clone, "45", &branch, &env)).unwrap();
    assert_eq!(
        git(&second.path, &["rev-parse", "HEAD"]),
        published,
        "must re-create at the remote branch, not at origin/main"
    );
    // ...so the next publish fast-forwards instead of being rejected.
    git(
        &second.path,
        &["commit", "--allow-empty", "-m", "more work"],
    );
    mgr.push_branch(&second.path, &branch).unwrap();

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
    let branch = agent_branches(&wt.path, "chore/dirty");
    // Leave an uncommitted change.
    std::fs::write(wt.path.join("scratch.txt"), b"work in progress").unwrap();

    let outcome = mgr
        .cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
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
    let branch = agent_branches(&wt.path, "chore/manual");
    let outcome = mgr
        .cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Manual,
            finished_at: Some("2026-07-01T00:00:00Z"),
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap();
    assert_eq!(outcome, CleanupOutcome::Retained);
    assert!(wt.path.is_dir(), "manual policy must keep the worktree");

    // RetentionDays not yet elapsed: keep.
    let wt2 = mgr.create(&request(&clone, "r", &env)).unwrap();
    let branch2 = agent_branches(&wt2.path, "chore/retained");
    let outcome = mgr
        .cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt2.path,
            branch: Some(&branch2),
            base_commit: Some(&wt2.base_commit),
            policy: CleanupPolicy::RetentionDays(30),
            finished_at: Some("2026-07-11T00:00:00Z"),
            now: "2026-07-12T00:00:00Z",
        })
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
                mgr.create(&request(&clone, &task_id, &env)).map(|w| w.path)
            })
        })
        .collect();

    let mut paths: Vec<PathBuf> = handles
        .into_iter()
        .map(|h| h.join().unwrap().expect("parallel create failed"))
        .collect();
    paths.sort();
    paths.dedup();
    assert_eq!(
        paths.len(),
        6,
        "all 6 parallel creations must succeed uniquely"
    );

    let _ = std::fs::remove_dir_all(&base);
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// The exact shape of the real-machine bug (#266): the local default branch
/// lags `origin`, which is its normal state, and cleanup silently fails to
/// delete the task's branch. Five `agent/*` branches had accumulated.
#[test]
fn cleanup_deletes_the_branch_even_when_the_local_default_lags_origin() {
    let base = scratch("branch_cleanup_stale");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    // Advance `origin/main` and leave the local `main` behind it — the
    // ordinary state of any clone that has not pulled lately.
    git(&clone, &["commit", "--allow-empty", "-m", "upstream work"]);
    git(&clone, &["push", "origin", "main"]);
    git(&clone, &["reset", "--hard", "HEAD~1"]);
    assert_ne!(
        git(&clone, &["rev-parse", "main"]),
        git(&clone, &["rev-parse", "origin/main"]),
        "the local default must lag origin for this test to mean anything"
    );

    let wt = mgr.create(&request(&clone, "11", &env)).unwrap();
    let branch = agent_branches(&wt.path, "fix/lagging-default");
    assert!(
        git(&clone, &["branch", "--list", &branch]).contains(&branch),
        "the branch exists before cleanup"
    );
    // `git branch -d` — what this used to do — refuses here, because it
    // judges against the lagging local HEAD.
    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::Removed
    );
    assert!(
        git(&clone, &["branch", "--list", &branch]).is_empty(),
        "the branch must be gone: it holds nothing that origin does not"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The other half of the contract: a branch carrying commits that exist
/// nowhere else survives its worktree.
#[test]
fn cleanup_keeps_a_branch_whose_commits_are_not_on_origin() {
    let base = scratch("branch_cleanup_unpushed");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr.create(&request(&clone, "12", &env)).unwrap();
    let branch = agent_branches(&wt.path, "feat/unpushed");
    // The agent committed, and nothing pushed it.
    std::fs::write(wt.path.join("work.txt"), b"the agent's output").unwrap();
    git(&wt.path, &["add", "work.txt"]);
    git(&wt.path, &["commit", "-m", "agent work"]);

    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::Removed,
        "the worktree is clean, so it still goes"
    );
    assert!(
        git(&clone, &["branch", "--list", &branch]).contains(&branch),
        "the branch must survive — its commit exists nowhere else"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A pushed branch with an open PR is published, so cleanup may take the
/// local copy. `git branch -d` got this right via the upstream rule, and a
/// narrower "is it merged into origin/main?" test would have regressed it.
#[test]
fn cleanup_deletes_a_pushed_branch_that_is_not_merged_into_the_default() {
    let base = scratch("branch_cleanup_pushed");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr.create(&request(&clone, "13", &env)).unwrap();
    let branch = agent_branches(&wt.path, "feat/pushed-open-pr");
    std::fs::write(wt.path.join("work.txt"), b"the agent's output").unwrap();
    git(&wt.path, &["add", "work.txt"]);
    git(&wt.path, &["commit", "-m", "agent work"]);
    mgr.push_branch(&wt.path, &branch).unwrap();

    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::Removed
    );
    assert!(
        git(&clone, &["branch", "--list", &branch]).is_empty(),
        "published work is safe to drop locally, even before the PR merges"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The guard that keeps the operator's branches out of cleanup's reach.
///
/// Deleting on "every commit is on origin" alone was safe only while the name
/// was orchestrator-generated. Agent-chosen names live in the same namespace a
/// human uses, and a fully-pushed branch someone else made satisfies that test
/// exactly. What distinguishes them is the base commit: a branch cut from an
/// older default branch does not contain this worktree's starting point.
#[test]
fn cleanup_keeps_a_branch_that_does_not_descend_from_the_base_commit() {
    let base = scratch("branch_cleanup_foreign");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    // A human's branch, cut from the default branch as it was, and pushed.
    git(&clone, &["branch", "feat/human-work"]);
    git(&clone, &["push", "origin", "feat/human-work"]);
    // `origin/main` then moves on, so the worktree's base commit is not on the
    // human's branch.
    git(&clone, &["commit", "--allow-empty", "-m", "upstream work"]);
    git(&clone, &["push", "origin", "main"]);

    let wt = mgr.create(&request(&clone, "14", &env)).unwrap();
    assert_ne!(
        wt.base_commit,
        git(&clone, &["rev-parse", "feat/human-work"]),
        "the base commit must be ahead of the human's branch for this to mean anything"
    );

    // The agent ended up on the human's branch rather than making its own —
    // whether by `git switch` without `-c`, or by `-c` colliding and being
    // retried without it. Cleanup sees a fully-published branch and, before
    // this guard, force-deleted it.
    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some("feat/human-work"),
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::Removed,
        "the worktree itself is still this task's to remove"
    );
    assert!(
        git(&clone, &["branch", "--list", "feat/human-work"]).contains("feat/human-work"),
        "a branch that does not descend from this worktree's base is not ours to delete"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A row written before the base commit was recorded cannot prove ownership,
/// and being unable to prove it is not permission to destroy.
#[test]
fn cleanup_keeps_a_branch_when_no_base_commit_was_recorded() {
    let base = scratch("branch_cleanup_no_base");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr.create(&request(&clone, "15", &env)).unwrap();
    let branch = agent_branches(&wt.path, "feat/legacy-row");

    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: Some(&branch),
            base_commit: None,
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::Removed
    );
    assert!(
        git(&clone, &["branch", "--list", &branch]).contains(&branch),
        "no base commit → no proof of ownership → keep the branch"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The data-loss path detached creation opens.
///
/// `git status --porcelain` is **empty** for work committed onto a detached
/// `HEAD`, so the F-23 dirty guard does not see it; `git worktree remove` then
/// takes the only reachability those commits had. This could not happen while
/// the orchestrator put every worktree on a branch itself.
#[test]
fn a_detached_worktree_with_commits_is_kept() {
    let base = scratch("detached_commits");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr.create(&request(&clone, "16", &env)).unwrap();
    // The agent ignored the instruction to branch and just committed.
    std::fs::write(wt.path.join("work.txt"), b"the agent's output").unwrap();
    git(&wt.path, &["add", "work.txt"]);
    git(&wt.path, &["commit", "-m", "agent work"]);
    assert!(
        git(&wt.path, &["status", "--porcelain"]).is_empty(),
        "the worktree is clean — that is exactly why the dirty guard misses this"
    );

    assert_eq!(
        mgr.decide_cleanup(
            &wt.path,
            Some(&wt.base_commit),
            CleanupPolicy::Immediate,
            None,
            "2026-07-12T00:00:00Z",
        )
        .unwrap(),
        CleanupDecision::Dirty
    );
    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: None,
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::DirtySkipped
    );
    assert!(
        wt.path.is_dir(),
        "commits reachable from nothing but this worktree must survive it"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// Plan mode is detached for its whole life and is deliberately *not* caught by
/// the guard above: a plan-mode pane cannot run git, so it commits nothing and
/// cleanup proceeds exactly as before.
#[test]
fn a_detached_worktree_with_no_commits_is_removed() {
    let base = scratch("detached_no_commits");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner);

    let wt = mgr.create(&request(&clone, "17", &env)).unwrap();
    assert_eq!(
        mgr.cleanup(&CleanupRequest {
            repo_path: &clone,
            worktree_path: &wt.path,
            branch: None,
            base_commit: Some(&wt.base_commit),
            policy: CleanupPolicy::Immediate,
            finished_at: None,
            now: "2026-07-12T00:00:00Z",
        })
        .unwrap(),
        CleanupOutcome::Removed
    );
    assert!(!wt.path.exists());

    let _ = std::fs::remove_dir_all(&base);
}
