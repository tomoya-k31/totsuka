use chrono::Utc;
use qa_service::adapter_client::{AdapterClient, AgentSummary, MockAdapter};
use qa_service::recovery::reconcile;
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use std::sync::Arc;
use totsuka_core::SystemClock;

#[tokio::test]
async fn reconcile_keeps_pairs_drops_mapping_orphans_closes_pane_orphans() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);
    let thread_map = ThreadMapRepo::new(pool.clone(), clock.clone());

    let alive_ts = format!("alive_{}", uuid::Uuid::new_v4().simple());
    let orphan_ts = format!("orphan_{}", uuid::Uuid::new_v4().simple());

    for t in [&alive_ts, &orphan_ts] {
        sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
            .bind(t)
            .execute(&pool)
            .await
            .unwrap();
    }

    let now = Utc::now();
    thread_map
        .upsert(&ThreadMapping {
            thread_ts: alive_ts.clone(),
            terminal_id: "term_alive".into(),
            repo: "acme/api".into(),
            last_activity_at: now,
            created_at: now,
        })
        .await
        .unwrap();
    thread_map
        .upsert(&ThreadMapping {
            thread_ts: orphan_ts.clone(),
            terminal_id: "term_dead".into(),
            repo: "acme/api".into(),
            last_activity_at: now,
            created_at: now,
        })
        .await
        .unwrap();

    let adapter = MockAdapter::new();
    adapter.set_list_response(vec![
        AgentSummary {
            agent_id: "agent_alive".into(),
            terminal_id: "term_alive".into(),
            label: "totsuka:qa-1:answer:0".into(),
        },
        AgentSummary {
            agent_id: "agent_pane_orphan".into(),
            terminal_id: "term_pane_orphan".into(),
            label: "totsuka:qa-2:answer:0".into(),
        },
    ]);

    let report = reconcile(&thread_map, &adapter as &dyn AdapterClient)
        .await
        .unwrap();
    assert_eq!(report.kept, 1);
    assert_eq!(report.mapping_orphans_deleted, 1);
    assert_eq!(report.pane_orphans_closed, 1);

    // Pane orphan should have been stopped.
    let stops = adapter.expected_stops();
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0].0, "agent_pane_orphan");

    // Mapping orphan should be gone.
    assert!(thread_map.get(&orphan_ts).await.unwrap().is_none());
    // Alive mapping should still exist.
    assert!(thread_map.get(&alive_ts).await.unwrap().is_some());

    for t in [&alive_ts, &orphan_ts] {
        sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
            .bind(t)
            .execute(&pool)
            .await
            .unwrap();
    }
}
