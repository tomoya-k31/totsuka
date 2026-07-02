use chrono::TimeZone;
use serde_json::json;
use std::sync::Arc;
use totsuka_bus::*;
use totsuka_core::{DomainEvent, MockClock, Source};

#[tokio::test]
async fn publish_consume_ack() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let pool = db.pool.clone();
    let qname = format!("test_env_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &qname).await.unwrap();

    let clock = Arc::new(MockClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
    ));
    let publisher = Publisher::new(qname.clone(), clock);
    let consumer = Consumer::new(qname.clone());

    let ev = DomainEvent {
        event_key: "gh:delivery:abc".into(),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: json!({"to_status": "design"}),
    };
    let msg_id = publisher
        .send(&pool, ev, Some("trace-1".into()))
        .await
        .unwrap();
    assert!(msg_id > 0);

    let Some((mid, env)) = consumer.poll_one(&pool, 30).await.unwrap() else {
        panic!("expected message");
    };
    assert_eq!(mid, msg_id);
    assert_eq!(env.event_key, "gh:delivery:abc");
    assert_eq!(env.event_type, "github.status_changed");
    assert_eq!(env.trace_id.as_deref(), Some("trace-1"));

    assert!(consumer.ack(&pool, mid).await.unwrap());
    assert!(consumer.poll_one(&pool, 30).await.unwrap().is_none());

    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&qname)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn send_in_tx_commit_visible() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let pool = db.pool.clone();
    let qname = format!("test_tx_commit_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &qname).await.unwrap();

    let clock = Arc::new(MockClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
    ));
    let publisher = Publisher::new(qname.clone(), clock);
    let consumer = Consumer::new(qname.clone());

    let ev = DomainEvent {
        event_key: "gh:delivery:tx-commit".into(),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: json!({"committed": true}),
    };

    // begin tx → send_in_tx → commit
    let mut tx = pool.begin().await.unwrap();
    let msg_id = publisher
        .send_in_tx(&mut tx, ev, Some("trace-tx".into()))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // message should be visible after commit
    let Some((mid, env)) = consumer.poll_one(&pool, 30).await.unwrap() else {
        panic!("expected message after tx commit");
    };
    assert_eq!(mid, msg_id);
    assert_eq!(env.event_key, "gh:delivery:tx-commit");
    assert_eq!(env.trace_id.as_deref(), Some("trace-tx"));
    consumer.ack(&pool, mid).await.unwrap();

    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&qname)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn send_in_tx_rollback_invisible() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    };
    let pool = db.pool.clone();
    // pgmq queue names must be ≤47 chars; prefix is 7 chars → 39 total
    let qname = format!("tx_rb_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &qname).await.unwrap();

    let clock = Arc::new(MockClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
    ));
    let publisher = Publisher::new(qname.clone(), clock);
    let consumer = Consumer::new(qname.clone());

    let ev = DomainEvent {
        event_key: "gh:delivery:tx-rollback".into(),
        source: Source::Internal,
        event_type: "github.status_changed".into(),
        payload: json!({"rolled_back": true}),
    };

    // begin tx → send_in_tx → rollback
    let mut tx = pool.begin().await.unwrap();
    publisher.send_in_tx(&mut tx, ev, None).await.unwrap();
    tx.rollback().await.unwrap();

    // message should NOT be visible after rollback
    assert!(
        consumer.poll_one(&pool, 30).await.unwrap().is_none(),
        "message must not appear after tx rollback"
    );

    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&qname)
        .execute(&pool)
        .await
        .unwrap();
}
