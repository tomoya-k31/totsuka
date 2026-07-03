use orchestrator::adapter_client::MockAdapter;
use orchestrator::effect::EffectLedger;
use orchestrator::gh_writeback::MockWriteback;
use orchestrator::repository::PgRepository;
use orchestrator::sm::{Engine, HandleOutcome};
use orchestrator::wip::WipGate;
use std::sync::Arc;
use totsuka_core::{DomainEvent, Source, SystemClock};

fn ev(item_id: &str, to: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:{}:{}", item_id, to),
        source: Source::Github,
        event_type: "github.status_changed".into(),
        payload: serde_json::json!({"item_id": item_id, "to_status": to, "repo": "x/y", "issue_number": 18}),
    }
}

async fn engine() -> Option<(Engine, Arc<MockAdapter>, Arc<MockWriteback>)> {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return None;
    };
    let pool = db.pool.clone();
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

/// The human moving a card into 🤖 調査・設計 is the signal to start the
/// design agent — the 🤖 columns mean "AI works here".
#[tokio::test]
async fn design_column_move_spawns_designer() {
    let Some((e, adapter, _)) = engine().await else {
        return;
    };
    let id = format!("PVTI_rd_{}", uuid::Uuid::new_v4().simple());
    let out = e.handle(&ev(&id, "design")).await.unwrap();
    assert_eq!(out, HandleOutcome::Applied);
    assert_eq!(adapter.spawn_count(), 1);
    let req = adapter.last_spawn().unwrap();
    assert!(req.branch.ends_with("/design"), "branch={}", req.branch);
    assert_eq!(req.attempt, 0);
    assert!(
        req.detached,
        "design phase must not create a branch (detached worktree)"
    );

    // The agent must receive its task prompt right after spawn — an idle
    // Claude with no instructions is useless.
    let (_agent, prompt) = adapter.last_send().expect("prompt sent after spawn");
    assert!(
        prompt.contains("#18"),
        "prompt should reference the issue: {prompt}"
    );
    assert!(
        prompt.contains("x/y"),
        "prompt should reference the repo: {prompt}"
    );
    assert!(
        prompt.contains("gh issue comment"),
        "design deliverable is an issue comment, not a commit: {prompt}"
    );
    assert!(
        prompt.contains("設計レビュー"),
        "design prompt must instruct the card move: {prompt}"
    );
    assert!(
        prompt.contains("42"),
        "prompt must carry the project number for the card move: {prompt}"
    );
    assert!(
        prompt.ends_with('\r'),
        "prompt must end with CR — the TUI submits on Enter (\\r); \
         without it the text sits in the input box forever"
    );
}

/// 📋 Ready is a backlog state, not a trigger: the column is recorded
/// but no agent starts.
#[tokio::test]
async fn ready_event_records_column_without_spawning() {
    let Some((e, adapter, _)) = engine().await else {
        return;
    };
    let id = format!("PVTI_rd_{}", uuid::Uuid::new_v4().simple());
    let out = e.handle(&ev(&id, "ready")).await.unwrap();
    assert_eq!(out, HandleOutcome::Applied);
    assert_eq!(adapter.spawn_count(), 0, "ready must not spawn an agent");
}
