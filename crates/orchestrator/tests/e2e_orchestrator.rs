use orchestrator::adapter_client::MockAdapter;
use orchestrator::consumer::run_consumer;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::repository::{PgRepository, Repository};
use orchestrator::sm::Engine;
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use totsuka_bus::pgmq::create_queue;
use totsuka_bus::publisher::Publisher;
use totsuka_core::{DomainEvent, Source, SystemClock, TaskId};

fn ev(item_id: &str, ty: &str, payload: serde_json::Value) -> DomainEvent {
    DomainEvent {
        event_key: format!("e2e:{}:{}:{}", ty, item_id, uuid::Uuid::new_v4().simple()),
        source: Source::Github,
        event_type: ty.into(),
        payload,
    }
}

#[tokio::test]
async fn full_walk_inbox_to_released() {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let q = format!(
        "e2e_{}",
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
    let writeback = Arc::new(MockWriteback::new());
    let repo = Arc::new(PgRepository::new(pool.clone(), clock.clone()));
    let engine = Arc::new(Engine {
        repo: repo.clone(),
        adapter: adapter.clone(),
        writeback: writeback.clone(),
        effects: Arc::new(EffectLedger::new(pool.clone(), clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock: clock.clone(),
        config: cfg.clone(),
        owner_id: "e2e".into(),
    });

    let token = CancellationToken::new();
    let consumer_engine = engine.clone();
    let consumer_pool = pool.clone();
    let consumer_q = q.clone();
    let consumer_token = token.clone();
    let consumer_h = tokio::spawn(async move {
        run_consumer(
            consumer_engine,
            consumer_pool,
            consumer_q,
            16,
            30,
            consumer_token,
        )
        .await
    });

    let id = format!("PVTI_e2e_{}", uuid::Uuid::new_v4().simple());
    let pub_ = Publisher::new(q.clone(), clock.clone());

    let wait_for = |col: &'static str, deadline: Duration| {
        let repo = repo.clone();
        let id = id.clone();
        async move {
            let stop = Instant::now() + deadline;
            loop {
                if let Some(t) = repo.get(&TaskId::new(id.clone())).await.unwrap() {
                    if t.current_column == col {
                        return;
                    }
                }
                if Instant::now() > stop {
                    panic!("timeout waiting for column {col}");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };

    // 1. Human moves Inbox → Ready (event from watcher)
    pub_.send(
        &pool,
        ev(
            &id,
            "github.status_changed",
            serde_json::json!({"item_id": id, "to_status": "ready", "repo": "x/y"}),
        ),
        None,
    )
    .await
    .unwrap();
    wait_for("ready", Duration::from_secs(5)).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while adapter.spawn_count() < 1 {
        if Instant::now() > deadline {
            panic!("designer not spawned");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(adapter.spawn_count(), 1, "designer spawned");

    // 2. Designer signals + human gate ①: column moves to design_review then impl_verify
    pub_.send(
        &pool,
        ev(
            &id,
            "github.status_changed",
            serde_json::json!({"item_id": id, "to_status": "design_review", "repo": "x/y"}),
        ),
        None,
    )
    .await
    .unwrap();
    wait_for("design_review", Duration::from_secs(5)).await;
    pub_.send(
        &pool,
        ev(
            &id,
            "github.status_changed",
            serde_json::json!({"item_id": id, "to_status": "impl_verify", "repo": "x/y"}),
        ),
        None,
    )
    .await
    .unwrap();
    wait_for("impl_verify", Duration::from_secs(5)).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while adapter.spawn_count() < 2 {
        if Instant::now() > deadline {
            panic!("implementer not spawned");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(adapter.spawn_count() >= 2, "implementer spawned");

    // 3. PR ready → verifier spawn
    pub_.send(
        &pool,
        ev(
            &id,
            "github.pr_merged_ready",
            serde_json::json!({"item_id": id, "pr_diff": "diff..."}),
        ),
        None,
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while adapter.spawn_count() < 3 {
        if Instant::now() > deadline {
            panic!("verifier not spawned");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // 4. Verifier passes → writeback to final_review
    pub_.send(
        &pool,
        ev(
            &id,
            "github.pr_verification_passed",
            serde_json::json!({"item_id": id}),
        ),
        None,
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut found = false;
    while Instant::now() < deadline {
        if writeback
            .moves()
            .iter()
            .any(|(_, to, _)| to == "final_review")
        {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "writeback to final_review");

    // 5. Human gate ② → AwaitingRelease via human-driven status_change
    pub_.send(
        &pool,
        ev(
            &id,
            "github.status_changed",
            serde_json::json!({"item_id": id, "to_status": "awaiting_release", "repo": "x/y"}),
        ),
        None,
    )
    .await
    .unwrap();
    wait_for("awaiting_release", Duration::from_secs(5)).await;

    // 6. Release event → writeback to released
    pub_.send(
        &pool,
        ev(
            &id,
            "github.release_published",
            serde_json::json!({"repo": "x/y"}),
        ),
        None,
    )
    .await
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut released_seen = false;
    while Instant::now() < deadline {
        if writeback
            .moves()
            .iter()
            .any(|(t, to, _)| t == &id && to == "released")
        {
            released_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(released_seen, "writeback released");

    token.cancel();
    let _ = consumer_h.await;
    sqlx::query("SELECT pgmq.drop_queue($1)")
        .bind(&q)
        .execute(&pool)
        .await
        .unwrap();
}
