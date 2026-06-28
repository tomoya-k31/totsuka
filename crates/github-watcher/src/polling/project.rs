use super::RepoTracker;
#[allow(unused_imports)]
use crate::column_map::build as _build; // unused but documents the source
use crate::cursor::{get, set, CursorKey};
use crate::error::WatcherError;
use crate::gh_client::GhClient;
use crate::snapshot::{ItemSnapshot, SnapshotStore};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_core::{event_key_gh_status, Clock, ColumnId, ColumnMap, DomainEvent, Source};
use totsuka_telemetry::HealthState;

pub struct ProjectLoopConfig {
    pub project_node_id: String,
    pub page_size: u32,
    pub poll_interval: Duration,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_project_loop(
    pool: PgPool,
    client: Arc<dyn GhClient>,
    store: Arc<dyn SnapshotStore>,
    column_map: Arc<ColumnMap>,
    tracker: RepoTracker,
    _clock: Arc<dyn Clock>,
    health: HealthState,
    cfg: ProjectLoopConfig,
    shutdown: CancellationToken,
) -> Result<(), WatcherError> {
    let mut interval = tokio::time::interval(cfg.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if let Err(e) = run_one_pass(&pool, &client, &store, &column_map, &tracker, &cfg).await {
                    tracing::error!(error=%e, "project loop tick failed");
                    health.set_check("github", &format!("fail: {e}")).await;
                } else {
                    health.set_check("github", "ok").await;
                }
            }
        }
    }
}

async fn run_one_pass(
    pool: &PgPool,
    client: &Arc<dyn GhClient>,
    store: &Arc<dyn SnapshotStore>,
    column_map: &Arc<ColumnMap>,
    tracker: &RepoTracker,
    cfg: &ProjectLoopConfig,
) -> Result<(), WatcherError> {
    let mut after: Option<String> = get(pool, &CursorKey::project_items()).await?;
    if after.as_deref() == Some("") {
        after = None;
    }
    loop {
        let page = client
            .project_items_page(&cfg.project_node_id, after.as_deref(), cfg.page_size)
            .await?;
        // 1. Translate ProjectItem → ItemSnapshot
        let mut snapshots = Vec::with_capacity(page.items.len());
        let mut item_repos = Vec::with_capacity(page.items.len());
        for it in &page.items {
            let status: Option<ColumnId> = match &it.status_display {
                None => None,
                Some(display) => match column_map.resolve(display) {
                    Some(c) => Some(c),
                    None => return Err(WatcherError::UnknownColumn(display.clone())),
                },
            };
            let content_ref = it
                .repo
                .as_ref()
                .zip(it.content_number)
                .map(|(r, n)| format!("{r}#{n}"));
            snapshots.push(ItemSnapshot {
                item_id: it.id.clone(),
                status,
                content_ref,
                closed_at: it.closed_at,
            });
            if let Some(r) = &it.repo {
                item_repos.push(r.clone());
            }
        }
        // 2. Diff against current snapshot
        let diffs = store.diff_page(&snapshots).await?;
        // 3. Build events
        let mut events: Vec<(String, DomainEvent)> = Vec::with_capacity(diffs.len());
        for d in &diffs {
            let Some(to) = d.to_status else { continue }; // skip transitions to "no status"
            let snake = to.as_snake();
            let hash_full = format!("{:x}", md5::compute(snake.as_bytes()));
            let hash = &hash_full[..8];
            let key = event_key_gh_status(&d.item_id, hash);
            let ev = DomainEvent {
                event_key: key.clone(),
                source: Source::Github,
                event_type: "github.status_changed".into(),
                payload: serde_json::json!({
                    "item_id": d.item_id,
                    "to_status": snake,
                    "repo": d.repo.clone().unwrap_or_default(),
                }),
            };
            events.push((key, ev));
        }
        // 4. Atomic commit (events + UPSERTs + cursor in one tx)
        store
            .commit_page(&snapshots, &events, page.end_cursor.as_deref())
            .await?;
        // 5. RepoTracker bookkeeping (after commit — we don't want to leak repos on failure)
        for r in item_repos {
            tracker.insert(r).await;
        }

        if !page.has_next {
            break;
        }
        after = page.end_cursor;
    }
    // Reset cursor so next tick walks from page 1 (ProjectsV2 has no since;
    // the snapshot/diff layer absorbs the no-op cost via deterministic event_key).
    set(pool, &CursorKey::project_items(), "").await?;
    Ok(())
}
