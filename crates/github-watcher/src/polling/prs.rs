use super::RepoTracker;
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::{GhClient, PrUpdate};
use crate::linkage::resolve_task_id;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::Publisher;
use totsuka_core::{Clock, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct PrsLoopConfig {
    pub poll_interval: Duration,
    pub catchup_window: chrono::Duration,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_prs_loop(
    pool: PgPool,
    publisher: Arc<Publisher>,
    client: Arc<dyn GhClient>,
    tracker: RepoTracker,
    clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: PrsLoopConfig,
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
                        tracing::warn!(repo=%repo, error=%e, "prs poll failed");
                        health.set_check("github_prs", &format!("fail: {e}")).await;
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
    cfg: &PrsLoopConfig,
    clock: &Arc<dyn Clock>,
) -> Result<(), WatcherError> {
    let key = CursorKey::prs(&repo.to_string());
    let since: DateTime<Utc> = match get(pool, &key).await? {
        Some(s) => DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| WatcherError::Internal(format!("bad pr cursor: {e}")))?,
        None => clock.now() - cfg.catchup_window,
    };
    let prs = client.prs_since(repo, since).await?;
    let mut high_water = since;
    for pr in prs {
        if let Some(merged_at) = pr.merged_at {
            if merged_at > since {
                publish_pr_merged(pool, publisher, &pr).await?;
            }
        }
        // TODO: verification events (check-runs) will be emitted by a separate loop
        if pr.updated_at > high_water {
            high_water = pr.updated_at;
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

async fn publish_pr_merged(
    pool: &PgPool,
    publisher: &Publisher,
    pr: &PrUpdate,
) -> Result<(), WatcherError> {
    let task_id = resolve_task_id(pool, &pr.head_ref, pr.body.as_deref()).await?;
    let event_key = format!(
        "gh:pr:{}:{}:pr_merged",
        pr.node_id,
        pr.merged_at.unwrap_or(pr.updated_at).timestamp_millis(),
    );
    let payload = serde_json::json!({
        "item_id":    task_id.clone().unwrap_or_default(),
        "pr_node_id": pr.node_id,
        "repo":       pr.repo.to_string(),
        "pr_number":  pr.number,
        "pr_diff":    "",
    });
    if task_id.is_none() {
        tracing::info!(
            pr_node = %pr.node_id,
            head    = %pr.head_ref,
            "PR has no task linkage; skipping publish",
        );
        return Ok(());
    }
    let ev = DomainEvent {
        event_key,
        source: Source::Github,
        event_type: "github.pr_merged_ready".into(),
        payload,
    };
    publisher
        .send(pool, ev, None)
        .await
        .map_err(WatcherError::Bus)?;
    Ok(())
}
