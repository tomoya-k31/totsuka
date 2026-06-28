use chrono::Utc;
use github_watcher::cursor::{get, CursorKey};
use github_watcher::snapshot::{ItemSnapshot, PgSnapshotStore, SnapshotStore};
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_bus::{Consumer, Publisher};
use totsuka_core::{ColumnId, DomainEvent, Source, SystemClock};

async fn pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL").ok()?;
    Some(PgPoolOptions::new().max_connections(2).connect(&url).await.unwrap())
}

fn unique_queue() -> String {
    format!("ghw_test_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn diff_detects_new_and_changed_items() {
    let Some(pool) = pool().await else { return };
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    // Seed: item A in design
    sqlx::query("INSERT INTO gh_item_status (item_id, status) VALUES ($1, 'design') ON CONFLICT DO NOTHING")
        .bind("PVTI_A")
        .execute(&pool).await.unwrap();

    let page = vec![
        ItemSnapshot { item_id: "PVTI_A".into(), status: Some(ColumnId::ImplVerify), content_ref: Some("acme/r#1".into()), closed_at: None },
        ItemSnapshot { item_id: "PVTI_B".into(), status: Some(ColumnId::Ready),     content_ref: Some("acme/r#2".into()), closed_at: None },
    ];
    let diffs = store.diff_page(&page).await.unwrap();
    assert_eq!(diffs.len(), 2);
    let a = diffs.iter().find(|d| d.item_id == "PVTI_A").unwrap();
    assert_eq!(a.from_status, Some(ColumnId::Design));
    assert_eq!(a.to_status,   Some(ColumnId::ImplVerify));
    let b = diffs.iter().find(|d| d.item_id == "PVTI_B").unwrap();
    assert_eq!(b.from_status, None);
    assert_eq!(b.to_status,   Some(ColumnId::Ready));
    assert_eq!(b.repo.as_deref(), Some("acme/r"));
}

#[tokio::test]
async fn commit_page_writes_events_snapshots_and_cursor_atomically() {
    let Some(pool) = pool().await else { return };
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    let page = vec![
        ItemSnapshot { item_id: "PVTI_C".into(), status: Some(ColumnId::Ready),  content_ref: Some("acme/x#9".into()), closed_at: Some(Utc::now()) },
    ];
    let ev = DomainEvent {
        event_key: "gh:status:PVTI_C:abc12345".into(),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({ "item_id": "PVTI_C", "to_status": "ready", "repo": "acme/x" }),
    };
    store.commit_page(&page, &[(ev.event_key.clone(), ev)], Some("endCursor-1")).await.unwrap();

    // Snapshot row was written.
    let row: (Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, closed_at FROM gh_item_status WHERE item_id = 'PVTI_C'")
            .fetch_one(&pool).await.unwrap();
    assert_eq!(row.0.as_deref(), Some("ready"));
    assert!(row.1.is_some());

    // Cursor was advanced.
    assert_eq!(get(&pool, &CursorKey::project_items()).await.unwrap(), Some("endCursor-1".into()));

    // The published event sits in pgmq.
    let consumer = Consumer::new(q.clone());
    let (msg_id, env) = consumer.poll_one(&pool, 5).await.unwrap().expect("one envelope");
    assert_eq!(env.event_type, "github.status_changed");
    consumer.ack(&pool, msg_id).await.unwrap();
}
