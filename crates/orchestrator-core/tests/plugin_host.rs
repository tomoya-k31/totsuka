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
        repositories: vec![],
        llm: None,
        triggers: vec![],
        poll_interval_secs: None,
        timeout: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn initialize_carries_the_supplied_repositories_and_llm() {
    let dir = test_support::scratch("host_init_repos");
    let log = dir.join("init.ndjson");

    let mut with_supplies = spec(">=0.1.6, <0.3");
    with_supplies.init_config = serde_json::json!({ "init_log": log });
    with_supplies.repositories = vec![plugin_protocol::methods::RepoInfo {
        name: "web-app".into(),
        summary: Some("customer web app".into()),
        path: Some("/repos/web-app".into()),
    }];
    with_supplies.llm = Some(plugin_protocol::methods::LlmInfo {
        base_url: "https://openrouter.ai/api/v1".into(),
        model: "anthropic/claude-haiku-4.5".into(),
        api_key: Some("sk-or-resolved".into()),
    });
    let plugin = Plugin::launch(with_supplies).await.expect("launch");
    let _ = plugin.shutdown(Duration::from_secs(2)).await;

    let recorded = std::fs::read_to_string(&log).unwrap();
    let line: serde_json::Value = serde_json::from_str(recorded.lines().next().unwrap()).unwrap();
    assert_eq!(line["method"], "initialize");
    assert_eq!(line["params"]["repositories"][0]["name"], "web-app");
    assert_eq!(line["params"]["repositories"][0]["path"], "/repos/web-app");
    assert_eq!(line["params"]["llm"]["model"], "anthropic/claude-haiku-4.5");
    assert_eq!(line["params"]["llm"]["api_key"], "sk-or-resolved");

    // An empty list / unset llm is omitted from the wire entirely — an older
    // plugin never even sees an unknown field.
    let log = dir.join("init_empty.ndjson");
    let mut without = spec(">=0.1.6, <0.3");
    without.init_config = serde_json::json!({ "init_log": log });
    let plugin = Plugin::launch(without).await.expect("launch");
    let _ = plugin.shutdown(Duration::from_secs(2)).await;
    let recorded = std::fs::read_to_string(&log).unwrap();
    let line: serde_json::Value = serde_json::from_str(recorded.lines().next().unwrap()).unwrap();
    assert!(line["params"].get("repositories").is_none(), "{line}");
    assert!(line["params"].get("llm").is_none(), "{line}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn lifecycle_initialize_shutdown_and_config_validate() {
    let plugin = Plugin::launch(spec(">=0.1.6, <0.3")).await.expect("launch");

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
    // The orchestrator protocol is 0.2.x; a plugin requiring >=1.0 must be
    // rejected before any RPC (F-54).
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
            thread_key: None,
            last_signal_at: None,
        })
        .unwrap();
    db.apply_event(task_id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(task_id, TaskEvent::Start, None).unwrap();

    let plugin = Plugin::launch(spec(">=0.1.6, <0.3")).await.expect("launch");

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
    let plugin2 = Plugin::launch(spec(">=0.1.6, <0.3"))
        .await
        .expect("relaunch");
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
    let mut s = spec(">=0.1.6, <0.3");
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
    let plugin = Plugin::launch(spec(">=0.1.6, <0.3")).await.expect("launch");
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

/// Poll `path` until it records a `{"method": <method>, ...}` NDJSON line
/// (the mock's observation channel), failing after 5s.
async fn recorded_line(path: &std::path::Path, method: &str) -> serde_json::Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                    && v["method"] == method
                {
                    return v;
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no `{method}` line recorded in {path:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A spec whose mock emits the given plugin-initiated request right after
/// `initialize` (0.1.6) and records responses to `log`.
fn spec_with_request_on_init(log: &std::path::Path, request: serde_json::Value) -> PluginSpec {
    let mut s = spec(">=0.1.6, <0.3");
    s.init_config = serde_json::json!({ "notify_log": log, "request_on_init": request });
    s
}

#[tokio::test]
async fn plugin_initiated_request_is_surfaced_and_answered() {
    let dir = test_support::scratch("host_incoming_ok");
    let log = dir.join("notify.ndjson");
    let plugin = Plugin::launch(spec_with_request_on_init(
        &log,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "submit-1",
            "method": "task/submit",
            "params": { "task": { "id": "42", "source": "slack", "title": "t" } }
        }),
    ))
    .await
    .expect("launch");
    let mut notifications = plugin.take_notifications().await.expect("notif rx");
    let mut incoming = plugin.take_incoming_requests().await.expect("incoming rx");

    // The request is surfaced with method, params and a responder.
    let request = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("incoming request did not arrive")
        .expect("incoming channel closed");
    assert_eq!(request.method, "task/submit");
    assert_eq!(request.params.as_ref().expect("params")["task"]["id"], "42");

    // Bidirectional interleaving: an O→P call round-trips while the plugin's
    // own request is still unanswered.
    let ok = plugin.config_validate(serde_json::json!({})).await.unwrap();
    assert!(ok.valid);

    // Answer; the mock records the response it received, correlated by id.
    request
        .responder
        .ok(serde_json::json!({ "status": "accepted" }));
    let recorded = recorded_line(&log, "response").await;
    assert_eq!(recorded["params"]["id"], "submit-1");
    assert_eq!(recorded["params"]["result"]["status"], "accepted");

    // Pin the routing fix: a request (method + id) must never be misrouted
    // to the notification channel (the pre-0.1.6 behavior silently accepted
    // it as a `Notification` and dropped the id).
    assert!(
        notifications.try_recv().is_err(),
        "request must not appear as a notification"
    );

    plugin.shutdown(Duration::from_secs(5)).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn plugin_initiated_request_can_be_answered_with_an_error() {
    let dir = test_support::scratch("host_incoming_err");
    let log = dir.join("notify.ndjson");
    let plugin = Plugin::launch(spec_with_request_on_init(
        &log,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "task/submit",
            "params": { "task": { "id": "43", "source": "slack", "title": "t" } }
        }),
    ))
    .await
    .expect("launch");
    let mut incoming = plugin.take_incoming_requests().await.expect("incoming rx");
    let request = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("incoming request did not arrive")
        .expect("incoming channel closed");

    request.responder.err(plugin_protocol::jsonrpc::Error::new(
        plugin_protocol::error_code::SUBMIT_OVERLOADED,
        "submit budget exhausted → retry with backoff",
    ));
    let recorded = recorded_line(&log, "response").await;
    assert_eq!(recorded["params"]["id"], 7);
    assert_eq!(
        recorded["params"]["error"]["code"],
        plugin_protocol::error_code::SUBMIT_OVERLOADED
    );

    plugin.shutdown(Duration::from_secs(5)).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn malformed_plugin_request_gets_a_prompt_invalid_request_error() {
    let dir = test_support::scratch("host_incoming_malformed");
    let log = dir.join("notify.ndjson");
    // No `jsonrpc` field → not a valid `Request`; the host must answer with
    // INVALID_REQUEST (correlated by id) instead of silently dropping it and
    // leaving the plugin to wait out its own call timeout.
    let plugin = Plugin::launch(spec_with_request_on_init(
        &log,
        serde_json::json!({ "id": 9, "method": "task/submit" }),
    ))
    .await
    .expect("launch");

    let recorded = recorded_line(&log, "response").await;
    assert_eq!(recorded["params"]["id"], 9);
    assert_eq!(
        recorded["params"]["error"]["code"],
        plugin_protocol::error_code::INVALID_REQUEST
    );

    plugin.shutdown(Duration::from_secs(5)).await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn incoming_channel_closes_on_crash_and_late_answer_is_harmless() {
    let dir = test_support::scratch("host_incoming_crash");
    let log = dir.join("notify.ndjson");
    let plugin = Plugin::launch(spec_with_request_on_init(
        &log,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "submit-2",
            "method": "task/submit",
            "params": { "task": { "id": "44", "source": "slack", "title": "t" } }
        }),
    ))
    .await
    .expect("launch");
    let mut incoming = plugin.take_incoming_requests().await.expect("incoming rx");
    let request = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("incoming request did not arrive")
        .expect("incoming channel closed");

    // The plugin dies before its request is answered.
    let result: Result<serde_json::Value, _> = plugin.call("crash", &()).await;
    assert!(matches!(result, Err(HostError::Crashed(_))));

    // The reader task ended, so the incoming channel closes for its consumer…
    let next = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("channel close not observed");
    assert!(next.is_none(), "channel must close when the plugin exits");

    // …and answering the orphaned request is a harmless no-op, not a panic.
    request
        .responder
        .ok(serde_json::json!({ "status": "accepted" }));

    let _ = std::fs::remove_dir_all(&dir);
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
