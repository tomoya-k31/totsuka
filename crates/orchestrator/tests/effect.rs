use orchestrator::effect::{ClaimOutcome, EffectLedger};
use std::sync::Arc;
use totsuka_core::SystemClock;

#[tokio::test]
async fn double_claim_second_skipped() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let l = EffectLedger::new(pool, Arc::new(SystemClock), 30);
    let key = format!("spawn:test:{}:0", uuid::Uuid::new_v4().simple());
    let event = format!("gh:test:{}", uuid::Uuid::new_v4().simple());
    let first = l.claim(&key, &event, "spawn", "owner-a").await.unwrap();
    assert_eq!(first, ClaimOutcome::Claimed);
    let second = l.claim(&key, &event, "spawn", "owner-b").await.unwrap();
    assert!(matches!(second, ClaimOutcome::Skipped { .. }));
}

#[tokio::test]
async fn complete_then_re_claim_skipped() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let l = EffectLedger::new(pool, Arc::new(SystemClock), 30);
    let key = format!("spawn:test:{}:0", uuid::Uuid::new_v4().simple());
    l.claim(&key, "ev", "spawn", "a").await.unwrap();
    l.complete(&key, serde_json::json!({"ok": true}))
        .await
        .unwrap();
    let again = l.claim(&key, "ev", "spawn", "b").await.unwrap();
    match again {
        ClaimOutcome::Skipped { reason } => assert!(reason.contains("done")),
        other => panic!("expected skipped done, got {other:?}"),
    }
}
