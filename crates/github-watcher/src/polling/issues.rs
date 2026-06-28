use super::RepoTracker;
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::GhClient;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::Publisher;
use totsuka_core::{event_key_gh_issue, Clock, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct IssuesLoopConfig {
    pub poll_interval: Duration,
    pub catchup_window: chrono::Duration,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_issues_loop(
    pool: PgPool,
    publisher: Arc<Publisher>,
    client: Arc<dyn GhClient>,
    tracker: RepoTracker,
    clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: IssuesLoopConfig,
    shutdown: CancellationToken,
) -> Result<(), WatcherError> {
    let mut interval = tokio::time::interval(cfg.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                for repo in tracker.snapshot().await {
                    if let Err(e) = poll_repo(&pool, &publisher, &client, &repo, &cfg, &clock).await {
                        tracing::warn!(repo=%repo, error=%e, "issues poll failed");
                        health.set_check("github_issues", &format!("fail: {e}")).await;
                    }
                }
            }
        }
    }
}

async fn poll_repo(
    pool: &PgPool,
    publisher: &Publisher,
    client: &Arc<dyn GhClient>,
    repo: &crate::gh_client::RepoSlug,
    cfg: &IssuesLoopConfig,
    clock: &Arc<dyn Clock>,
) -> Result<(), WatcherError> {
    let key = CursorKey::issues(&repo.to_string());
    let since = match get(pool, &key).await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| WatcherError::Internal(format!("bad issue cursor: {e}")))?,
        None => clock.now() - cfg.catchup_window,
    };
    let issues = client.issues_since(repo, since).await?;
    let mut high_water = since;
    for u in issues {
        let event_key = event_key_gh_issue(&u.node_id, u.updated_at.timestamp_millis());
        let payload = serde_json::json!({
            "issue_node_id": u.node_id,
            "repo": u.repo.to_string(),
            "number": u.number,
            "state": u.state,
            "updated_at": u.updated_at,
        });
        let ev = DomainEvent {
            event_key,
            source: Source::Github,
            event_type: "github.issue_updated".into(),
            payload,
        };
        publisher
            .send(pool, ev, None)
            .await
            .map_err(WatcherError::Bus)?;
        if u.updated_at > high_water {
            high_water = u.updated_at;
        }
    }
    if high_water > since {
        set(
            pool,
            &key,
            &high_water.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .await?;
    }
    Ok(())
}
