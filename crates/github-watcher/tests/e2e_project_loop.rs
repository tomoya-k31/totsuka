use chrono::Utc;
use github_watcher::cursor::{get, CursorKey};
use github_watcher::gh_client::{GhClient, MockGhClient, ProjectItem, ProjectItemPage, RepoSlug};
use github_watcher::polling::project::{run_project_loop, ProjectLoopConfig};
use github_watcher::polling::RepoTracker;
use github_watcher::snapshot::PgSnapshotStore;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::{create_queue, Consumer, Publisher};
use totsuka_core::{ColumnId, ColumnMap, SystemClock};
use totsuka_telemetry::HealthState;

fn map() -> ColumnMap {
    use std::collections::HashMap;
    let mut m = HashMap::new();
    m.insert(ColumnId::Inbox, "📥 Inbox".into());
    m.insert(ColumnId::Ready, "📋 Ready".into());
    m.insert(ColumnId::Design, "🤖 調査・設計".into());
    m.insert(ColumnId::DesignReview, "🚧 設計レビュー".into());
    m.insert(ColumnId::ImplVerify, "🤖 実装・受入検証".into());
    m.insert(ColumnId::FinalReview, "🚧 最終レビュー".into());
    m.insert(ColumnId::AwaitingRelease, "🚀 リリース待ち".into());
    m.insert(ColumnId::Released, "🏁 完了".into());
    ColumnMap::try_new(m).unwrap()
}

#[tokio::test]
async fn project_loop_publishes_status_changed_for_every_diff() {
    let Some(url) = std::env::var("DATABASE_URL").ok() else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();

    // Clean slate
    sqlx::query("DELETE FROM gh_item_status WHERE item_id LIKE 'E2E_%'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM catchup_cursor WHERE source='github' AND scope='projectv2_items'")
        .execute(&pool)
        .await
        .unwrap();

    let queue = format!("ghw_e2e_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &queue).await.unwrap();
    let publisher = Arc::new(Publisher::new(queue.clone(), Arc::new(SystemClock)));

    let mock = Arc::new(MockGhClient::new());
    let r1 = RepoSlug::parse("acme/x").unwrap();
    mock.set_project_items_pages(vec![
        ProjectItemPage {
            items: vec![
                ProjectItem {
                    id: "E2E_A".into(),
                    status_display: Some("📋 Ready".into()),
                    repo: Some(r1.clone()),
                    content_number: Some(1),
                    closed_at: None,
                },
                ProjectItem {
                    id: "E2E_B".into(),
                    status_display: Some("🤖 調査・設計".into()),
                    repo: Some(r1.clone()),
                    content_number: Some(2),
                    closed_at: None,
                },
            ],
            end_cursor: Some("p1".into()),
            has_next: true,
        },
        ProjectItemPage {
            items: vec![ProjectItem {
                id: "E2E_C".into(),
                status_display: Some("🏁 完了".into()),
                repo: Some(r1.clone()),
                content_number: Some(3),
                closed_at: Some(Utc::now()),
            }],
            end_cursor: Some("p2".into()),
            has_next: false,
        },
    ]);

    let snapshot = Arc::new(PgSnapshotStore::new(pool.clone(), publisher.clone()));
    let tracker = RepoTracker::new();
    let column_map = Arc::new(map());
    let health = HealthState::new();
    let cfg = ProjectLoopConfig {
        project_node_id: "PVT_x".into(),
        page_size: 100,
        poll_interval: Duration::from_millis(50),
    };
    let shutdown = CancellationToken::new();
    let pool2 = pool.clone();
    let s2 = shutdown.clone();
    let h = tokio::spawn(async move {
        run_project_loop(
            pool2,
            mock.clone() as Arc<dyn GhClient>,
            snapshot,
            column_map,
            tracker,
            Arc::new(SystemClock),
            health,
            cfg,
            s2,
        )
        .await
    });

    // Allow one tick
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), h).await;

    // Snapshot rows present
    let a: (Option<String>,) =
        sqlx::query_as("SELECT status FROM gh_item_status WHERE item_id='E2E_A'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(a.0.as_deref(), Some("ready"));

    let b: (Option<String>,) =
        sqlx::query_as("SELECT status FROM gh_item_status WHERE item_id='E2E_B'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(b.0.as_deref(), Some("design"));

    let c: (Option<String>,) =
        sqlx::query_as("SELECT status FROM gh_item_status WHERE item_id='E2E_C'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(c.0.as_deref(), Some("released"));

    // Three envelopes published
    let consumer = Consumer::new(queue.clone());
    let mut seen = 0;
    for _ in 0..10 {
        if let Some((mid, env)) = consumer.poll_one(&pool, 1).await.unwrap() {
            assert_eq!(env.event_type, "github.status_changed");
            consumer.ack(&pool, mid).await.unwrap();
            seen += 1;
        } else {
            break;
        }
    }
    assert_eq!(seen, 3);

    // Cursor reset after last page
    assert_eq!(
        get(&pool, &CursorKey::project_items()).await.unwrap(),
        Some("".into())
    );
}
