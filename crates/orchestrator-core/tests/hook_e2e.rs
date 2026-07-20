//! End-to-end hook tests through the **real** UDS socket and the **real**
//! `Engine::run` loop (#141).
//!
//! Unlike `hook_integration.rs` — which drives `Engine::on_signal` directly to
//! unit-test the state machine — these tests wire the whole path: a hook-capable
//! `mock_plugin` is dispatched, injects a synthetic Stop over the real Unix
//! socket (`hook_post_on_dispatch`, the same JSON `on-stop.sh` emits), and the
//! `adapters::hook_uds` receiver → `SignalPort` → run-loop → `on_signal` chain
//! transitions the task with no direct call. The socket transport, the Bearer
//! check, and the `PluginEvent::HookSignal` wiring are all exercised for real.
//!
//! One-shot `run` (`watch = false`) settles once every dispatched task reaches a
//! terminal or waiting state, so each test terminates deterministically:
//! `Done`/`WaitingInput` free their slot (`counts_toward_slot` is false), while a
//! still-`Running` task would keep the loop alive — hence these cover only the
//! settling outcomes (COMPLETED → Done, NEEDS_INPUT → WaitingInput). The
//! non-settling paths (UNKNOWN escalation, timeout, Verifying) are covered by
//! `hook_integration.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orchestrator_core::adapters::StateDb;
use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::OpenAiRouter;
use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::state::TaskState;
use orchestrator_core::domain::workflow::Workflow;
use orchestrator_core::ports::SecretString;
use orchestrator_core::repo_select::SelectConfig;
use orchestrator_core::run::{Engine, EngineSettings, HookRuntime, PluginSet, RepoSettings};
use orchestrator_core::scheduler::Limits;
use orchestrator_core::worktree::{CleanupPolicy, DEFAULT_BRANCH_TEMPLATE};
use plugin_protocol::manifest::Manifest;
use serde_json::json;
use test_support::{bare_origin_and_clone as setup_repo, scratch};

/// A safety-net ceiling so a wiring regression fails the test instead of hanging
/// CI: a healthy run settles far inside this.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);
const GRACE: Duration = Duration::from_secs(5);

fn mock_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin"))
}

async fn launch(kind: &str, name: &str, init_config: serde_json::Value) -> Plugin {
    let manifest = Manifest::from_toml_str(&format!(
        r#"
name = "{name}"
kind = "{kind}"
version = "0.1.0"
protocol_version = ">=0.1.6, <0.3"
"#
    ))
    .unwrap();
    Plugin::launch(PluginSpec {
        name: name.to_string(),
        program: mock_plugin(),
        args: vec![],
        manifest,
        init_config,
        repositories: vec![],
        llm: None,
        triggers: vec![],
        poll_interval_secs: None,
        timeout: Duration::from_secs(10),
    })
    .await
    .expect("launch mock plugin")
}

fn no_llm() -> Option<OpenAiRouter> {
    None
}

fn read_log(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// One workflow `wf` (source `mock_src`, agent `mock_agent`) at the given
/// verification mode; output `none` so a COMPLETED finalizes without a PR.
fn workflows(verification: &str) -> Vec<Workflow> {
    let cfg = RootConfig::from_toml_str(&format!(
        r#"
[[workflows]]
name = "wf"
source = "mock_src"
trigger = {{}}
mode = "implement"
agent = "mock_agent"
output = "none"
verification = "{verification}"
on_success = {{ set_status = "done" }}
on_failure = {{ set_status = "failed" }}
"#
    ))
    .unwrap();
    Workflow::from_configs(&cfg.workflows)
}

/// Engine settings on a real repo clone with a hook runtime bound to `socket`.
fn settings(repo: &Path, socket: &Path, base: &Path) -> EngineSettings {
    let hook = HookRuntime {
        socket_path: socket.to_path_buf(),
        auth_token: Some(SecretString::new("e2e-token")),
        spool_dir: Some(base.join("spool")),
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    EngineSettings {
        workflows: workflows("llm"),
        repos: vec![RepoSettings {
            name: "clone".to_string(),
            path: repo.to_path_buf(),
            summary: None,
            worktree_location: None,
        }],
        limits: Limits::global(4),
        branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
        location_template: "{repo}/../wt/{branch}".to_string(),
        cleanup_implement: CleanupPolicy::Manual,
        cleanup_plan: CleanupPolicy::Immediate,
        env: HashMap::new(),
        select: SelectConfig::default(),
        readme_cache_dir: None,
        pr_title_template: "t: {title}".to_string(),
        pr_body_template: "{summary}".to_string(),
        hook: Some(hook),
    }
}

/// A hook-capable agent (`resume_session` takes the hook-dispatch path) that
/// POSTs the given synthetic Stop over the socket on dispatch, a source that
/// hands back one task, and a recording notifier. `stream_states` is the
/// mock agent's `state/subscribe` sequence — pass `[]` when the test only
/// cares about the hook-signal path, so a delayed `running` notification
/// can never race a hook signal that already parked/finished the task (a
/// `running` report legitimately resumes `WaitingInput`, so an
/// out-of-order one after a real hook signal would spuriously undo it).
async fn plugins(
    hook_spec: serde_json::Value,
    stream_states: &[&str],
    notify_log: &Path,
) -> PluginSet {
    let mut set = PluginSet::default();
    set.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({
                "task_submit": true,
                "submit_tasks": [{ "id": "1", "source": "github", "title": "hook task" }],
            }),
        )
        .await,
    );
    set.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({
                "resume_session": true,
                "stream_states": stream_states,
                "hook_post_on_dispatch": hook_spec,
            }),
        )
        .await,
    );
    set.notifiers.insert(
        "mock_notify".to_string(),
        launch(
            "notifier",
            "mock_notify",
            json!({ "notify_log": notify_log }),
        )
        .await,
    );
    set
}

