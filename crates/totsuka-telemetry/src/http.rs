use std::{collections::HashMap, sync::Arc};

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};
use serde::Serialize;
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub struct HealthState {
    inner: Arc<RwLock<HealthInner>>,
}

#[derive(Default)]
struct HealthInner {
    ready: bool,
    checks: HashMap<String, String>, // name -> "ok" / "fail: <msg>"
}

impl HealthState {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn set_check(&self, name: &str, status: &str) {
        self.inner
            .write()
            .await
            .checks
            .insert(name.into(), status.into());
    }

    pub async fn set_ready(&self, ready: bool) {
        self.inner.write().await.ready = ready;
    }
}

#[derive(Serialize)]
struct ReadyResponse<'a> {
    ready: bool,
    checks: &'a HashMap<String, String>,
}

pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics_stub))
        .with_state(state)
        .layer(axum::middleware::from_fn(crate::request_id::middleware))
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(s): State<HealthState>) -> impl IntoResponse {
    let g = s.inner.read().await;
    let body = serde_json::to_string(&ReadyResponse {
        ready: g.ready,
        checks: &g.checks,
    })
    .unwrap();
    let code = if g.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
}

async fn metrics_stub() -> impl IntoResponse {
    // 実装は D3 で差し替え
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/plain")],
        "# HELP placeholder\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = router(HealthState::new());
        let res = app
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
    async fn readyz_starts_not_ready() {
        let st = HealthState::new();
        let app = router(st.clone());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_ok_after_set_ready() {
        let st = HealthState::new();
        st.set_ready(true).await;
        st.set_check("db", "ok").await;
        let app = router(st.clone());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn request_id_echoed() {
        let app = router(HealthState::new());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .header(crate::request_id::HEADER, "test-id-1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.headers().get(crate::request_id::HEADER).unwrap(),
            "test-id-1"
        );
    }
}
