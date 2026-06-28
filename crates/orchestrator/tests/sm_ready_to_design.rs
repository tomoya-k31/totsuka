use orchestrator::adapter_client::MockAdapter;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::repository::PgRepository;
use orchestrator::sm::{Engine, HandleOutcome};
use orchestrator::wip::WipGate;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use totsuka_core::{DomainEvent, Source, SystemClock};

fn ev(item_id: &str, to: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:{}:{}", item_id, to),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({"item_id": item_id, "to_status": to, "repo": "x/y"}),
    }
}

async fn engine() -> Option<(Engine, Arc<MockAdapter>, Arc<MockWriteback>)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .unwrap();
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
    let engine = Engine {
        repo: Arc::new(PgRepository::new(pool.clone(), clock.clone())),
        adapter: adapter.clone(),
        writeback: writeback.clone(),
        effects: Arc::new(EffectLedger::new(pool, clock.clone(), 30)),
        wip: Arc::new(WipGate::new(3)),
        clock,
        config: cfg,
        owner_id: "test".into(),
    };
    Some((engine, adapter, writeback))
}

#[tokio::test]
async fn ready_event_spawns_designer() {
    let Some((e, adapter, _)) = engine().await else {
        return;
    };
    let id = format!("PVTI_rd_{}", uuid::Uuid::new_v4().simple());
    let out = e.handle(&ev(&id, "ready")).await.unwrap();
    assert_eq!(out, HandleOutcome::Applied);
    assert_eq!(adapter.spawn_count(), 1);
    let req = adapter.last_spawn().unwrap();
    assert!(req.branch.ends_with("/design"), "branch={}", req.branch);
    assert_eq!(req.attempt, 0);
}
