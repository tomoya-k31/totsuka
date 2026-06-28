use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::repo::ReloadReport;
use crate::server::AppState;
use totsuka_config::schema::AgentAdapterSection;

/// Programmatic reload entry, used by both the HTTP route and the SIGHUP
/// handler (Task 17). Returns the diff so callers can log + notify.
pub fn apply_reload(state: &AppState, cfg: &AgentAdapterSection) -> ReloadReport {
    state.repos.reload(cfg)
}

#[derive(Serialize)]
pub struct ReloadResponse {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// `POST /v1/repos/reload`. Body: ignored. Reads the config file from
/// the env `TOTSUKA_CONFIG` (same env the bin used at startup). Errors map
/// to RFC7807 via AdapterError::Internal.
pub async fn reload(
    State(s): State<AppState>,
) -> Result<Json<ReloadResponse>, crate::error::AdapterError> {
    let path =
        std::env::var("TOTSUKA_CONFIG").unwrap_or_else(|_| "~/.config/totsuka/config.toml".into());
    let cfg = totsuka_config::Config::load(&path)
        .map_err(|e| crate::error::AdapterError::Internal(format!("reload config: {e}")))?;
    let report = apply_reload(&s, &cfg.agent_adapter);
    Ok(Json(ReloadResponse {
        added: report
            .added
            .iter()
            .map(|k| k.as_str().to_string())
            .collect(),
        removed: report
            .removed
            .iter()
            .map(|k| k.as_str().to_string())
            .collect(),
    }))
}
