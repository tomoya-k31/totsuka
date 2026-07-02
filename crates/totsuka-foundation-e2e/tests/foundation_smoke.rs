use chrono::TimeZone;
use serde_json::json;
use std::sync::Arc;
use totsuka_bus::*;
use totsuka_config::Config;
use totsuka_core::{DomainEvent, MockClock, NotifyKind, Source};
use totsuka_telemetry::*;

#[tokio::test]
async fn config_loaded_publish_consume_notify_deduped() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("skip");
        return;
    };

    // 1. config (例ファイル) を読む
    let example_path = format!(
        "{}/../../examples/totsuka.toml.example",
        env!("CARGO_MANIFEST_DIR")
    );
    let txt = std::fs::read_to_string(&example_path).unwrap();
    let cfg = Config::from_toml_str(&txt).unwrap();
    cfg.validate().expect("example must validate");
    assert_eq!(cfg.bus.queue_name, "totsuka_events");

    // 2. bus publish/consume
    let pool = db.pool.clone();
    let qname = format!("smoke_{}", uuid::Uuid::new_v4().simple());
    create_queue(&pool, &qname).await.unwrap();
    let clock = Arc::new(MockClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 6, 28, 12, 0, 0).unwrap(),
    ));
    let pubr = Publisher::new(qname.clone(), clock.clone());
    let cons = Consumer::new(qname.clone());

    let ev = DomainEvent {
        event_key: "smoke:1".into(),
        source: Source::Internal,
        event_type: "smoke.tick".into(),
        payload: json!({"n": 1}),
    };
    let mid = pubr.send(&pool, ev, None).await.unwrap();
    let (got_id, env) = cons.poll_one(&pool, 30).await.unwrap().unwrap();
    assert_eq!(got_id, mid);
    assert_eq!(env.event_key, "smoke:1");
    cons.ack(&pool, mid).await.unwrap();

    // 3. notifier (LogSink 1 つ、dedup TTL=60s)
    use std::collections::HashMap;
    let tmp = tempfile::tempdir().unwrap();
    let mut ttl = HashMap::new();
    ttl.insert(NotifyKind::TaskStuck, 60);
    let mut route = HashMap::new();
    route.insert(NotifyKind::TaskStuck, vec![SinkId::Log]);

    let n = Notifier::new(
        clock.clone(),
        tmp.path().join("notify_state.json"),
        vec![Arc::new(LogSink)],
        route,
        ttl,
    )
    .await;

    n.notify(NotifyKind::TaskStuck, "task:x", NotifyPayload::default())
        .await;
    n.notify(NotifyKind::TaskStuck, "task:x", NotifyPayload::default())
        .await;
    // dedup されていれば state ファイルに 1 entry のみ
    let bytes = std::fs::read(tmp.path().join("notify_state.json")).unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("task_stuck:task:x"));

    // 清掃
    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&qname)
        .execute(&pool)
        .await
        .unwrap();
}
