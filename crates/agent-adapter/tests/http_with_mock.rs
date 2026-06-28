use agent_adapter::herdr::mock::MockHerdr;
use agent_adapter::repo::{RepoKey, RepoRegistry};
use agent_adapter::server::{router, AppState};
use agent_adapter::worktree::WorktreeManager;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::collections::HashMap;
use std::sync::Arc;
use totsuka_config::schema::{AgentAdapterSection, RepoSection};
use totsuka_core::SystemClock;
use totsuka_telemetry::HealthState;
use tower::ServiceExt;

fn app() -> axum::Router {
    let state = AppState {
        herdr: Arc::new(MockHerdr::new()),
        repos: Arc::new(RepoRegistry::new()),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };
    router(state)
}

#[tokio::test]
async fn healthz_returns_ok_through_adapter_router() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn unknown_v1_path_returns_404() {
    let res = app()
        .oneshot(
            Request::builder()
                .uri("/v1/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

fn cfg_with_repo(repo_path: &str, worktree_root: &str) -> AgentAdapterSection {
    AgentAdapterSection {
        uds_path: "/tmp/u".into(),
        tcp_bind: String::new(),
        herdr_socket: "/tmp/h".into(),
        node_capacity: 8,
        repos_root: "/unused".into(),
        auto_clone: false,
        worktree_failed_ttl_hours: 72,
        worktree_orphan_scan_interval_secs: 3600,
        vars: HashMap::new(),
        repos: HashMap::from_iter([(
            "x/y".to_string(),
            RepoSection {
                description: "test".into(),
                repo_path: Some(repo_path.into()),
                worktree_subdir: None,
                worktree_path: Some(worktree_root.into()),
                default_branch: Some("main".into()),
            },
        )]),
    }
}

async fn app_with_real_git() -> (tempfile::TempDir, axum::Router, Arc<MockHerdr>) {
    use tokio::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let r = repo.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        async move {
            assert!(Command::new("git")
                .current_dir(&r)
                .args(args)
                .output()
                .await
                .unwrap()
                .status
                .success());
        }
    };
    run(&["init", "-b", "main"]).await;
    run(&["config", "commit.gpgsign", "false"]).await;
    run(&["config", "user.email", "t@example.com"]).await;
    run(&["config", "user.name", "Test"]).await;
    run(&["commit", "--allow-empty", "-m", "init"]).await;

    let repos = RepoRegistry::new();
    repos.reload(&cfg_with_repo(repo.to_str().unwrap(), wt.to_str().unwrap()));
    let herdr = Arc::new(MockHerdr::new());
    let state = AppState {
        herdr: herdr.clone(),
        repos: Arc::new(repos),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };
    (tmp, router(state), herdr)
}

#[tokio::test]
async fn spawn_happy_path() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    let body = serde_json::json!({
        "task_id": "PVTI_abc",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/abcdefabcdef/design",
        "argv": ["claude", "--model", "x"],
        "env": {"CLAUDE_TOKEN": "tk_x"}
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 201);
    assert_eq!(herdr.count(), 1);

    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["agent_id"].as_str().unwrap().starts_with("ag_"));
    assert!(v["worktree_path"]
        .as_str()
        .unwrap()
        .contains("totsuka__abcdefabcdef__design"));
}

#[tokio::test]
async fn spawn_rejects_argv_with_token_flag() {
    let (_tmp, app, _herdr) = app_with_real_git().await;
    let body = serde_json::json!({
        "task_id": "PVTI_abc",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/abcdefabcdef/design",
        "argv": ["claude", "--api-token", "tk_x"],
        "env": {}
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["type"], "/errors/argv_secret_violation");
}

#[tokio::test]
async fn spawn_unknown_repo_returns_404() {
    let (_tmp, app, _herdr) = app_with_real_git().await;
    let body = serde_json::json!({
        "task_id": "PVTI_abc",
        "phase": "design",
        "attempt": 0,
        "repo": "no/such",
        "branch": "totsuka/abcdefabcdef/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn send_round_trip() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    // Spawn first.
    let spawn_body = serde_json::json!({
        "task_id": "PVTI_x",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/zzzzzzzzzzzz/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(spawn_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = v["agent_id"].as_str().unwrap();

    // Send a message.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/agents/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    let _ = herdr;
}

#[tokio::test]
async fn send_to_unknown_agent_returns_404() {
    let (_tmp, app, _) = app_with_real_git().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents/nope/messages")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn output_returns_revision_and_text() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    let spawn_body = serde_json::json!({
        "task_id": "PVTI_x",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/yyyyyyyyyyyy/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(spawn_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let id = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["agent_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Simulate two "send" updates so revision is > 0.
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/agents/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"foo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/agents/{id}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"text":"bar"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Read snapshot with since_revision=1, expect is_newer=true.
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/v1/agents/{id}/output?since_revision=1"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["revision"], 2);
    assert_eq!(v["text"], "foobar");
    assert_eq!(v["is_newer"], true);
    let _ = herdr;
}

#[tokio::test]
async fn stop_closes_pane_and_removes_worktree() {
    let (_tmp, app, herdr) = app_with_real_git().await;
    let spawn_body = serde_json::json!({
        "task_id": "PVTI_x",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/qqqqqqqqqqqq/design",
        "argv": [],
        "env": {}
    });
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(spawn_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = v["agent_id"].as_str().unwrap().to_string();
    let worktree = v["worktree_path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&worktree).exists());

    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agents/{id}"))
                .header("x-totsuka-branch", "totsuka/qqqqqqqqqqqq/design")
                .header("x-totsuka-repo", "x/y")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 204);
    assert_eq!(herdr.count(), 0);
    assert!(!std::path::Path::new(&worktree).exists());
}

async fn app_with_real_git_and_state() -> (tempfile::TempDir, axum::Router, Arc<MockHerdr>, AppState)
{
    use tokio::process::Command;
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&repo).unwrap();
    let run = |args: &[&str]| {
        let r = repo.clone();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        async move {
            assert!(Command::new("git")
                .current_dir(&r)
                .args(args)
                .output()
                .await
                .unwrap()
                .status
                .success());
        }
    };
    run(&["init", "-b", "main"]).await;
    run(&["config", "commit.gpgsign", "false"]).await;
    run(&["config", "user.email", "t@example.com"]).await;
    run(&["config", "user.name", "Test"]).await;
    run(&["commit", "--allow-empty", "-m", "init"]).await;

    let repos = Arc::new(RepoRegistry::new());
    repos.reload(&cfg_with_repo(repo.to_str().unwrap(), wt.to_str().unwrap()));
    let herdr = Arc::new(MockHerdr::new());
    let state = AppState {
        herdr: herdr.clone(),
        repos: repos.clone(),
        worktrees: Arc::new(WorktreeManager::new()),
        clock: Arc::new(SystemClock),
        health: HealthState::new(),
    };
    let app = router(state.clone());
    (tmp, app, herdr, state)
}

#[tokio::test]
async fn gc_removes_orphan_worktree() {
    use agent_adapter::gc::gc_tick;
    let (_tmp, app, herdr, state) = app_with_real_git_and_state().await;

    // Spawn one agent (worktree + live in herdr).
    let body = serde_json::json!({
        "task_id": "PVTI_keep",
        "phase": "design",
        "attempt": 0,
        "repo": "x/y",
        "branch": "totsuka/keepbranchaaa/design",
        "argv": [],
        "env": {}
    });
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/agents")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let entry = state
        .repos
        .resolve(&RepoKey::new("x/y".into()))
        .expect("repo present");
    let orphan_path = state
        .worktrees
        .create(&entry, "totsuka/orphanbranchx/design")
        .await
        .unwrap();
    assert!(orphan_path.exists());
    let _ = herdr; // already wired through state

    // Run a GC tick.
    let report = gc_tick(&state).await;
    assert!(report.removed >= 1, "no orphan removed: {report:?}");
    assert!(!orphan_path.exists());
}

#[tokio::test]
async fn apply_reload_reports_added_repos() {
    use agent_adapter::server::reload::apply_reload;
    let (_tmp, _app, _herdr) = app_with_real_git().await;
    let repos = std::sync::Arc::new(RepoRegistry::new());
    repos.reload(&cfg_with_repo("/tmp/a", "/tmp/wta"));
    let state = AppState {
        herdr: std::sync::Arc::new(MockHerdr::new()),
        repos: repos.clone(),
        worktrees: std::sync::Arc::new(WorktreeManager::new()),
        clock: std::sync::Arc::new(totsuka_core::SystemClock),
        health: totsuka_telemetry::HealthState::new(),
    };

    let mut new_cfg = cfg_with_repo("/tmp/a", "/tmp/wta");
    new_cfg.repos.insert(
        "x/b".into(),
        totsuka_config::schema::RepoSection {
            description: "B".into(),
            repo_path: None,
            worktree_subdir: Some(".w".into()),
            worktree_path: None,
            default_branch: None,
        },
    );
    let report = apply_reload(&state, &new_cfg);
    assert_eq!(report.added, vec![RepoKey::new("x/b".into())]);
    assert!(report.removed.is_empty());
}
