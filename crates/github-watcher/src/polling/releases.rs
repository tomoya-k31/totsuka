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
use totsuka_core::{Clock, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct ReleasesLoopConfig {
    pub poll_interval: Duration,
    pub catchup_window: chrono::Duration,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_releases_loop(
    pool: PgPool,
    publisher: Arc<Publisher>,
    client: Arc<dyn GhClient>,
    tracker: RepoTracker,
    clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: ReleasesLoopConfig,
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
                        tracing::warn!(repo=%repo, error=%e, "releases poll failed");
                        health.set_check("github_releases", &format!("fail: {e}")).await;
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
    cfg: &ReleasesLoopConfig,
    clock: &Arc<dyn Clock>,
) -> Result<(), WatcherError> {
    let key = CursorKey::releases(&repo.to_string());
    let since = match get(pool, &key).await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| WatcherError::Internal(format!("bad release cursor: {e}")))?,
        None => clock.now() - cfg.catchup_window,
    };
    let releases = client.releases_since(repo, since).await?;
    let mut high_water = since;
    for rel in releases {
        let event_key = format!("gh:release:{}", rel.node_id);
        let payload = serde_json::json!({ "repo": rel.repo.to_string(), "tag": rel.tag });
        let ev = DomainEvent {
            event_key,
            source: Source::Github,
            event_type: "github.release_published".into(),
            payload,
        };
        publisher
            .send(pool, ev, None)
            .await
            .map_err(WatcherError::Bus)?;
        if rel.published_at > high_water {
            high_water = rel.published_at;
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
