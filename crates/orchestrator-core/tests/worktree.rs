//! Integration test for worktree lifecycle against a real git repo, using a
//! local bare repo as `origin` (F-20–F-25).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::domain::CleanupPolicy;
use orchestrator_core::domain::SourceTaskId;
use orchestrator_core::domain::TaskId;
use orchestrator_core::paths::Paths;
use orchestrator_core::worktree::{
    CleanupDecision, CleanupOutcome, CleanupRequest, CreateRequest, WorktreeManager,
    default_location_template,
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
    task_id: &'a SourceTaskId,
    env: &'a HashMap<String, String>,
) -> CreateRequest<'a> {
    CreateRequest {
        repo_path: clone,
        repo_name: "myrepo",
        source: "github",
        task_id,
        existing_branch: None,
        task_number: Some(TaskId(1)),
        handle: None,
        location_template: ENV_LOCATION_TEMPLATE,
        base_branch: None,
        hinted: None,
        env,
    }
}

/// A re-creation request: the task is known to have been on `branch`.
fn resume<'a>(
    clone: &'a Path,
    task_id: &'a SourceTaskId,
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&CreateRequest {
            repo_path: &clone,
            repo_name: "myrepo",
            source: "slack",
            task_id: &SourceTaskId("C0ABCDEF12:1720000000.123456".into()),
            existing_branch: None,
            task_number: Some(TaskId(1)),
            handle: None,
            location_template: &template,
            base_branch: None,
            hinted: None,
            env: &HashMap::new(),
        })
        .unwrap();

    // Handed over detached: naming is the agent's, because the convention it
    // has to follow is written inside the repository.
    assert_eq!(wt.branch, None);
    assert_eq!(mgr.head_branch(&wt.path), None);
    assert!(wt.path.is_dir(), "worktree dir must exist");
    // The directory is named from the task number and a digest (ADR-0071),
    // not from the branch and no longer from the source's id — so the `:` a
    // Slack task id carries cannot reach the filesystem at all.
    let leaf = wt.path.file_name().unwrap().to_str().unwrap();
    assert!(leaf.starts_with("1-"), "{leaf}");
    assert!(
        leaf.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{leaf}"
    );
    assert_eq!(
        wt.path.parent().unwrap(),
        home.join(".local/state/totsuka/worktrees/myrepo")
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    // Create.
    let wt = mgr
        .create(&request(&clone, &SourceTaskId("123".into()), &env))
        .unwrap();
    assert_eq!(wt.branch, None, "created detached");
    assert!(wt.path.is_dir(), "worktree dir must exist");
    let leaf = wt.path.file_name().unwrap().to_str().unwrap().to_string();
    assert!(leaf.starts_with("1-"), "{leaf}");
    assert_eq!(
        wt.path,
        state.join(format!("totsuka/worktrees/myrepo/{leaf}"))
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let first = mgr
        .create(&request(&clone, &SourceTaskId("42".into()), &env))
        .unwrap();
    let branch = agent_branches(&first.path, "fix/flaky-test");
    mgr.remove(&clone, &first.path, Some(&branch), Some(&first.base_commit))
        .unwrap();
    assert!(!first.path.exists());

    // Nothing of the branch survived (it held nothing origin did not), so the
    // agent names the work again — but the path is still a pure function of
    // the task, which is what keeps the agent session attached to it.
    let second = mgr
        .create(&request(&clone, &SourceTaskId("42".into()), &env))
        .unwrap();
    assert_eq!(second.path, first.path, "same task → same path");
    assert_eq!(second.branch, None);
    assert!(second.path.is_dir());

    let _ = std::fs::remove_dir_all(&base);
}

/// A stray directory sitting where a removed worktree used to be is "nothing
/// to clean up", not a failed cleanup (#694).
///
/// The task row keeps `worktree_path` after a successful removal, so the sweep
/// revisits the path forever; anything that recreates it — an agent's
/// leftovers, a `mkdir` by hand — used to make `git status` report `fatal: not
/// a git repository` and the orchestrator warn about it once per sweep, for
/// the life of the process, about a task that finished cleanly.
#[test]
fn a_stray_directory_at_a_removed_worktree_path_is_gone_not_an_error() {
    let base = scratch("stray-dir-cleanup");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("44".into()), &env))
        .unwrap();
    let branch = agent_branches(&wt.path, "chore/tidy");
    mgr.remove(&clone, &wt.path, Some(&branch), Some(&wt.base_commit))
        .unwrap();
    assert!(!wt.path.exists());

    // Something unrelated takes the name back.
    std::fs::create_dir_all(&wt.path).unwrap();

    let request = CleanupRequest {
        repo_path: &clone,
        worktree_path: &wt.path,
        branch: Some(&branch),
        base_commit: Some(&wt.base_commit),
        policy: CleanupPolicy::Immediate,
        finished_at: None,
        now: "2026-07-12T00:00:00Z",
    };
    assert_eq!(
        mgr.decide_cleanup(
            request.repo_path,
            request.worktree_path,
            request.base_commit,
            request.policy,
            request.finished_at,
            request.now,
        )
        .unwrap(),
        CleanupDecision::Gone
    );
    assert_eq!(mgr.cleanup(&request).unwrap(), CleanupOutcome::Gone);
    // Left where it was: `git worktree remove` could not have removed it
    // either, so cleanup does not reach for a directory it cannot account for.
    assert!(wt.path.is_dir());

    let _ = std::fs::remove_dir_all(&base);
}

