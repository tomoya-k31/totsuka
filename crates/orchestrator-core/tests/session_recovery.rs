//! Integration test for restart recovery (F-37, §5.3), driving a real mock
//! plugin subprocess over NDJSON stdio: dispatch → persist session → simulate
//! SIGKILL → restart → `session/attach` → resume (or defer to a human).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::adapters::state_db::NewTask;
use orchestrator_core::adapters::{PluginAgentSession, StateDb};
use orchestrator_core::domain::state::{TaskEvent, TaskState};
use orchestrator_core::recovery::{RecoveryResult, recover};
use plugin_protocol::Task;
use plugin_protocol::manifest::Manifest;
use plugin_protocol::method;
use plugin_protocol::methods::{ExecutionMode, TaskDispatchParams, TaskDispatchResult};

fn spec() -> PluginSpec {
    PluginSpec {
        name: "mock".to_string(),
        program: PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin")),
        args: vec![],
        manifest: Manifest::from_toml_str(
            r#"
name = "mock"
kind = "agent_ide"
version = "0.1.0"
protocol_version = "^0.1"
"#,
        )
        .unwrap(),
        init_config: serde_json::json!({}),
        repositories: vec![],
        llm: None,
        timeout: Duration::from_secs(10),
    }
}

fn new_task(source_task_id: &str) -> NewTask {
    NewTask {
        source: "github".into(),
        source_task_id: source_task_id.into(),
        workflow: "implement".into(),
        mode: "implement".into(),
        repo: Some("totsuka".into()),
        priority: 0,
        title: "t".into(),
        url: None,
        source_payload: None,
    }
}

fn dispatch_task() -> Task {
    Task {
        id: "1".into(),
        source: "github".into(),
        title: "t".into(),
        body: None,
        repo_hint: None,
        labels: vec![],
        priority: 0,
        status: None,
        url: None,
        assignee: None,
    }
}

/// A single-plugin attacher map keyed by the plugin's instance name.
fn one_plugin(plugin: Plugin) -> HashMap<String, Plugin> {
    HashMap::from([("mock".to_string(), plugin)])
}

#[tokio::test]
async fn kill9_restart_attach_resumes_running() {
    let db = StateDb::open_in_memory().unwrap();
    let id = db.upsert_task(&new_task("1")).unwrap();

    // Dispatch through a plugin, persist the returned session, then start work.
    let plugin1 = Plugin::launch(spec()).await.expect("launch");
    let disp: TaskDispatchResult = plugin1
        .call(
            method::TASK_DISPATCH,
            &TaskDispatchParams {
                task: dispatch_task(),
                worktree_path: "/wt/agent-github-1".into(),
                mode: ExecutionMode::Implement,
                extra_context: None,
            },
        )
        .await
        .expect("dispatch");
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    db.record_session(id, "mock", &disp.session_id).unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();
    assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);

    // Simulate SIGKILL: drop the plugin process. The state DB survives.
    drop(plugin1);

    // Restart: a fresh plugin re-attaches to the persisted session.
    let plugins = one_plugin(Plugin::launch(spec()).await.expect("relaunch"));
    let attacher = PluginAgentSession::new(&plugins);
    let report = recover(&db, &attacher).await.unwrap();

    assert_eq!(report.resumed().count(), 1);
    assert!(matches!(
        report.outcomes[0].result,
        RecoveryResult::Resumed {
            state: TaskState::Running
        }
    ));
    assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);

    plugins["mock"]
        .shutdown(Duration::from_secs(5))
        .await
        .unwrap();
}

#[tokio::test]
async fn lost_session_defers_to_human_not_failed() {
    let db = StateDb::open_in_memory().unwrap();
    let id = db.upsert_task(&new_task("2")).unwrap();
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    // `sess-gone` makes the mock report `attached: false` (session lost).
    db.record_session(id, "mock", "sess-gone").unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();

    let plugins = one_plugin(Plugin::launch(spec()).await.expect("launch"));
    let attacher = PluginAgentSession::new(&plugins);
    let report = recover(&db, &attacher).await.unwrap();

    assert_eq!(report.needs_confirmation().count(), 1);
    // Not auto-failed (§5.3): the task keeps its state for the human to decide.
    assert_eq!(db.get_task(id).unwrap().unwrap().state, TaskState::Running);

    plugins["mock"]
        .shutdown(Duration::from_secs(5))
        .await
        .unwrap();
}
