use agent_adapter::herdr::mock::MockHerdr;
use agent_adapter::repo::RepoRegistry;
use agent_adapter::server::{router, AppState};
use agent_adapter::worktree::WorktreeManager;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use std::sync::Arc;
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
