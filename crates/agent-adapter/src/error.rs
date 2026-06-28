//! Adapter-specific errors. Map to RFC7807 Problem responses on the HTTP
//! layer per spec §11.6.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("repo not registered: {0}")]
    RepoNotRegistered(String),
    #[error("worktree in use: {0}")]
    WorktreeInUse(String),
    #[error("capacity full")]
    CapacityFull,
    #[error("argv contains secret-like flag: {0}")]
    ArgvSecretViolation(String),
    #[error("herdr unavailable: {0}")]
    HerdrUnavailable(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl AdapterError {
    pub fn code(&self) -> &'static str {
        match self {
            AdapterError::RepoNotRegistered(_) => "/errors/repo_not_registered",
            AdapterError::WorktreeInUse(_) => "/errors/worktree_in_use",
            AdapterError::CapacityFull => "/errors/capacity_full",
            AdapterError::ArgvSecretViolation(_) => "/errors/argv_secret_violation",
            AdapterError::HerdrUnavailable(_) => "/errors/herdr_unavailable",
            AdapterError::NotFound(_) => "/errors/not_found",
            AdapterError::Internal(_) => "/errors/internal",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AdapterError::RepoNotRegistered(_) => StatusCode::NOT_FOUND,
            AdapterError::NotFound(_) => StatusCode::NOT_FOUND,
            AdapterError::WorktreeInUse(_) => StatusCode::CONFLICT,
            AdapterError::CapacityFull => StatusCode::CONFLICT,
            AdapterError::ArgvSecretViolation(_) => StatusCode::BAD_REQUEST,
            AdapterError::HerdrUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AdapterError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    ty: &'a str,
    title: &'a str,
    status: u16,
    detail: String,
}

impl IntoResponse for AdapterError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Problem {
            ty: self.code(),
            title: self.code().trim_start_matches("/errors/"),
            status: status.as_u16(),
            detail: self.to_string(),
        };
        let mut resp = (status, Json(body)).into_response();
        resp.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_in_use_maps_to_409() {
        let e = AdapterError::WorktreeInUse("totsuka/abc/design".into());
        assert_eq!(e.status(), StatusCode::CONFLICT);
        assert_eq!(e.code(), "/errors/worktree_in_use");
    }

    #[test]
    fn argv_violation_maps_to_400() {
        let e = AdapterError::ArgvSecretViolation("--token".into());
        assert_eq!(e.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn into_response_has_problem_json_content_type() {
        use axum::body::to_bytes;
        let resp = AdapterError::CapacityFull.into_response();
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .unwrap(),
            "application/problem+json"
        );
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["type"], "/errors/capacity_full");
        assert_eq!(body["status"], 409);
    }
}
