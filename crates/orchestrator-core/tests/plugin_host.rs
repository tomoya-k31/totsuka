//! Integration test for the plugin host, driving a real mock plugin subprocess
//! over NDJSON stdio (F-51/F-54/§5.3).

use std::path::PathBuf;
use std::time::Duration;

use orchestrator_core::adapters::plugin_host::{
    HostError, Plugin, PluginSpec, launchable_plugin_names,
};
use orchestrator_core::adapters::{NewTask, StateDb};
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::state::{TaskEvent, TaskState};
use plugin_protocol::manifest::Manifest;

/// Path to the compiled mock plugin binary (provided by cargo to integration
/// tests of this crate).
fn mock_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin"))
}

fn manifest(protocol_req: &str) -> Manifest {
    Manifest::from_toml_str(&format!(
        r#"
name = "mock"
kind = "agent_ide"
version = "0.1.0"
protocol_version = "{protocol_req}"
"#
    ))
    .unwrap()
}

fn spec(protocol_req: &str) -> PluginSpec {
    PluginSpec {
        name: "mock".to_string(),
        program: mock_plugin(),
        args: vec![],
        manifest: manifest(protocol_req),
        init_config: serde_json::json!({}),
        timeout: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn lifecycle_initialize_shutdown_and_config_validate() {
    let plugin = Plugin::launch(spec("^0.1")).await.expect("launch");

    // initialize recorded the plugin's capabilities and version.
    assert!(plugin.capabilities().plan_mode);
    assert!(plugin.capabilities().state_stream);
    assert_eq!(plugin.plugin_version(), &semver::Version::new(0, 1, 0));

    // config/validate delegation (F-59).
    let ok = plugin.config_validate(serde_json::json!({})).await.unwrap();
    assert!(ok.valid);
    let bad = plugin
        .config_validate(serde_json::json!({ "invalid": true }))
        .await
        .unwrap();
    assert!(!bad.valid);
    assert!(!bad.errors.is_empty());

    plugin.shutdown(Duration::from_secs(5)).await.unwrap();
    assert!(plugin.is_closed());
}

#[tokio::test]
async fn protocol_mismatch_is_explicit() {
    // The mock (and orchestrator) protocol is 0.1.x; a plugin requiring >=1.0
    // must be rejected before any RPC (F-54).
    let err = Plugin::launch(spec(">=1.0.0")).await.unwrap_err();
    assert!(
        matches!(err, HostError::ProtocolMismatch { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn crash_fails_task_and_host_survives() {
    let db = StateDb::open_in_memory().unwrap();
    let task_id = db
        .upsert_task(&NewTask {
            source: "github".into(),
            source_task_id: "1".into(),
            workflow: "implement".into(),
            mode: "implement".into(),
            repo: None,
            priority: 0,
            title: "t".into(),
            url: None,
            source_payload: None,
        })
        .unwrap();
    db.apply_event(task_id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(task_id, TaskEvent::Start, None).unwrap();

    let plugin = Plugin::launch(spec("^0.1")).await.expect("launch");

    // The `crash` method makes the plugin exit without responding.
    let result: Result<serde_json::Value, _> = plugin.call("crash", &()).await;
    assert!(
        matches!(result, Err(HostError::Crashed(_))),
        "got {result:?}"
    );
    assert!(plugin.is_closed());

    // The caller reacts by failing the running task (§5.3).
    let state = db
        .apply_event(
            task_id,
            TaskEvent::Fail,
            Some(serde_json::json!({ "reason": "plugin crashed" })),
        )
        .unwrap();
    assert_eq!(state, TaskState::Failed);

    // The host process is unaffected: a fresh plugin still launches and works.
    let plugin2 = Plugin::launch(spec("^0.1")).await.expect("relaunch");
    assert!(
        plugin2
            .config_validate(serde_json::json!({}))
            .await
            .unwrap()
            .valid
    );
    plugin2.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[tokio::test]
async fn call_after_close_returns_promptly_not_after_timeout() {
    // A very long per-call timeout: if a call after close ever fell through to
    // the timeout path, this test would take ~30s. It must return quickly.
    let mut s = spec("^0.1");
    s.timeout = Duration::from_secs(30);
    let plugin = Plugin::launch(s).await.expect("launch");
    plugin.shutdown(Duration::from_secs(5)).await.unwrap();
    assert!(plugin.is_closed());

    let start = std::time::Instant::now();
    let result: Result<serde_json::Value, _> =
        plugin.call("config/validate", &serde_json::json!({})).await;
    assert!(result.is_err());
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a call on a closed plugin must not wait for the full timeout"
    );
}

#[tokio::test]
async fn receives_plugin_notifications() {
    let plugin = Plugin::launch(spec("^0.1")).await.expect("launch");
    let mut notifications = plugin
        .take_notifications()
        .await
        .expect("receiver available");

    // The mock emits a `state/notification` before acking `state/subscribe`.
    let _: serde_json::Value = plugin
        .call("state/subscribe", &serde_json::json!({ "session_id": "s" }))
        .await
        .unwrap();

    let note = tokio::time::timeout(Duration::from_secs(5), notifications.recv())
        .await
        .expect("notification did not arrive")
        .expect("notification channel closed");
    assert_eq!(note.method, "state/notification");
    let params = note.params.expect("params present");
    assert_eq!(params["state"], "running");
    assert_eq!(params["log_chunk"], "compiling...");

    plugin.shutdown(Duration::from_secs(5)).await.unwrap();
}

#[test]
fn disabled_plugins_are_not_launchable() {
    let cfg = RootConfig::from_toml_str(
        r#"
[plugins.github]
enabled = true
kind = "task_source"

[plugins.notion]
enabled = false
kind = "task_source"
"#,
    )
    .unwrap();
    let names = launchable_plugin_names(&cfg);
    assert_eq!(names, vec!["github".to_string()]);
    assert!(
        !names.contains(&"notion".to_string()),
        "F-58: disabled never launched"
    );
}
