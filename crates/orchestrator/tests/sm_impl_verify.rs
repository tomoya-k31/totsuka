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

#[tokio::test]
async fn impl_verify_enter_spawns_implementer() {
    let Some((e, adapter, _)) = engine().await else {
        return;
    };
    let id = format!("PVTI_iv_{}", uuid::Uuid::new_v4().simple());
    let out = e.handle(&ev(&id, "impl_verify")).await.unwrap();
    assert_eq!(out, HandleOutcome::Applied);
    assert_eq!(adapter.spawn_count(), 1);
    let req = adapter.last_spawn().unwrap();
    assert!(req.branch.ends_with("/implv"), "branch={}", req.branch);
    assert_eq!(req.attempt, 0);
    assert!(!req.detached, "impl phase needs a real branch to commit on");

    // The task prompt rides on argv — see sm_ready_to_design for why.
    assert_eq!(
        adapter.send_count(),
        0,
        "no post-spawn typing — the prompt must be in argv"
    );
    let prompt = req.argv.last().cloned().expect("argv carries the prompt");
    assert!(
        prompt.contains("#18"),
        "prompt should reference the issue: {prompt}"
    );
    assert!(
        prompt.contains("x/y"),
        "prompt should reference the repo: {prompt}"
    );
    assert!(
        !prompt.ends_with('\r'),
        "argv prompts need no CR — that was only for typed input"
    );
}

fn pr_ready(item_id: &str, diff: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:pr:{}", item_id),
        source: Source::Github,
        event_type: "github.pr_merged_ready".into(),
        payload: serde_json::json!({"item_id": item_id, "pr_diff": diff}),
    }
}

#[tokio::test]
async fn pr_ready_spawns_verifier_after_implementer() {
    let Some((e, adapter, _)) = engine().await else {
        return;
    };
    let id = format!("PVTI_pr_{}", uuid::Uuid::new_v4().simple());
    let _ = e.handle(&ev(&id, "impl_verify")).await.unwrap();
    assert_eq!(adapter.spawn_count(), 1, "implementer spawned");
    let _ = e.handle(&pr_ready(&id, "diff text here")).await.unwrap();
    assert_eq!(adapter.spawn_count(), 2, "verifier spawned");
    let verifier = adapter.last_spawn().unwrap();
    assert_eq!(verifier.phase, "verify");
}

fn verify_event(item_id: &str, ty: &str) -> DomainEvent {
    DomainEvent {
        event_key: format!("test:v:{}", item_id),
        source: Source::Github,
        event_type: ty.into(),
        payload: serde_json::json!({"item_id": item_id}),
    }
}

#[tokio::test]
async fn diff_back_bumps_attempt_and_respawns() {
    let Some((e, adapter, _)) = engine().await else {
        return;
    };
    let id = format!("PVTI_db_{}", uuid::Uuid::new_v4().simple());
    let _ = e.handle(&ev(&id, "impl_verify")).await.unwrap();
    let _ = e
        .handle(&verify_event(&id, "github.pr_verification_failed"))
        .await
        .unwrap();
    assert_eq!(adapter.spawn_count(), 2);
    let last = adapter.last_spawn().unwrap();
    assert_eq!(last.attempt, 1, "attempt bumped");
    assert!(last.branch.ends_with("/implv"));
}