/// Drive a watch-mode run until `cond` holds (checked every 100ms, capped by
/// `RUN_TIMEOUT`), then stop it and return the engine for state assertions.
/// The task is pushed via `task/submit`, arriving as an event on its own
/// schedule, so `cond` observes durable state (e.g. re-opening the state DB)
/// rather than borrowing `engine`, which `run` holds mutably for the loop's
/// duration.
async fn run_until(engine: &mut Engine<SystemGitRunner, OpenAiRouter>, cond: impl Fn() -> bool) {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut stop_tx = Some(stop_tx);
    let run_fut = engine.run(true, async move {
        let _ = stop_rx.await;
    });
    tokio::pin!(run_fut);
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        tokio::select! {
            result = &mut run_fut => {
                result.expect("run loop error");
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if stop_tx.is_some() && cond() {
                    let _ = stop_tx.take().unwrap().send(());
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "condition not reached within {}s — the socket→engine wiring may be broken",
                    RUN_TIMEOUT.as_secs()
                );
            }
        }
    }
}

#[tokio::test]
async fn e2e_socket_completion_dispatches_to_done() {
    // Dispatch → agent POSTs COMPLETED over the real UDS socket → receiver →
    // SignalPort → run loop → on_signal → Done, with no direct engine call.
    let base = scratch("e2e_completion");
    let repo = setup_repo(&base);
    let socket = base.join("claude.sock");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();

    let db_path = base.join("state.db");
    let mut engine = Engine::new(
        db,
        settings(&repo, &socket, &base),
        plugins(
            json!({ "status": "COMPLETED", "message": "shipped <<STATUS:COMPLETED>>" }),
            &["running"],
            &notify_log,
        )
        .await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    run_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;

    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(
        task.state,
        TaskState::Done,
        "the socket-delivered COMPLETED finalizes the task"
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes.iter().any(|n| n["params"]["event"] == "done"),
        "done notification delivered end-to-end: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn e2e_socket_duplicate_delivery_transitions_once() {
    // The agent POSTs the identical Stop twice (`repeat: 2`): both cross the
    // real socket, but the hook_events UNIQUE key (D-05) drops the second, so
    // the task transitions — and notifies — exactly once.
    let base = scratch("e2e_duplicate");
    let repo = setup_repo(&base);
    let socket = base.join("claude.sock");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");
    let db = StateDb::open(&db_path).unwrap();

    let mut engine = Engine::new(
        db,
        settings(&repo, &socket, &base),
        plugins(
            json!({ "status": "COMPLETED", "message": "done <<STATUS:COMPLETED>>", "repeat": 2 }),
            &["running"],
            &notify_log,
        )
        .await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    run_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;

    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Done);
    engine.shutdown(GRACE).await;

    let done_notes = read_log(&notify_log)
        .into_iter()
        .filter(|n| n["params"]["event"] == "done")
        .count();
    assert_eq!(
        done_notes, 1,
        "the duplicate socket delivery must not double-notify"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn e2e_socket_needs_input_parks_in_waiting() {
    // A NEEDS_INPUT Stop over the socket parks the task in WaitingInput and
    // notifies. `stream_states: []` (no state/subscribe traffic) so a
    // delayed `running` report can never race the hook signal and undo it
    // (`apply_agent_state`'s `Running` branch legitimately resumes
    // `WaitingInput`, since that's how a real post-answer resume works).
    let base = scratch("e2e_needs_input");
    let repo = setup_repo(&base);
    let socket = base.join("claude.sock");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");
    let db = StateDb::open(&db_path).unwrap();

    let mut engine = Engine::new(
        db,
        settings(&repo, &socket, &base),
        plugins(
            json!({ "status": "NEEDS_INPUT", "message": "which branch? <<STATUS:NEEDS_INPUT reason=\"branch?\">>" }),
            &[],
            &notify_log,
        )
        .await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    run_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::WaitingInput)
    })
    .await;

    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(
        task.state,
        TaskState::WaitingInput,
        "a socket NEEDS_INPUT parks the task awaiting a human"
    );
    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes
            .iter()
            .any(|n| n["params"]["event"] == "waiting_input"),
        "waiting_input notification delivered end-to-end: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}
