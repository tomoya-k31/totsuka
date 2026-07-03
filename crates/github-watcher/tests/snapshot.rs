use chrono::Utc;
use github_watcher::cursor::{get, CursorKey};
use github_watcher::snapshot::{ItemSnapshot, PgSnapshotStore, SnapshotStore};
use std::sync::Arc;
use totsuka_bus::{Consumer, Publisher};
use totsuka_core::{ColumnId, DomainEvent, Source, SystemClock};

fn unique_queue() -> String {
    format!("ghw_test_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn diff_detects_new_and_changed_items() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    // Seed: item A in design
    sqlx::query(
        "INSERT INTO gh_item_status (item_id, status) VALUES ($1, 'design') ON CONFLICT DO NOTHING",
    )
    .bind("PVTI_A")
    .execute(&pool)
    .await
    .unwrap();

    let page = vec![
        ItemSnapshot {
            item_id: "PVTI_A".into(),
            status: Some(ColumnId::ImplVerify),
            content_ref: Some("acme/r#1".into()),
            closed_at: None,
        },
        ItemSnapshot {
            item_id: "PVTI_B".into(),
            status: Some(ColumnId::Ready),
            content_ref: Some("acme/r#2".into()),
            closed_at: None,
        },
    ];
    let diffs = store.diff_page(&page).await.unwrap();
    assert_eq!(diffs.len(), 2);
    let a = diffs.iter().find(|d| d.item_id == "PVTI_A").unwrap();
    assert_eq!(a.from_status, Some(ColumnId::Design));
    assert_eq!(a.to_status, Some(ColumnId::ImplVerify));
    let b = diffs.iter().find(|d| d.item_id == "PVTI_B").unwrap();
    assert_eq!(b.from_status, None);
    assert_eq!(b.to_status, Some(ColumnId::Ready));
    assert_eq!(b.repo.as_deref(), Some("acme/r"));
}

#[tokio::test]
async fn commit_page_writes_events_snapshots_and_cursor_atomically() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    let page = vec![ItemSnapshot {
        item_id: "PVTI_C".into(),
        status: Some(ColumnId::Ready),
        content_ref: Some("acme/x#9".into()),
        closed_at: Some(Utc::now()),
    }];
    let ev = DomainEvent {
        event_key: "gh:status:PVTI_C:abc12345".into(),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({ "item_id": "PVTI_C", "to_status": "ready", "repo": "acme/x" }),
    };
    store
        .commit_page(&page, &[(ev.event_key.clone(), ev)], Some("endCursor-1"))
        .await
        .unwrap();

    // Snapshot row was written.
    let row: (Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, closed_at FROM gh_item_status WHERE item_id = 'PVTI_C'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row.0.as_deref(), Some("ready"));
    assert!(row.1.is_some());

    // Cursor was advanced.
    assert_eq!(
        get(&pool, &CursorKey::project_items()).await.unwrap(),
        Some("endCursor-1".into())
    );

    // The published event sits in pgmq.
    let consumer = Consumer::new(q.clone());
    let (msg_id, env) = consumer
        .poll_one(&pool, 5)
        .await
        .unwrap()
        .expect("one envelope");
    assert_eq!(env.event_type, "github.status_changed");
    consumer.ack(&pool, msg_id).await.unwrap();
}

/// Moving a card A→B→A must produce THREE distinct events: the seq
/// generation in the event key is what lets a design-review send work
/// back to design and actually re-trigger the agent.
#[tokio::test]
async fn revisiting_a_column_produces_a_fresh_event_key() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let q = unique_queue();
    totsuka_bus::create_queue(&pool, &q).await.unwrap();
    let publisher = Arc::new(Publisher::new(q.clone(), Arc::new(SystemClock)));
    let store = PgSnapshotStore::new(pool.clone(), publisher);

    let snap = |col: ColumnId| {
        vec![ItemSnapshot {
            item_id: "PVTI_GEN".into(),
            status: Some(col),
            content_ref: Some("acme/r#7".into()),
            closed_at: None,
        }]
    };
    let mut keys = Vec::new();
    for col in [ColumnId::Design, ColumnId::DesignReview, ColumnId::Design] {
        let page = snap(col);
        let diffs = store.diff_page(&page).await.unwrap();
        assert_eq!(diffs.len(), 1, "each move is a diff");
        let d = &diffs[0];
        let hash_full = format!(
            "{:x}",
            md5::compute(d.to_status.unwrap().as_snake().as_bytes())
        );
        let key = totsuka_core::key::event_key_gh_status("PVTI_GEN", &hash_full[..8], d.seq);
        let ev = DomainEvent {
            event_key: key.clone(),
            source: Source::Github,
            event_type: "github.status_changed".into(),
            payload: serde_json::json!({"item_id": "PVTI_GEN"}),
        };
        store
            .commit_page(&page, &[(key.clone(), ev)], None)
            .await
            .unwrap();
        keys.push(key);
    }
    assert_eq!(keys.len(), 3);
    assert_ne!(
        keys[0], keys[2],
        "second visit to design must be a NEW event, got identical keys"
    );
    // Generation is monotone per transition.
    let (seq,): (i64,) =
        sqlx::query_as("SELECT status_seq FROM gh_item_status WHERE item_id='PVTI_GEN'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(seq, 3, "three transitions = seq 3");
}