/// The same, for a stray directory that sits *inside* a repository — where an
/// operator who points `[worktree].location` into the repo would leave one.
///
/// The question the cleanup asks has to be identity ("is this path a worktree
/// root"), not containment ("is this path inside some worktree"): containment
/// answers yes here, and everything after it would then be about the
/// **enclosing** repo — `git status` reporting its tree as the data-loss
/// guard, and `git worktree remove` failing on a path git does not list.
#[test]
fn a_stray_directory_inside_the_repo_is_gone_too() {
    let base = scratch("stray-dir-nested");
    let clone = setup(&base);
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let stray = clone.join(".worktrees/44-was-here");
    std::fs::create_dir_all(&stray).unwrap();
    // Dirty the repo it sits in: were the decision made about the enclosing
    // tree, this would read as `Dirty` rather than `Gone`.
    std::fs::write(clone.join("uncommitted.txt"), "x").unwrap();

    assert_eq!(
        mgr.decide_cleanup(
            &clone,
            &stray,
            None,
            CleanupPolicy::Immediate,
            None,
            "2026-07-12T00:00:00Z",
        )
        .unwrap(),
        CleanupDecision::Gone
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// And for a stray that is a repository in its own right.
///
/// "Is this path a repo root" is the other cheap question that gets this
/// wrong: a `git init` at the old name answers yes and is clean, so the
/// decision would be `Remove` — and `git worktree remove` then fails against
/// the repo that does not list it, which is the warning loop again. The
/// question has to be membership in **this repo's** registry.
#[test]
fn a_stray_repository_at_a_removed_worktree_path_is_gone_too() {
    let base = scratch("stray-repo");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("45".into()), &env))
        .unwrap();
    mgr.remove(&clone, &wt.path, None, Some(&wt.base_commit))
        .unwrap();
    std::fs::create_dir_all(&wt.path).unwrap();
    git(&wt.path, &["init"]);
    assert!(wt.path.join(".git").is_dir(), "a repository of its own");

    assert_eq!(
        mgr.decide_cleanup(
            &clone,
            &wt.path,
            Some(&wt.base_commit),
            CleanupPolicy::Immediate,
            None,
            "2026-07-12T00:00:00Z",
        )
        .unwrap(),
        CleanupDecision::Gone
    );

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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let first = mgr
        .create(&request(&clone, &SourceTaskId("43".into()), &env))
        .unwrap();
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

    let second = mgr
        .create(&resume(&clone, &SourceTaskId("43".into()), &branch, &env))
        .unwrap();
    assert_eq!(second.branch.as_deref(), Some(branch.as_str()));
    assert_eq!(
        git(&second.path, &["rev-parse", "HEAD"]),
        agent_commit,
        "re-creation must keep the branch's commits, not reset it to origin"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The nastiest re-creation case: cleanup deletes a branch once every commit on
/// it is also on `origin`, so it succeeds on exactly the branches whose commits
/// have been published — leaving those commits on the remote only. Re-creating
/// from `origin/{default}` there would strand the published work and make the
/// next push a non-fast-forward rejection (#254).
#[test]
fn recreates_from_the_remote_branch_after_a_published_branch_was_cleaned_up() {
    let base = scratch("recreate-published");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let first = mgr
        .create(&request(&clone, &SourceTaskId("45".into()), &env))
        .unwrap();
    let branch = agent_branches(&first.path, "feat/published");
    git(
        &first.path,
        &["commit", "--allow-empty", "-m", "published work"],
    );
    let published = git(&first.path, &["rev-parse", "HEAD"]);
    git(&first.path, &["push", "-u", "origin", &branch]);

    mgr.remove(&clone, &first.path, Some(&branch), Some(&first.base_commit))
        .unwrap();
    assert!(
        git(&clone, &["branch", "--list", &branch]).is_empty(),
        "the local branch really is deleted once published — that is the hazard"
    );

    let second = mgr
        .create(&resume(&clone, &SourceTaskId("45".into()), &branch, &env))
        .unwrap();
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
    git(&second.path, &["push", "-u", "origin", &branch]);

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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let first = mgr
        .create(&request(&clone, &SourceTaskId("44".into()), &env))
        .unwrap();
    std::fs::remove_dir_all(&first.path).unwrap();
    assert!(
        git(&clone, &["worktree", "list", "--porcelain"]).contains("prunable"),
        "the registration must still be there for this test to mean anything"
    );

    let second = mgr
        .create(&request(&clone, &SourceTaskId("44".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let origin_main = git(&clone, &["rev-parse", "origin/main"]);
    // Advance the *local* main so it diverges from origin/main.
    git(&clone, &["commit", "--allow-empty", "-m", "local-only"]);
    let local_main = git(&clone, &["rev-parse", "main"]);
    assert_ne!(local_main, origin_main);

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("9".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("7".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    // Manual: never auto-remove.
    let wt = mgr
        .create(&request(&clone, &SourceTaskId("m".into()), &env))
        .unwrap();
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
    let wt2 = mgr
        .create(&request(&clone, &SourceTaskId("r".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let handles: Vec<_> = (0..6)
        .map(|i| {
            let clone = clone.clone();
            let state = state.clone();
            let mgr = mgr.clone();
            std::thread::spawn(move || {
                let env = env(&state);
                let task_id = format!("p{i}");
                mgr.create(&request(&clone, &SourceTaskId(task_id.clone()), &env))
                    .map(|w| w.path)
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

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

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("11".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("12".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("13".into()), &env))
        .unwrap();
    let branch = agent_branches(&wt.path, "feat/pushed-open-pr");
    std::fs::write(wt.path.join("work.txt"), b"the agent's output").unwrap();
    git(&wt.path, &["add", "work.txt"]);
    git(&wt.path, &["commit", "-m", "agent work"]);
    git(&wt.path, &["push", "-u", "origin", &branch]);

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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    // A human's branch, cut from the default branch as it was, and pushed.
    git(&clone, &["branch", "feat/human-work"]);
    git(&clone, &["push", "origin", "feat/human-work"]);
    // `origin/main` then moves on, so the worktree's base commit is not on the
    // human's branch.
    git(&clone, &["commit", "--allow-empty", "-m", "upstream work"]);
    git(&clone, &["push", "origin", "main"]);

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("14".into()), &env))
        .unwrap();
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

/// A row written before #694 can carry the enclosing repo's `main` as "the
/// task's branch". `main` passes both ownership tests — it descends from any
/// base commit and is all on origin — so only a name check keeps a sweep of
/// old rows from taking the local default branch out from under the clone.
#[test]
fn a_gone_worktree_never_takes_the_default_branch_with_it() {
    let base = scratch("gone_worktree_default_branch");
    let clone = setup(&base);
    let state = base.join("state");
    let env = env(&state);
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("16".into()), &env))
        .unwrap();
    std::fs::remove_dir_all(&wt.path).unwrap();
    // Off `main`, so git itself would not refuse to delete it.
    git(&clone, &["switch", "-c", "elsewhere"]);

    mgr.delete_branch_of_gone_worktree(&clone, "main", Some(&wt.base_commit))
        .unwrap();
    assert!(
        git(&clone, &["branch", "--list", "main"]).contains("main"),
        "the default branch is never a task's to delete"
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("15".into()), &env))
        .unwrap();
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("16".into()), &env))
        .unwrap();
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
            &clone,
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
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    let wt = mgr
        .create(&request(&clone, &SourceTaskId("17".into()), &env))
        .unwrap();
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

// ---------------------------------------------------------------------------
// A branch the task's source hinted at (`Task.branch_hint`, #734)
// ---------------------------------------------------------------------------

use orchestrator_core::worktree::{HintedStart, WorktreeError};

/// Stand in for whoever owns the pull request — a dependency bot, a colleague:
/// commit `content` to `f.txt` on `branch` and push it to `origin` from the
/// fixture's *other* clone, so the clone under test learns of it only by
/// fetching. `rewrite` rebuilds the branch from `main` and force-pushes, which
/// is what a bot rebasing its branch looks like from outside. Returns the head.
fn someone_pushes(base: &Path, branch: &str, content: &str, rewrite: bool) -> String {
    let seed = base.join("seed");
    if rewrite {
        git(&seed, &["switch", "-C", branch, "main"]);
    } else if git(&seed, &["branch", "--list", branch]).is_empty() {
        git(&seed, &["switch", "-c", branch, "main"]);
    } else {
        git(&seed, &["switch", branch]);
    }
    std::fs::write(seed.join("f.txt"), content).unwrap();
    git(&seed, &["add", "f.txt"]);
    git(&seed, &["commit", "-m", content]);
    git(&seed, &["push", "--force", "origin", branch]);
    git(&seed, &["rev-parse", "HEAD"])
}

fn hinted<'a>(
    clone: &'a Path,
    task_id: &'a SourceTaskId,
    hint: HintedStart<'a>,
    env: &'a HashMap<String, String>,
) -> CreateRequest<'a> {
    CreateRequest {
        hinted: Some(hint),
        ..request(clone, task_id, env)
    }
}

/// A writable stage goes **on** the hinted branch, at `origin`'s head of it,
/// and what it pushes fast-forwards the pull request rather than opening a
/// second line of history.
#[test]
fn a_hinted_branch_puts_a_writable_worktree_on_it() {
    let base = scratch("hint-on");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    let theirs = someone_pushes(&base, "renovate/x", "v1", false);

    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-1".into()),
            HintedStart::On("renovate/x"),
            &env,
        ))
        .unwrap();

    assert_eq!(wt.branch.as_deref(), Some("renovate/x"));
    assert_eq!(mgr.head_branch(&wt.path).as_deref(), Some("renovate/x"));
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), theirs);
    assert_eq!(wt.base_commit, theirs, "the work starts at their head");

    git(&wt.path, &["commit", "--allow-empty", "-m", "a fix on top"]);
    git(&wt.path, &["push", "origin", "renovate/x"]);

    let _ = std::fs::remove_dir_all(&base);
}

/// A read-only stage sees the branch's files without being on the branch: a
/// read-only worktree found on a named branch is failed as "the agent ran git"
/// (ADR-0045), so putting it there ourselves would fail every such task.
#[test]
fn a_hinted_branch_leaves_a_read_only_worktree_detached_at_its_head() {
    let base = scratch("hint-detached");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    let theirs = someone_pushes(&base, "renovate/x", "v1", false);

    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-1".into()),
            HintedStart::DetachedAt("renovate/x"),
            &env,
        ))
        .unwrap();

    assert_eq!(wt.branch, None);
    assert_eq!(
        mgr.head_branch(&wt.path),
        None,
        "detached, not on the branch"
    );
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), theirs);
    assert_ne!(theirs, git(&clone, &["rev-parse", "origin/main"]));
    assert_eq!(
        std::fs::read_to_string(wt.path.join("f.txt")).unwrap(),
        "v1"
    );
    assert!(
        git(&clone, &["branch", "--list", "renovate/x"]).is_empty(),
        "and no local branch was made on the way"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The difference from a *recorded* branch, which falls back to a detached
/// worktree when it is gone: a hint that cannot be honoured must stop the
/// task, because starting at the default branch instead is how a second pull
/// request for the same change gets opened.
#[test]
fn a_hinted_branch_missing_from_origin_is_an_error_not_a_fallback() {
    let base = scratch("hint-missing");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    for hint in [
        HintedStart::On("merged/and-deleted"),
        HintedStart::DetachedAt("merged/and-deleted"),
    ] {
        let err = mgr
            .create(&hinted(&clone, &SourceTaskId("pr-1".into()), hint, &env))
            .unwrap_err();
        assert!(
            matches!(&err, WorktreeError::HintedBranchMissing { branch } if branch == "merged/and-deleted"),
            "{hint:?}: {err}"
        );
    }
    // The lenient path is untouched: a recorded branch that vanished still
    // yields a detached worktree.
    let wt = mgr
        .create(&resume(
            &clone,
            &SourceTaskId("45".into()),
            "merged/and-deleted",
            &env,
        ))
        .unwrap();
    assert_eq!(wt.branch, None);

    let _ = std::fs::remove_dir_all(&base);
}

/// A local copy of the branch is somebody's — an old `gh pr checkout`, or this
/// task's own earlier run — so it is never reset. Behind is fast-forwarded,
/// ahead is unpushed work and left alone, diverged has no lossless answer.
#[test]
fn a_local_copy_of_the_hinted_branch_is_reconciled_without_losing_anything() {
    let base = scratch("hint-local");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());

    // Behind: they pushed again after the local copy was taken.
    someone_pushes(&base, "pr/behind", "v1", false);
    git(&clone, &["fetch", "origin"]);
    git(
        &clone,
        &["branch", "--no-track", "pr/behind", "origin/pr/behind"],
    );
    let v2 = someone_pushes(&base, "pr/behind", "v2", false);
    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("behind".into()),
            HintedStart::On("pr/behind"),
            &env,
        ))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), v2, "fast-forwarded");

    // Ahead: a commit that exists only here.
    someone_pushes(&base, "pr/ahead", "v1", false);
    git(&clone, &["fetch", "origin"]);
    git(
        &clone,
        &["switch", "--no-track", "-c", "pr/ahead", "origin/pr/ahead"],
    );
    git(&clone, &["commit", "--allow-empty", "-m", "unpushed"]);
    let unpushed = git(&clone, &["rev-parse", "HEAD"]);
    git(&clone, &["switch", "main"]);
    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("ahead".into()),
            HintedStart::On("pr/ahead"),
            &env,
        ))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), unpushed, "kept");

    // Diverged: local work on one side, their force-push on the other.
    someone_pushes(&base, "pr/diverged", "v1", false);
    git(&clone, &["fetch", "origin"]);
    git(
        &clone,
        &[
            "switch",
            "--no-track",
            "-c",
            "pr/diverged",
            "origin/pr/diverged",
        ],
    );
    git(&clone, &["commit", "--allow-empty", "-m", "local work"]);
    let local = git(&clone, &["rev-parse", "HEAD"]);
    git(&clone, &["switch", "main"]);
    someone_pushes(&base, "pr/diverged", "rebased", true);
    let err = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("diverged".into()),
            HintedStart::On("pr/diverged"),
            &env,
        ))
        .unwrap_err();
    assert!(
        matches!(&err, WorktreeError::HintedBranchDiverged { branch } if branch == "pr/diverged"),
        "{err}"
    );
    assert_eq!(
        git(&clone, &["rev-parse", "pr/diverged"]),
        local,
        "the local branch is exactly where it was"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// git gives a branch to one worktree. The error names the holder, because
/// the usual holder is another task (the issue this pull request came from,
/// kept by a retention policy) and the remedy is to continue *that* task.
#[test]
fn a_hinted_branch_held_by_another_worktree_is_an_error_naming_it() {
    let base = scratch("hint-held");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    someone_pushes(&base, "feat/x", "v1", false);

    let first = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("issue-10".into()),
            HintedStart::On("feat/x"),
            &env,
        ))
        .unwrap();
    let err = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-12".into()),
            HintedStart::On("feat/x"),
            &env,
        ))
        .unwrap_err();
    match &err {
        WorktreeError::HintedBranchHeld { branch, holder } => {
            assert_eq!(branch, "feat/x");
            assert_eq!(
                holder.canonicalize().unwrap(),
                first.path.canonicalize().unwrap()
            );
        }
        other => panic!("expected HintedBranchHeld, got {other}"),
    }
    // A read-only stage takes no branch, so it is not in anyone's way.
    mgr.create(&hinted(
        &clone,
        &SourceTaskId("pr-12".into()),
        HintedStart::DetachedAt("feat/x"),
        &env,
    ))
    .unwrap();

    // The operator's own checkout counts too: a `gh pr checkout` they are
    // still sitting on holds the branch exactly as a task's worktree does.
    someone_pushes(&base, "feat/y", "v1", false);
    git(&clone, &["fetch", "origin"]);
    git(
        &clone,
        &["switch", "--no-track", "-c", "feat/y", "origin/feat/y"],
    );
    let err = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-13".into()),
            HintedStart::On("feat/y"),
            &env,
        ))
        .unwrap_err();
    assert!(
        matches!(&err, WorktreeError::HintedBranchHeld { holder, .. }
            if holder.canonicalize().unwrap() == clone.canonicalize().unwrap()),
        "{err}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A dispatch reuses a worktree that is still on disk and never calls
/// `create` — where a hint is applied. So a task handed from a read-only stage
/// to a writable one would start the writable stage detached, at whatever
/// commit the first stage saw. `sync_to_hint` is what closes that.
#[test]
fn sync_moves_a_surviving_worktree_to_its_hinted_start() {
    let base = scratch("hint-sync");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    let v1 = someone_pushes(&base, "renovate/x", "v1", false);

    // The design stage.
    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-1".into()),
            HintedStart::DetachedAt("renovate/x"),
            &env,
        ))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), v1);

    // They push again; a second design dispatch must not read stale code.
    let v2 = someone_pushes(&base, "renovate/x", "v2", false);
    mgr.sync_to_hint(&clone, &wt.path, HintedStart::DetachedAt("renovate/x"))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), v2);
    assert_eq!(mgr.head_branch(&wt.path), None, "still detached");

    // The card moves to the implement column: same task, same worktree.
    mgr.sync_to_hint(&clone, &wt.path, HintedStart::On("renovate/x"))
        .unwrap();
    assert_eq!(mgr.head_branch(&wt.path).as_deref(), Some("renovate/x"));
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), v2);

    // Nothing moved: a no-op that leaves the agent's unpushed commit alone.
    git(&wt.path, &["commit", "--allow-empty", "-m", "agent's fix"]);
    let fix = git(&wt.path, &["rev-parse", "HEAD"]);
    mgr.sync_to_hint(&clone, &wt.path, HintedStart::On("renovate/x"))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), fix);

    // Pushed, and then they push on top: the worktree is behind on its own
    // branch, which can only be fast-forwarded from inside it.
    git(&wt.path, &["push", "origin", "renovate/x"]);
    let seed = base.join("seed");
    git(&seed, &["pull", "origin", "renovate/x"]);
    let v3 = someone_pushes(&base, "renovate/x", "v3", false);
    mgr.sync_to_hint(&clone, &wt.path, HintedStart::On("renovate/x"))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), v3);

    let _ = std::fs::remove_dir_all(&base);
}

