//! HTTP surface. Mounts the totsuka-telemetry healthz/readyz/metrics router
//! and adds the adapter's `/v1/*` routes (see sibling modules added by
//! later tasks).

use axum::Router;
use std::sync::Arc;

use crate::herdr::HerdrClient;
use crate::repo::RepoRegistry;
use crate::worktree::WorktreeManager;
use totsuka_core::Clock;
use totsuka_telemetry::HealthState;

#[derive(Clone)]
pub struct AppState {
    pub herdr: Arc<dyn HerdrClient>,
    pub repos: Arc<RepoRegistry>,
    pub worktrees: Arc<WorktreeManager>,
    pub clock: Arc<dyn Clock>,
    pub health: HealthState,
}

/// Build the complete adapter router. Telemetry routes (healthz / readyz /
/// metrics + request_id middleware) are nested first; `/v1/*` routes are
/// added by subsequent tasks via `with_v1_routes`. The request_id middleware
/// is also applied at the top level so `/v1/*` gets the same propagation
/// (spec §11.6); foundation's inner layer on healthz/readyz reuses the
/// header the outer one set, so double-application is idempotent.
pub fn router(state: AppState) -> Router {
    let health = totsuka_telemetry::http::router(state.health.clone());
    let v1 = with_v1_routes(Router::new(), state.clone());
    Router::new()
        .merge(health)
        .nest("/v1", v1)
        .layer(axum::middleware::from_fn(
            totsuka_telemetry::request_id::middleware,
        ))
}

pub mod list;
pub mod output;
pub mod reload;
pub mod send;
pub mod spawn;
pub mod stop;

/// Tasks 11–15 each add their handler here. Kept as a single fn so reviewers
/// see all `/v1/*` routes at a glance.
pub fn with_v1_routes(r: Router, state: AppState) -> Router {
    use axum::routing::{delete, get, post};
    let stateful = Router::new()
        .route("/agents", post(spawn::spawn).get(list::list))
        .route("/agents/:id", delete(stop::stop))
        .route("/agents/:id/messages", post(send::send))
        .route("/agents/:id/output", get(output::output))
        .route("/repos/reload", post(reload::reload))
        .with_state(state);
    r.merge(stateful)
}
