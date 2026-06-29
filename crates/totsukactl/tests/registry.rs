use chrono::{TimeZone, Utc};
use totsukactl::registry::{Registry, ORDER};
use totsukactl::state::ChildState;

#[tokio::test]
async fn list_returns_spec_startup_order() {
    let reg = Registry::new();
    let names: Vec<_> = reg.list().await.into_iter().map(|e| e.name).collect();
    let want: Vec<_> = ORDER.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(names, want);
}

#[tokio::test]
async fn set_state_persists() {
    let reg = Registry::new();
    reg.set_state("orchestrator", ChildState::Healthy).await;
    assert_eq!(reg.get("orchestrator").await.unwrap().state, ChildState::Healthy);
}

#[tokio::test]
async fn bump_then_reset_failure() {
    let reg = Registry::new();
    assert_eq!(reg.bump_failure("qa-service").await, 1);
    assert_eq!(reg.bump_failure("qa-service").await, 2);
    reg.reset_failure("qa-service").await;
    assert_eq!(reg.get("qa-service").await.unwrap().consecutive_failures, 0);
}

#[tokio::test]
async fn touch_records_last_healthz_at() {
    let reg = Registry::new();
    let t = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    reg.touch_healthz("agent-adapter", t).await;
    assert_eq!(reg.get("agent-adapter").await.unwrap().last_healthz_at, Some(t));
}

#[tokio::test]
async fn set_pid_round_trip() {
    let reg = Registry::new();
    let t = Utc.with_ymd_and_hms(2026, 6, 29, 12, 0, 0).unwrap();
    reg.set_pid("orchestrator", Some(4242), Some(t)).await;
    let e = reg.get("orchestrator").await.unwrap();
    assert_eq!(e.pid, Some(4242));
    assert_eq!(e.started_at, Some(t));
}