/// The one thing a sync never does is discard. Uncommitted changes that the
/// switch would overwrite stop the task, with the worktree named, and are
/// still there afterwards.
#[test]
fn sync_refuses_to_overwrite_uncommitted_changes() {
    let base = scratch("hint-sync-dirty");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    someone_pushes(&base, "renovate/x", "v1", false);
    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-1".into()),
            HintedStart::DetachedAt("renovate/x"),
            &env,
        ))
        .unwrap();
    std::fs::write(wt.path.join("f.txt"), "notes the agent left").unwrap();
    someone_pushes(&base, "renovate/x", "v2", false);

    let err = mgr
        .sync_to_hint(&clone, &wt.path, HintedStart::On("renovate/x"))
        .unwrap_err();
    assert!(
        matches!(&err, WorktreeError::HintSyncBlocked { path, branch, .. }
            if path == &wt.path && branch == "renovate/x"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(wt.path.join("f.txt")).unwrap(),
        "notes the agent left"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The case `HintedBranchMissing`'s own message names: the pull request was
/// merged and its branch deleted **after this clone had fetched it**. A plain
/// `git fetch origin` does not prune, so `refs/remotes/origin/<branch>` lives
/// on and would answer for a branch that is gone — the task would rebuild the
/// dead branch from a stale commit and resurrect it on push.
#[test]
fn a_hinted_branch_deleted_on_origin_after_being_fetched_is_missing() {
    let base = scratch("hint-deleted");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    someone_pushes(&base, "renovate/x", "v1", false);
    git(&clone, &["fetch", "origin"]);
    git(
        &base.join("seed"),
        &["push", "origin", "--delete", "renovate/x"],
    );
    git(&clone, &["fetch", "origin"]);
    assert!(
        !git(&clone, &["branch", "-r", "--list", "origin/renovate/x"]).is_empty(),
        "the stale remote-tracking ref is still here — that is the hazard"
    );

    for hint in [
        HintedStart::On("renovate/x"),
        HintedStart::DetachedAt("renovate/x"),
    ] {
        let err = mgr
            .create(&hinted(&clone, &SourceTaskId("pr-1".into()), hint, &env))
            .unwrap_err();
        assert!(
            matches!(&err, WorktreeError::HintedBranchMissing { branch } if branch == "renovate/x"),
            "{hint:?}: {err}"
        );
    }

    let _ = std::fs::remove_dir_all(&base);
}

/// `sync_to_hint` runs `git switch` **inside** the recorded path, and a plain
/// directory under a repository answers git as the *enclosing* repository
/// (#694). The task row keeps its path after the worktree is removed, so a
/// directory that reappears there — leftovers, a `mkdir` — would get the
/// operator's own checkout switched out from under them.
#[test]
fn sync_refuses_a_path_that_is_not_this_repositorys_worktree() {
    let base = scratch("hint-sync-unregistered");
    let clone = setup(&base);
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    someone_pushes(&base, "renovate/x", "v1", false);
    let stray = clone.join("leftovers");
    std::fs::create_dir_all(&stray).unwrap();
    let before = git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]);

    let err = mgr
        .sync_to_hint(&clone, &stray, HintedStart::On("renovate/x"))
        .unwrap_err();
    assert!(
        matches!(&err, WorktreeError::HintedWorktreeUnregistered { path } if path == &stray),
        "{err}"
    );
    assert_eq!(
        git(&clone, &["rev-parse", "--abbrev-ref", "HEAD"]),
        before,
        "the enclosing checkout was not touched"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A plan stage is *meant* to make no commits, and nothing enforces that for a
/// workflow with no profile. Commits made on the detached `HEAD` are reachable
/// from no ref, so moving `HEAD` away would leave them to the reflog.
#[test]
fn sync_refuses_to_leave_detached_commits_behind() {
    let base = scratch("hint-sync-detached-commits");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    someone_pushes(&base, "renovate/x", "v1", false);
    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-1".into()),
            HintedStart::DetachedAt("renovate/x"),
            &env,
        ))
        .unwrap();
    git(
        &wt.path,
        &["commit", "--allow-empty", "-m", "made while detached"],
    );
    let orphan = git(&wt.path, &["rev-parse", "HEAD"]);

    let err = mgr
        .sync_to_hint(&clone, &wt.path, HintedStart::On("renovate/x"))
        .unwrap_err();
    assert!(
        matches!(&err, WorktreeError::HintSyncBlocked { .. }),
        "{err}"
    );
    assert!(
        err.to_string().contains(&orphan[..12]) && err.to_string().contains("no branch"),
        "the error must name the commit that would be lost: {err}"
    );
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), orphan, "still there");

    let _ = std::fs::remove_dir_all(&base);
}

