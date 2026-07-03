//! Real git operations against a tempdir-backed bare repo. Skips if `git`
//! is not on PATH (CI always has it).

use agent_adapter::repo::RepoEntry;
use agent_adapter::worktree::WorktreeManager;
use tokio::process::Command;

async fn init_repo() -> (tempfile::TempDir, RepoEntry) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_path = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_path).unwrap();
    let run = |args: &[&str]| {
        let repo_path = repo_path.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        async move {
            let out = Command::new("git")
                .current_dir(&repo_path)
                .args(args)
                .output()
                .await
                .expect("spawn git");
            assert!(out.status.success(), "git failed: {:?}", out);
        }
    };
    run(&["init", "-b", "main"]).await;
    run(&["config", "user.email", "t@example.com"]).await;
    run(&["config", "user.name", "Test"]).await;
    run(&["config", "commit.gpgsign", "false"]).await;
    run(&["commit", "--allow-empty", "-m", "init"]).await;

    let worktree_root = tmp.path().join("worktrees");
    let entry = RepoEntry {
        description: "t".into(),
        repo_path,
        worktree_root,
    };
    (tmp, entry)
}

#[tokio::test]
async fn create_then_list_then_remove() {
    let _git = std::process::Command::new("git").arg("--version").output();
    let (_tmp, entry) = init_repo().await;

    let m = WorktreeManager::new();
    let path = m
        .create(&entry, "totsuka/aaaaaaaaaaaa/design", false)
        .await
        .unwrap();
    assert!(path.exists());

    let records = m.list(&entry).await.unwrap();
    let found = records
        .iter()
        .any(|r| r.branch.as_deref() == Some("totsuka/aaaaaaaaaaaa/design"));
    assert!(found, "created branch not in list: {:?}", records);

    m.remove(&entry, "totsuka/aaaaaaaaaaaa/design")
        .await
        .unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn create_returns_worktree_in_use_when_branch_already_has_one() {
    let (_tmp, entry) = init_repo().await;
    let m = WorktreeManager::new();
    m.create(&entry, "totsuka/aaaaaaaaaaaa/design", false)
        .await
        .unwrap();
    let err = m
        .create(&entry, "totsuka/aaaaaaaaaaaa/design", false)
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("worktree in use"), "got: {msg}");
}

/// A re-spawn of the same phase lands on the same worktree path (branch
/// naming is generation-independent): the leftover worktree from the
/// previous run must be replaced, not fail the spawn.
#[tokio::test]
async fn create_replaces_leftover_worktree_at_same_path() {
    let (tmp, entry) = init_repo().await;
    let m = WorktreeManager::new();
    let first = m
        .create(&entry, "totsuka/aaaaaaaaaaaa/design", true)
        .await
        .unwrap();
    // Leave a marker to prove the second create really replaced it.
    std::fs::write(first.join("stale-marker"), "old run").unwrap();

    let second = m
        .create(&entry, "totsuka/aaaaaaaaaaaa/design", true)
        .await
        .expect("re-spawn must tolerate the leftover worktree");
    assert_eq!(first, second);
    assert!(
        !second.join("stale-marker").exists(),
        "the leftover worktree must be replaced by a fresh checkout"
    );
    drop(tmp);
}
