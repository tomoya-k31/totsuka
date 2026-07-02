use orchestrator::adapter_client::MockAdapter;
use orchestrator::consumer::run_consumer;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::repository::{PgRepository, Repository};
use orchestrator::sm::Engine;
use orchestrator::wip::WipGate;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_bus::publisher::Publisher;
use totsuka_core::{DomainEvent, Source, SystemClock, TaskId};

#[tokio::test]
async fn consumer_drives_status_change_into_repo() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let q = format!(
        "test_{}",
        uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(20)
            .collect::<String>()
    );
    create_queue(&pool, &q).await.unwrap();

    let clock: Arc<dyn totsuka_core::Clock> = Arc::new(SystemClock);
    let cfg = Arc::new(
        totsuka_config::Config::load(format!(
            "{}/../../examples/totsuka.toml.example",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap(),
    );
    let adapter = Arc::new(MockAdapter::new());
    let repo = Arc::new(PgRepository::new(pool.clone(), clock.clone()));
    let engine = Arc::new(Engine {
        repo: repo.clone(),
        adapter: adapter.clone(),
        writeback: Arc::new(MockWriteback::new()),
        effects: Arc::new(EffectLedger::new(pool.clone(), clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock: clock.clone(),
        config: cfg,
        owner_id: "test".into(),
    });

    let id = format!("PVTI_cons_{}", uuid::Uuid::new_v4().simple());
    let pub_ = Publisher::new(q.clone(), clock.clone());
    pub_.send(
        &pool,
        DomainEvent {
            event_key: format!("test:cons:{}", id),
            source: Source::Github,
            event_type: "github.status_changed".into(),
            payload: serde_json::json!({"item_id": id, "to_status": "ready", "repo": "x/y"}),
        },
        None,
    )
    .await
    .unwrap();

    let token = CancellationToken::new();
    let token2 = token.clone();
    let engine2 = engine.clone();
    let pool2 = pool.clone();
    let q2 = q.clone();
    let h = tokio::spawn(async move { run_consumer(engine2, pool2, q2, 16, 30, token2).await });

    // Poll until the task row appears (or 5s timeout).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(t) = repo.get(&TaskId::new(id.clone())).await.unwrap() {
            if t.current_column == "ready" {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("timed out waiting for SM to apply");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        adapter.spawn_count(),
        1,
        "ready event should spawn designer"
    );
    token.cancel();
    let _ = h.await;

    // Cleanup queue.
    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&q)
        .execute(&pool)
        .await
        .unwrap();
}
