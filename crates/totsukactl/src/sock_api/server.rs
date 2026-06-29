use super::dto::{ProcessDto, ShutdownReq};
use crate::registry::Registry;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct SockApiState {
    pub registry: Arc<Registry>,
    pub control_tx: mpsc::Sender<ControlMsg>,
}

#[derive(Debug, Clone)]
pub enum ControlMsg {
    Restart(String),
    Reload(String),
    Shutdown { postgres: bool, force: bool },
}

pub fn router(state: SockApiState) -> Router {
    Router::new()
        .route("/v1/processes", get(list))
        .route("/v1/processes/:name/restart", post(restart))
        .route("/v1/processes/:name/reload", post(reload))
        .route("/v1/shutdown", post(shutdown))
        .with_state(state)
}

async fn list(State(s): State<SockApiState>) -> impl IntoResponse {
    let entries = s.registry.list().await;
    let dto: Vec<ProcessDto> = entries.into_iter().map(Into::into).collect();
    (StatusCode::OK, Json(dto))
}

async fn restart(State(s): State<SockApiState>, Path(name): Path<String>) -> impl IntoResponse {
    if !known(&name) {
        return rfc7807(StatusCode::NOT_FOUND, "/errors/unknown_child", &name);
    }
    let _ = s.control_tx.send(ControlMsg::Restart(name.clone())).await;
    (StatusCode::ACCEPTED, Json(json!({ "queued": true, "name": name }))).into_response()
}

async fn reload(State(s): State<SockApiState>, Path(name): Path<String>) -> impl IntoResponse {
    if name != "agent-adapter" {
        return rfc7807(StatusCode::BAD_REQUEST, "/errors/not_reloadable", &name);
    }
    let _ = s.control_tx.send(ControlMsg::Reload(name.clone())).await;
    (StatusCode::ACCEPTED, Json(json!({ "queued": true, "name": name }))).into_response()
}

async fn shutdown(
    State(s): State<SockApiState>,
    Json(req): Json<ShutdownReq>,
) -> impl IntoResponse {
    let _ = s
        .control_tx
        .send(ControlMsg::Shutdown {
            postgres: req.postgres,
            force: req.force,
        })
        .await;
    (StatusCode::ACCEPTED, Json(json!({ "accepted": true }))).into_response()
}

fn known(name: &str) -> bool {
    crate::registry::ORDER.contains(&name)
}

fn rfc7807(code: StatusCode, type_uri: &str, detail: &str) -> axum::response::Response {
    (
        code,
        [(axum::http::header::CONTENT_TYPE, "application/problem+json")],
        Json(json!({
            "type": type_uri,
            "title": code.canonical_reason().unwrap_or("error"),
            "status": code.as_u16(),
            "detail": detail,
        })),
    )
        .into_response()
}
