use chrono::{Duration, Utc};
use qa_service::thread_map::{ThreadMapRepo, ThreadMapping};
use std::sync::Arc;
use totsuka_core::SystemClock;

fn ts() -> String {
    format!("t_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn upsert_get_round_trip() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let repo = ThreadMapRepo::new(db.pool.clone(), Arc::new(SystemClock));
    let tts = ts();
    let m = ThreadMapping {
        thread_ts: tts.clone(),
        terminal_id: "term_1".into(),
        repo: "acme/r".into(),
        origin: "owner".into(),
        last_activity_at: Utc::now(),
        created_at: Utc::now(),
    };
    repo.upsert(&m).await.unwrap();
    let got = repo.get(&tts).await.unwrap().unwrap();
    assert_eq!(got.terminal_id, "term_1");
    assert_eq!(got.repo, "acme/r");
    assert_eq!(got.origin, "owner");
    repo.delete(&tts).await.unwrap();
}

#[tokio::test]
async fn self_mention_origin_round_trips() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let repo = ThreadMapRepo::new(db.pool.clone(), Arc::new(SystemClock));
    let tts = ts();
    let m = ThreadMapping {
        thread_ts: tts.clone(),
        terminal_id: "term_sm".into(),
        repo: "acme/r".into(),
        origin: "self_mention".into(),
        last_activity_at: Utc::now(),
        created_at: Utc::now(),
    };
    repo.upsert(&m).await.unwrap();
    let got = repo.get(&tts).await.unwrap().unwrap();
    assert_eq!(got.origin, "self_mention");
    repo.delete(&tts).await.unwrap();
}

#[tokio::test]
async fn touch_advances_last_activity() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let repo = ThreadMapRepo::new(db.pool.clone(), Arc::new(SystemClock));
    let tts = ts();
    let initial = Utc::now() - Duration::hours(1);
    repo.upsert(&ThreadMapping {
        thread_ts: tts.clone(),
        terminal_id: "term_2".into(),
        repo: "acme/r".into(),
        origin: "owner".into(),
        last_activity_at: initial,
        created_at: initial,
    })
    .await
    .unwrap();
    repo.touch(&tts).await.unwrap();
    let got = repo.get(&tts).await.unwrap().unwrap();
    assert!(got.last_activity_at > initial);
    repo.delete(&tts).await.unwrap();
}

#[tokio::test]
async fn list_idle_filters_by_threshold() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let repo = ThreadMapRepo::new(db.pool.clone(), Arc::new(SystemClock));
    let old_ts = ts();
    let new_ts = ts();
    let now = Utc::now();
    repo.upsert(&ThreadMapping {
        thread_ts: old_ts.clone(),
        terminal_id: "term_old".into(),
        repo: "acme/r".into(),
        origin: "owner".into(),
        last_activity_at: now - Duration::hours(2),
        created_at: now - Duration::hours(2),
    })
    .await
    .unwrap();
    repo.upsert(&ThreadMapping {
        thread_ts: new_ts.clone(),
        terminal_id: "term_new".into(),
        repo: "acme/r".into(),
        origin: "owner".into(),
        last_activity_at: now,
        created_at: now,
    })
    .await
    .unwrap();
    let idle = repo.list_idle(now - Duration::hours(1)).await.unwrap();
    let ids: Vec<&str> = idle.iter().map(|m| m.terminal_id.as_str()).collect();
    assert!(ids.contains(&"term_old"));
    assert!(!ids.contains(&"term_new"));
    repo.delete(&old_ts).await.unwrap();
    repo.delete(&new_ts).await.unwrap();
}