/// The guard above must not mistake a **stale base** for the agent's work. A
/// dependency bot rebases its branch as a matter of routine; the commit a
/// design stage was detached at then hangs off no ref at all, exactly like a
/// commit made while detached — but nobody made it here, and refusing to move
/// would wedge every such task on the bot's next rebase.
#[test]
fn sync_follows_a_force_push_away_from_a_stale_detached_base() {
    let base = scratch("hint-sync-force-push");
    let clone = setup(&base);
    let env = env(&base.join("state"));
    let mgr = WorktreeManager::new(SystemGitRunner::default());
    someone_pushes(&base, "renovate/x", "v1", false);
    let wt = mgr
        .create(&hinted(
            &clone,
            &SourceTaskId("pr-1".into()),
            HintedStart::DetachedAt("renovate/x"),
            &env,
        ))
        .unwrap();
    let rebased = someone_pushes(&base, "renovate/x", "rebased", true);

    mgr.sync_to_hint(&clone, &wt.path, HintedStart::DetachedAt("renovate/x"))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), rebased);

    let again = someone_pushes(&base, "renovate/x", "rebased again", true);
    mgr.sync_to_hint(&clone, &wt.path, HintedStart::On("renovate/x"))
        .unwrap();
    assert_eq!(git(&wt.path, &["rev-parse", "HEAD"]), again);

    let _ = std::fs::remove_dir_all(&base);
}
