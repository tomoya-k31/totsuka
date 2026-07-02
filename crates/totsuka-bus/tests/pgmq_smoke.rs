use serde_json::json;
use totsuka_bus::*;

#[tokio::test]
async fn send_read_delete_cycle() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let qname = format!("test_q_{}", uuid::Uuid::new_v4().simple());

    create_queue(&pool, &qname).await.unwrap();
    let payload = json!({"event_key": "t:1", "payload": {"x": 1}});
    let msg_id = send_json(&pool, &qname, &payload).await.unwrap();
    assert!(msg_id > 0);

    let m = read_one(&pool, &qname, 30)
        .await
        .unwrap()
        .expect("must read 1 message");
    assert_eq!(m.msg_id, msg_id);
    assert_eq!(m.message["event_key"], "t:1");

    let ok = delete(&pool, &qname, m.msg_id).await.unwrap();
    assert!(ok);

    // clean up: drop the test queue
    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&qname)
        .execute(&pool)
        .await
        .unwrap();
}
