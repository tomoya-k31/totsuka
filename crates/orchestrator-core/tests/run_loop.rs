//! Integration tests for the run main loop (#63) against real mock-plugin
//! subprocesses and a real git repository.
//!
//! Covers the issue's acceptance criteria (updated for 0.2.0's push-only
//! ingestion, #190 — every source pushes via `task/submit` instead of the
//! removed `tasks/fetch`):
//! 1. push → worktree → dispatch → done → cleanup, end to end.
//! 2. One-shot leaves waiting tasks in place (double-ingest is covered
//!    separately by `duplicate_submit_is_acked_duplicate_and_ingested_once`,
//!    since push re-delivery — not a fetch rerun — is how a duplicate can
//!    arrive).
//! 3. A restart after an interrupted run recovers the in-flight task (§5.3).
//! 4. `--dry-run` is a zero-side-effect no-op (push sources have nothing to
//!    preview ahead of time).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orchestrator_core::adapters::StateDb;
use orchestrator_core::adapters::clock::ManualClock;
use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::OpenAiRouter;
use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::state::TaskState;
use orchestrator_core::domain::workflow::Workflow;
use orchestrator_core::repo_select::SelectConfig;
use orchestrator_core::run::output::{PrCreator, PrError, PrRequest};
use orchestrator_core::run::{Engine, EngineSettings, PluginSet, RepoSettings};
use orchestrator_core::scheduler::Limits;
use orchestrator_core::worktree::{CleanupPolicy, DEFAULT_BRANCH_TEMPLATE};
use plugin_protocol::manifest::Manifest;
use serde_json::json;
use std::sync::{Arc, Mutex};
use test_support::{bare_origin_and_clone as setup_repo, git, scratch};

/// A pull-request creator that records requests and returns a canned URL, so
/// the push flow is exercised without a real `gh`/GitHub.
#[derive(Clone, Default)]
struct RecordingPrCreator {
    requests: Arc<Mutex<Vec<PrRequest>>>,
    fail: bool,
}

impl PrCreator for RecordingPrCreator {
    fn create_pr(&self, req: &PrRequest) -> Result<String, PrError> {
        self.requests.lock().unwrap().push(req.clone());
        if self.fail {
            Err(PrError::Failed("mock refused".into()))
        } else {
            Ok("https://example.com/pr/1".to_string())
        }
    }
}

/// Path to the compiled mock plugin binary.
fn mock_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin"))
}

/// Launch the mock plugin under a manifest of the given kind.
async fn launch(kind: &str, name: &str, init_config: serde_json::Value) -> Plugin {
    let manifest = Manifest::from_toml_str(&format!(
        r#"
name = "{name}"
kind = "{kind}"
version = "0.1.0"
protocol_version = ">=0.1.6, <0.4"
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

/// The single test workflow: everything from `mock_src` runs on `mock_agent`.
fn workflows() -> Vec<Workflow> {
    let cfg = RootConfig::from_toml_str(
        r#"
[[workflows]]
name = "implement"
source = "mock_src"
trigger = {}
mode = "implement"
agent = "mock_agent"
output = "none"
on_success = { set_status = "レビュー待ち" }
"#,
    )
    .unwrap();
    Workflow::from_configs(&cfg.workflows)
}

/// Engine settings over one repo, worktrees under `<base>/wt/`, immediate
/// cleanup.
fn engine_settings(repo_path: &Path) -> EngineSettings {
    EngineSettings {
        workflows: workflows(),
        repos: vec![RepoSettings {
            name: "clone".to_string(),
            path: repo_path.to_path_buf(),
            summary: None,
            worktree_location: None,
            tool: None,
        }],
        limits: Limits::global(2),
        branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
        location_template: "{repo}/../wt/{branch}".to_string(),
        cleanup_implement: CleanupPolicy::Immediate,
        cleanup_plan: CleanupPolicy::Immediate,
        env: HashMap::new(),
        select: SelectConfig::default(),
        readme_cache_dir: None,
        pr_title_template: "totsuka: {title}".to_string(),
        pr_body_template: "Task {title} ({url})\n\n{summary}".to_string(),
        // Sweep every cycle, as before the interval existed (#210).
        worktree_sweep_interval: Duration::ZERO,
        // Nothing arrives asynchronously here: the mock source's `task/submit`
        // is driven in-process and every one-shot `run(false, ..)` below is
        // preceded by an explicit seed, so production's 2s quiet-period floor
        // would be pure waiting (#281). `settled()` still has to hold.
        one_shot_grace: Duration::ZERO,
        tools: orchestrator_core::tool::builtin_registry(),
        default_tool: "claude".to_string(),
        hook: None,
    }
}

/// Workflows for one `mode` × `output` combination (source `mock_src`, agent
/// `mock_agent`).
fn workflows_with(mode: &str, output: &str) -> Vec<Workflow> {
    let cfg = RootConfig::from_toml_str(&format!(
        r#"
[[workflows]]
name = "wf"
source = "mock_src"
trigger = {{}}
mode = "{mode}"
agent = "mock_agent"
output = "{output}"
on_success = {{ set_status = "レビュー待ち" }}
on_failure = {{ set_status = "失敗" }}
"#
    ))
    .unwrap();
    Workflow::from_configs(&cfg.workflows)
}

/// One pushable task in the mock source's config shape. The `source` field
/// deliberately differs from the plugin *instance* name (`mock_src`): real
/// plugins stamp their own source name (e.g. `github` whatever the instance
/// is called), and the engine must normalize it before matching.
fn mock_task(id: &str) -> serde_json::Value {
    json!({ "id": id, "source": "github", "title": format!("task {id}") })
}

/// Build a full plugin set: source (pushing `tasks` via `task/submit` right
/// after `initialize`, 0.1.6+), agent (config-driven), notifier (recording
/// to `notify_log`). `tasks` may be empty — a source that submits nothing.
async fn plugin_set(
    tasks: serde_json::Value,
    agent_config: serde_json::Value,
    source_log: &Path,
    notify_log: &Path,
) -> PluginSet {
    let mut set = PluginSet::default();
    set.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": tasks, "notify_log": source_log }),
        )
        .await,
    );
    set.agents.insert(
        "mock_agent".to_string(),
        launch("agent_ide", "mock_agent", agent_config).await,
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

/// No LLM in tests: repo selection resolves via the single-candidate rule.
fn no_llm() -> Option<OpenAiRouter> {
    None
}

/// Read a recorded NDJSON log (empty if never written).
fn read_log(path: &Path) -> Vec<serde_json::Value> {
    test_support::read_ndjson_log(path)
}

#[tokio::test]
async fn full_path_fetch_worktree_dispatch_done_cleanup() {
    let base = scratch("full_path");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({ "stream_states": ["running", "done"] }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;

    assert_eq!(summary.stats.submitted, 1);
    assert_eq!(summary.stats.dispatched, 1);
    assert_eq!(summary.stats.done, 1);
    assert_eq!(summary.stats.failed, 0);
    assert!(summary.waiting.is_empty() && summary.pending.is_empty());

    engine.shutdown(Duration::from_secs(5)).await;

    // The task is done, with its session and worktree recorded.
    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    assert_eq!(task.state, TaskState::Done);
    assert!(task.finished_at.is_some());
    assert_eq!(task.repo.as_deref(), Some("clone"));
    let session = db.latest_session(task.id).unwrap().unwrap();
    assert_eq!(session.session_id, "sess-mock");
    assert_eq!(session.plugin, "mock_agent");

    // Immediate cleanup removed the worktree directory (F-23).
    let worktree = PathBuf::from(task.worktree_path.unwrap());
    assert!(
        !worktree.exists(),
        "worktree must be cleaned up: {}",
        worktree.display()
    );

    // on_success wrote the status back to the source (F-84).
    let source_calls = read_log(&source_log);
    assert!(
        source_calls.iter().any(|c| c["method"] == "task/update_status"
            && c["params"]["status"] == "レビュー待ち"),
        "expected task/update_status: {source_calls:?}"
    );

    // The notifier saw the done event with the workflow attached (F-90/F-92).
    let notifications = read_log(&notify_log);
    assert!(
        notifications
            .iter()
            .any(|n| n["params"]["event"] == "done" && n["params"]["workflow"] == "implement"),
        "expected done notification: {notifications:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The loop settles once a dispatched task reaches `waiting_input`: it is
/// not "actively executing" (§5.1), so the run stops with it still listed
/// in `summary.waiting`, and the notifier fires. Idempotent re-submission
/// of the same task id is covered separately by
/// `duplicate_submit_is_acked_duplicate_and_ingested_once`.
#[tokio::test]
async fn run_settles_with_waiting_task_left_in_place() {
    let base = scratch("waiting");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("7")]),
        json!({ "stream_states": ["running", "waiting_input"] }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "7")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::WaitingInput)
    })
    .await;
    assert_eq!(summary.stats.submitted, 1);
    assert_eq!(summary.stats.dispatched, 1);
    assert_eq!(summary.waiting.len(), 1, "waiting task remains (§5.1)");

    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "7").unwrap().unwrap();
    assert_eq!(task.state, TaskState::WaitingInput);
    assert_eq!(db.list_tasks().unwrap().len(), 1);
    assert_eq!(db.list_sessions(task.id).unwrap().len(), 1);

    // The waiting_input event reached the notifier (F-35/F-90).
    let notifications = read_log(&notify_log);
    assert!(
        notifications
            .iter()
            .any(|n| n["params"]["event"] == "waiting_input"),
        "expected waiting_input notification: {notifications:?}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn restart_recovers_in_flight_task() {
    let base = scratch("recovery");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // First process: dispatch, then die before the task finishes (the mock
    // agent only ever reports `running`, so waiting for that state is
    // non-racy — it is the last thing the mock will ever report).
    {
        let plugins = plugin_set(
            json!([mock_task("9")]),
            json!({ "stream_states": ["running"] }),
            &source_log,
            &notify_log,
        )
        .await;
        let mut engine = Engine::new(
            StateDb::open(&db_path).unwrap(),
            engine_settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        let db_probe = db_path.clone();
        // Dropping the engine right after simulates SIGKILL (no graceful
        // shutdown).
        run_watch_until(&mut engine, move || {
            StateDb::open(&db_probe)
                .unwrap()
                .find_by_source("mock_src", "9")
                .unwrap()
                .is_some_and(|t| t.state == TaskState::Running)
        })
        .await;
        let db = StateDb::open(&db_path).unwrap();
        let task = db.find_by_source("mock_src", "9").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Running);
    }

    // Restart: fresh plugins, fresh engine over the same state DB. Recovery
    // re-attaches to `sess-mock` (the mock reports it running) and syncs the
    // state machine forward (§5.3).
    let plugins = plugin_set(
        json!([]),
        json!({ "stream_states": ["running"] }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let report = engine.recover().await.unwrap();
    assert_eq!(report.resumed().count(), 1);
    assert_eq!(report.needs_confirmation().count(), 0);

    let task = engine
        .db()
        .find_by_source("mock_src", "9")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Running, "resumed and synced forward");

    engine.shutdown(Duration::from_secs(5)).await;
    let _ = std::fs::remove_dir_all(&base);
}

/// `--dry-run` never touches the event loop, so a push source's pending
/// `task/submit` (already queued by the time the plugin's `initialize`
/// returns) is simply never consumed: nothing is fetched ahead of time
/// since every source is push-only (0.2.0), so `dry_run` always reports an
/// empty preview with zero side effects.
#[tokio::test]
async fn dry_run_has_no_preview_and_zero_side_effects() {
    let base = scratch("dry_run");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("3")]),
        json!({ "stream_states": ["running", "done"] }),
        &source_log,
        &notify_log,
    )
    .await;
    let engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let entries = engine.dry_run().await.unwrap();
    assert!(entries.is_empty(), "push sources cannot be previewed");

    // Zero side effects: nothing ingested, no worktree, no notifications, no
    // source write-backs — the queued task/submit is never consumed.
    assert!(engine.db().list_tasks().unwrap().is_empty());
    assert!(!base.join("wt").exists());
    engine.shutdown(Duration::from_secs(5)).await;
    assert!(read_log(&notify_log).is_empty());
    assert!(read_log(&source_log).is_empty());

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn unrecoverable_task_does_not_wedge_one_shot_exit() {
    let base = scratch("needs_confirmation");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // First process: dispatch under a session id the mock will report as gone
    // after restart, then die.
    {
        let plugins = plugin_set(
            json!([mock_task("11")]),
            json!({ "stream_states": [], "session_id": "sess-gone-11" }),
            &source_log,
            &notify_log,
        )
        .await;
        let mut engine = Engine::new(
            StateDb::open(&db_path).unwrap(),
            engine_settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        let db_probe = db_path.clone();
        // `stream_states: []` means the mock never reports past dispatch, so
        // waiting for `Dispatched` is non-racy.
        run_watch_until(&mut engine, move || {
            StateDb::open(&db_probe)
                .unwrap()
                .find_by_source("mock_src", "11")
                .unwrap()
                .is_some_and(|t| t.state == TaskState::Dispatched)
        })
        .await;
    }

    // Restart: the session is lost → needs confirmation (§5.3), and the
    // one-shot loop must still exit instead of waiting forever for a
    // notification that can never arrive.
    let plugins = plugin_set(json!([]), json!({}), &source_log, &notify_log).await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let report = engine.recover().await.unwrap();
    assert_eq!(report.needs_confirmation().count(), 1);

    let summary = tokio::time::timeout(
        Duration::from_secs(30),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("one-shot must exit despite the unrecoverable task")
    .unwrap();
    assert!(!summary.interrupted);

    // The task is left for the human (not auto-failed, §5.3).
    let task = engine
        .db()
        .find_by_source("mock_src", "11")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Dispatched);

    engine.shutdown(Duration::from_secs(5)).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn task_finished_while_down_is_finalized_on_recovery() {
    let base = scratch("recover_done");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // First process: dispatch under a session id the mock will report as
    // `done` on re-attach, then die before any notification is processed.
    {
        let plugins = plugin_set(
            json!([mock_task("13")]),
            json!({ "stream_states": [], "session_id": "sess-done-13" }),
            &source_log,
            &notify_log,
        )
        .await;
        let mut engine = Engine::new(
            StateDb::open(&db_path).unwrap(),
            engine_settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        let db_probe = db_path.clone();
        // `stream_states: []` means the mock never reports past dispatch, so
        // waiting for `Dispatched` is non-racy.
        run_watch_until(&mut engine, move || {
            StateDb::open(&db_probe)
                .unwrap()
                .find_by_source("mock_src", "13")
                .unwrap()
                .is_some_and(|t| t.state == TaskState::Dispatched)
        })
        .await;
    }

    // Restart: re-attach reports Done. Agents do not replay terminal states on
    // re-subscribe, so recovery itself must finalize: complete + write-back +
    // cleanup + notify.
    let plugins = plugin_set(json!([]), json!({}), &source_log, &notify_log).await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let report = engine.recover().await.unwrap();
    assert_eq!(report.resumed().count(), 1);

    let task = engine
        .db()
        .find_by_source("mock_src", "13")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Done, "finalized during recovery");
    let worktree = PathBuf::from(task.worktree_path.clone().unwrap());
    assert!(!worktree.exists(), "worktree cleaned up during recovery");

    // And the loop exits immediately: nothing is left in flight.
    let summary = tokio::time::timeout(
        Duration::from_secs(30),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("one-shot settles right away")
    .unwrap();
    assert!(summary.waiting.is_empty() && summary.queued.is_empty());

    engine.shutdown(Duration::from_secs(5)).await;
    let notifications = read_log(&notify_log);
    assert!(
        notifications.iter().any(|n| n["params"]["event"] == "done"),
        "done notification delivered on recovery finalize: {notifications:?}"
    );
    let source_calls = read_log(&source_log);
    assert!(
        source_calls
            .iter()
            .any(|c| c["method"] == "task/update_status"),
        "on_success write-back delivered on recovery finalize: {source_calls:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn agent_without_state_stream_fails_dispatch_instead_of_hanging() {
    let base = scratch("no_stream");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("17")]),
        json!({ "no_state_stream": true }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    // Progress could never be observed → the dispatch must fail the task and
    // the run must exit (not hold the slot forever).
    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "17")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Failed)
    })
    .await;
    assert_eq!(summary.stats.failed, 1);

    let task = engine
        .db()
        .find_by_source("mock_src", "17")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Failed);

    engine.shutdown(Duration::from_secs(5)).await;
    let notifications = read_log(&notify_log);
    assert!(
        notifications
            .iter()
            .any(|n| n["params"]["event"] == "failed"),
        "failed notification delivered: {notifications:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn output_pull_request_pushes_branch_and_opens_pr() {
    let base = scratch("pr");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // The mock agent commits in the worktree, then reports done.
    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({ "stream_states": ["running", "done"], "commit_on_dispatch": true }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.workflows = workflows_with("implement", "pull_request");
    let pr = RecordingPrCreator::default();
    let mut engine = Engine::with_pr_creator(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        Box::new(pr.clone()),
    )
    .await;

    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    assert_eq!(summary.stats.done, 1);
    assert_eq!(summary.stats.failed, 0);
    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Done);
    engine.shutdown(Duration::from_secs(5)).await;

    // The branch was pushed to the bare origin.
    let branches = String::from_utf8(
        Command::new("git")
            .current_dir(base.join("origin.git"))
            .args(["branch", "--list", "agent/*"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        branches.contains("agent/mock_src-1"),
        "branch pushed to origin: {branches:?}"
    );

    // The PR was opened with the templated title/body.
    let requests = pr.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].head_branch, "agent/mock_src-1");
    assert_eq!(requests[0].title, "totsuka: task 1");
    assert!(requests[0].body.contains("task 1"));
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn output_pull_request_with_zero_commits_fails() {
    let base = scratch("pr_nocommit");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // Agent reports done WITHOUT committing anything.
    let plugins = plugin_set(
        json!([mock_task("2")]),
        json!({ "stream_states": ["running", "done"] }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.workflows = workflows_with("implement", "pull_request");
    let pr = RecordingPrCreator::default();
    let mut engine = Engine::with_pr_creator(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        Box::new(pr.clone()),
    )
    .await;

    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "2")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Failed)
    })
    .await;
    assert_eq!(summary.stats.failed, 1, "zero-commit PR must fail");
    assert_eq!(summary.stats.done, 0);
    let task = engine
        .db()
        .find_by_source("mock_src", "2")
        .unwrap()
        .unwrap();
    engine.shutdown(Duration::from_secs(5)).await;

    // No PR attempted, and the worktree is KEPT for retry (issue #65).
    assert!(pr.requests.lock().unwrap().is_empty());
    assert_eq!(task.state, TaskState::Failed);
    let worktree = PathBuf::from(task.worktree_path.unwrap());
    assert!(worktree.exists(), "worktree kept for retry");

    // A recoverable publish failure must NOT flap the source status: on_failure
    // is not written back (it would revert on the next successful retry).
    let source_calls = read_log(&source_log);
    assert!(
        !source_calls
            .iter()
            .any(|c| c["method"] == "task/update_status"),
        "no source status write-back on a retryable publish failure: {source_calls:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn output_source_publishes_result_artifact() {
    let base = scratch("source_out");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // Plan mode + source output: the agent streams output, which is published.
    let plugins = plugin_set(
        json!([mock_task("5")]),
        json!({ "stream_states": ["running", "done"] }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.workflows = workflows_with("plan", "source");
    settings.cleanup_plan = CleanupPolicy::Immediate;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "5")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    assert_eq!(summary.stats.done, 1);
    let task = engine
        .db()
        .find_by_source("mock_src", "5")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Done);
    engine.shutdown(Duration::from_secs(5)).await;

    // The accumulated agent output flowed to result/publish (F-07).
    let source_calls = read_log(&source_log);
    let publish = source_calls
        .iter()
        .find(|c| c["method"] == "result/publish")
        .expect("result/publish called");
    assert_eq!(publish["params"]["task_id"], "5");
    assert!(
        publish["params"]["content"]
            .as_str()
            .unwrap()
            .contains("compiling"),
        "streamed agent output published: {publish}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn pull_request_retry_after_pr_failure_can_reopen() {
    // A partial publish (push succeeds, PR creation fails) must be retryable:
    // has_commits_to_publish compares against origin's default branch, so it
    // stays true even though the task's own branch is now on origin.
    let base = scratch("pr_retry");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({ "stream_states": ["running", "done"], "commit_on_dispatch": true }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.workflows = workflows_with("implement", "pull_request");
    // First attempt: PR creation fails after the push succeeds.
    let failing_pr = RecordingPrCreator {
        fail: true,
        ..Default::default()
    };
    let mut engine = Engine::with_pr_creator(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        Box::new(failing_pr.clone()),
    )
    .await;

    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Failed)
    })
    .await;
    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Failed, "PR failure fails the task");
    engine.shutdown(Duration::from_secs(5)).await;
    // The branch WAS pushed, and the PR was attempted once.
    assert_eq!(failing_pr.requests.lock().unwrap().len(), 1);

    // Retry: the branch is already on origin. A fresh engine re-dispatches
    // (reusing the worktree + session) and the agent re-reports done. The
    // commit check must still see the agent's commit (vs origin/main), so the
    // PR is attempted again — this time it succeeds.
    let task_id = task.id;
    StateDb::open(&db_path)
        .unwrap()
        .apply_event(
            task_id,
            orchestrator_core::domain::state::TaskEvent::Retry,
            None,
        )
        .unwrap();

    let plugins = plugin_set(
        json!([]),
        json!({ "stream_states": ["running", "done"], "commit_on_dispatch": false }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.workflows = workflows_with("implement", "pull_request");
    let ok_pr = RecordingPrCreator::default();
    let mut engine = Engine::with_pr_creator(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        Box::new(ok_pr.clone()),
    )
    .await;
    tokio::time::timeout(
        Duration::from_secs(60),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("settles")
    .unwrap();
    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(
        task.state,
        TaskState::Done,
        "retry reopens the PR and completes (not stuck on zero-commit)"
    );
    engine.shutdown(Duration::from_secs(5)).await;
    assert_eq!(
        ok_pr.requests.lock().unwrap().len(),
        1,
        "PR reattempted on retry"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn missing_workflow_at_finalize_keeps_worktree_not_deletes() {
    // A finished task whose workflow was removed from config must not be
    // silently completed with its committed worktree deleted.
    let base = scratch("missing_wf");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    // `sess-done` so the recovery re-attach reports the agent already done →
    // the Publishing task resumes and finalizes.
    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({ "stream_states": [], "session_id": "sess-done", "commit_on_dispatch": true }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.workflows = workflows_with("implement", "pull_request");
    // Never clean up implement worktrees, so we can assert it survives.
    settings.cleanup_implement = CleanupPolicy::Manual;
    let mut engine = Engine::with_pr_creator(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        Box::new(RecordingPrCreator::default()),
    )
    .await;

    // Dispatch (records the session), then shut down before finalize.
    // `stream_states: []` means the mock never reports past dispatch, so
    // waiting for `Dispatched` is non-racy.
    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Dispatched)
    })
    .await;
    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    let worktree = PathBuf::from(task.worktree_path.clone().unwrap());
    // Force the task into Publishing (agent done) directly, then finalize with a
    // config that no longer has the workflow.
    db.apply_event(
        task.id,
        orchestrator_core::domain::state::TaskEvent::Start,
        None,
    )
    .ok();
    db.apply_event(
        task.id,
        orchestrator_core::domain::state::TaskEvent::BeginPublish,
        None,
    )
    .unwrap();

    let plugins = plugin_set(json!([]), json!({}), &source_log, &notify_log).await;
    let mut settings = engine_settings(&repo);
    settings.workflows = Vec::new(); // workflow removed
    settings.cleanup_implement = CleanupPolicy::Manual;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    // Recovery finalizes the Publishing task; with no workflow it must fail
    // (keep worktree), not complete+delete.
    engine.recover().await.unwrap();
    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(task.state, TaskState::Failed);
    assert!(
        worktree.exists(),
        "committed worktree preserved, not deleted"
    );
    engine.shutdown(Duration::from_secs(5)).await;
    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Push ingestion (`task/submit`, 0.1.6 — #185)
// ---------------------------------------------------------------------------

/// Drive a watch-mode run until `cond` holds (checked every 100ms, 60s cap),
/// then stop it and return the summary. Push submissions arrive as events on
/// their own schedule, so tests wait on observable state instead of relying
/// on one-shot settling.
async fn run_watch_until(
    engine: &mut Engine<SystemGitRunner, OpenAiRouter>,
    cond: impl Fn() -> bool,
) -> orchestrator_core::run::RunSummary {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut stop_tx = Some(stop_tx);
    let run_fut = engine.run(true, async move {
        let _ = stop_rx.await;
    });
    tokio::pin!(run_fut);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        tokio::select! {
            summary = &mut run_fut => return summary.unwrap(),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if stop_tx.is_some() && cond() {
                    let _ = stop_tx.take().unwrap().send(());
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "condition not reached within 60s"
                );
            }
        }
    }
}

/// The `task/submit` acks the mock source recorded (`{"method":"response"}`
/// lines in its notify_log), as `(id, status-or-error-code)` pairs.
fn recorded_acks(source_log: &Path) -> Vec<(String, String)> {
    read_log(source_log)
        .into_iter()
        .filter(|l| l["method"] == "response")
        .map(|l| {
            let id = l["params"]["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| l["params"]["id"].to_string());
            let status = l["params"]["result"]["status"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| l["params"]["error"]["code"].to_string());
            (id, status)
        })
        .collect()
}

#[tokio::test]
async fn submitted_task_is_persisted_acked_and_dispatched_without_polling() {
    let base = scratch("submit_full_path");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let db_path = base.join("state.db");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            // A push source: submits one task at initialize, declares
            // task_submit.
            json!({
                "task_submit": true,
                "submit_tasks": [mock_task("s1")],
                "notify_log": source_log,
            }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "stream_states": ["running", "done"] }),
        )
        .await,
    );
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    let summary = run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "s1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;

    // Persist-before-ack: the source received a final `accepted` ack.
    assert_eq!(
        recorded_acks(&source_log),
        vec![("submit-0".to_string(), "accepted".to_string())]
    );
    // Counted as a submission; there is no fetch path to run (not polled).
    assert_eq!(summary.stats.submitted, 1);
    assert_eq!(summary.stats.dispatched, 1);

    engine.shutdown(Duration::from_secs(5)).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn duplicate_submit_is_acked_duplicate_and_ingested_once() {
    let base = scratch("submit_duplicate");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let db_path = base.join("state.db");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            // The same task twice: a re-submit after a lost ack must be
            // answered `duplicate`, never ingested twice.
            json!({
                "task_submit": true,
                "submit_tasks": [mock_task("d1"), mock_task("d1")],
                "notify_log": source_log,
            }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "stream_states": ["running", "done"] }),
        )
        .await,
    );
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let probe = source_log.clone();
    let summary = run_watch_until(&mut engine, move || recorded_acks(&probe).len() == 2).await;

    assert_eq!(
        recorded_acks(&source_log),
        vec![
            ("submit-0".to_string(), "accepted".to_string()),
            ("submit-1".to_string(), "duplicate".to_string()),
        ]
    );
    assert_eq!(summary.stats.submitted, 1, "duplicates must not count");

    engine.shutdown(Duration::from_secs(5)).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn submit_without_matching_workflow_is_rejected() {
    let base = scratch("submit_rejected");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let db_path = base.join("state.db");

    let mut plugins = PluginSet::default();
    // No [[workflows]] entry references `stray_src`.
    plugins.sources.insert(
        "stray_src".to_string(),
        launch(
            "task_source",
            "stray_src",
            json!({
                "task_submit": true,
                "submit_tasks": [mock_task("r1")],
                "notify_log": source_log,
            }),
        )
        .await,
    );
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let probe = source_log.clone();
    let _ = run_watch_until(&mut engine, move || !recorded_acks(&probe).is_empty()).await;

    let acks = recorded_acks(&source_log);
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0], ("submit-0".to_string(), "rejected".to_string()));
    // Rejected → never persisted.
    let db = StateDb::open(&db_path).unwrap();
    assert!(db.find_by_source("stray_src", "r1").unwrap().is_none());

    engine.shutdown(Duration::from_secs(5)).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn restart_dispatches_persisted_but_undispatched_submission() {
    let base = scratch("submit_replay");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let db_path = base.join("state.db");

    // Engine 1: zero slots — the submission is persisted (and acked) but can
    // never dispatch, simulating a crash between persist and dispatch.
    {
        let mut plugins = PluginSet::default();
        plugins.sources.insert(
            "mock_src".to_string(),
            launch(
                "task_source",
                "mock_src",
                json!({
                    "task_submit": true,
                    "submit_tasks": [mock_task("s9")],
                    "notify_log": source_log,
                }),
            )
            .await,
        );
        plugins.agents.insert(
            "mock_agent".to_string(),
            launch(
                "agent_ide",
                "mock_agent",
                json!({ "stream_states": ["running", "done"] }),
            )
            .await,
        );
        let mut settings = engine_settings(&repo);
        settings.limits = Limits::global(0);
        let mut engine = Engine::new(
            StateDb::open(&db_path).unwrap(),
            settings,
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        let db_probe = db_path.clone();
        let summary = run_watch_until(&mut engine, move || {
            StateDb::open(&db_probe)
                .unwrap()
                .find_by_source("mock_src", "s9")
                .unwrap()
                .is_some()
        })
        .await;
        assert_eq!(summary.stats.submitted, 1);
        assert_eq!(summary.stats.dispatched, 0);
        engine.shutdown(Duration::from_secs(5)).await;
    }

    // Engine 2 over the same DB: the startup cycle picks the queued row up
    // with no re-submission — persist-before-ack means nothing was lost.
    {
        let mut plugins = PluginSet::default();
        plugins.sources.insert(
            "mock_src".to_string(),
            launch(
                "task_source",
                "mock_src",
                json!({ "task_submit": true, "notify_log": source_log }),
            )
            .await,
        );
        plugins.agents.insert(
            "mock_agent".to_string(),
            launch(
                "agent_ide",
                "mock_agent",
                json!({ "stream_states": ["running", "done"] }),
            )
            .await,
        );
        let mut engine = Engine::new(
            StateDb::open(&db_path).unwrap(),
            engine_settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        let summary = tokio::time::timeout(
            Duration::from_secs(60),
            engine.run(false, std::future::pending()),
        )
        .await
        .expect("one-shot run must settle")
        .unwrap();

        assert_eq!(summary.stats.submitted, 0);
        assert_eq!(summary.stats.dispatched, 1);
        let db = StateDb::open(&db_path).unwrap();
        let task = db.find_by_source("mock_src", "s9").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Done);
        engine.shutdown(Duration::from_secs(5)).await;
    }

    let _ = std::fs::remove_dir_all(&base);
}

// ---------------------------------------------------------------------------
// Worktree cleanup 3-stage: decide → pane release → remove (#210)
// ---------------------------------------------------------------------------

/// The `session/release` calls recorded in a mock agent's dispatch_log.
fn recorded_releases(dispatch_log: &Path) -> Vec<serde_json::Value> {
    read_log(dispatch_log)
        .into_iter()
        .filter(|l| l["method"] == "session/release")
        .collect()
}

/// A manually driven clock frozen at a fixed epoch (#174); retention tests
/// advance it past the policy window instead of backdating rows in the DB.
fn manual_clock() -> Arc<ManualClock> {
    let t0 = time::OffsetDateTime::parse(
        "2026-01-01T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    Arc::new(ManualClock::new(t0))
}

#[tokio::test]
async fn done_task_releases_its_pane_before_immediate_worktree_removal() {
    let base = scratch("release_on_done");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({
            "stream_states": ["running", "done"],
            "pane_control": true,
            "dispatch_log": dispatch_log,
        }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    let worktree = task.worktree_path.clone().unwrap();
    assert!(
        !PathBuf::from(&worktree).exists(),
        "immediate cleanup removed the worktree"
    );
    let releases = recorded_releases(&dispatch_log);
    assert_eq!(releases.len(), 1, "one release per removal: {releases:?}");
    assert_eq!(releases[0]["params"]["session_id"], "sess-mock");
    assert_eq!(
        releases[0]["params"]["expect_cwd"], worktree,
        "the identity guard carries the task's worktree path"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn elapsed_retention_sweep_releases_pane_and_removes_worktree() {
    // The startup sweep of a later run: the worktree was retained under
    // `keep_7d` (= RetentionDays(7)); once the retention elapses, the sweep
    // must release the pane and then remove the worktree.
    let base = scratch("release_on_sweep");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let dispatch_log1 = base.join("dispatch1.ndjson");
    let dispatch_log2 = base.join("dispatch2.ndjson");
    let db_path = base.join("state.db");

    // Run 1: complete the task; retention keeps worktree AND pane. The
    // injected clock stamps `finished_at` with the frozen epoch (#174).
    let clock = manual_clock();
    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({
            "stream_states": ["running", "done"],
            "pane_control": true,
            "dispatch_log": dispatch_log1,
        }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.cleanup_implement = CleanupPolicy::RetentionDays(7);
    let mut engine = Engine::with_clock(
        StateDb::open_with_clock(&db_path, clock.clone()).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;
    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    let worktree = task.worktree_path.clone().unwrap();
    assert!(
        PathBuf::from(&worktree).exists(),
        "retention keeps the worktree"
    );
    assert!(
        recorded_releases(&dispatch_log1).is_empty(),
        "a retained worktree keeps its pane"
    );

    // Run 2, after the retention elapsed: the startup sweep cleans up.
    clock.advance(time::Duration::days(8));
    let plugins = plugin_set(
        json!([]),
        json!({ "pane_control": true, "dispatch_log": dispatch_log2 }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.cleanup_implement = CleanupPolicy::RetentionDays(7);
    let mut engine = Engine::with_clock(
        StateDb::open_with_clock(&db_path, clock.clone()).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;
    engine.cycle().await.unwrap();
    engine.shutdown(Duration::from_secs(5)).await;

    assert!(
        !PathBuf::from(&worktree).exists(),
        "elapsed retention removes the worktree"
    );
    let releases = recorded_releases(&dispatch_log2);
    assert_eq!(releases.len(), 1, "the pane was released: {releases:?}");
    assert_eq!(releases[0]["params"]["expect_cwd"], worktree);

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn dirty_worktree_keeps_both_worktree_and_pane() {
    // F-23's human entry point: a dirty worktree is DirtySkipped, and its pane
    // must stay open — the decision runs BEFORE any pane close.
    let base = scratch("dirty_keeps_pane");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({
            "stream_states": ["running", "done"],
            "pane_control": true,
            "dispatch_log": dispatch_log,
            "dirty_on_dispatch": true,
        }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    assert!(
        PathBuf::from(task.worktree_path.unwrap()).exists(),
        "a dirty worktree is preserved (F-23)"
    );
    assert!(
        recorded_releases(&dispatch_log).is_empty(),
        "a dirty worktree keeps its pane"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn manual_policy_never_releases_the_pane() {
    let base = scratch("manual_keeps_pane");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({
            "stream_states": ["running", "done"],
            "pane_control": true,
            "dispatch_log": dispatch_log,
        }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.cleanup_implement = CleanupPolicy::Manual;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    assert!(
        PathBuf::from(task.worktree_path.unwrap()).exists(),
        "manual policy keeps the worktree"
    );
    assert!(
        recorded_releases(&dispatch_log).is_empty(),
        "manual policy keeps the pane too"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn release_is_sent_once_even_when_removal_keeps_failing() {
    // `released_panes`: a worktree whose removal keeps failing (here: locked)
    // is re-decided `Remove` on every sweep, but the pane release must go out
    // exactly once.
    let base = scratch("release_once");
    let repo = setup_repo(&base);
    let source_log = base.join("source.ndjson");
    let notify_log = base.join("notify.ndjson");
    let dispatch_log1 = base.join("dispatch1.ndjson");
    let dispatch_log2 = base.join("dispatch2.ndjson");
    let db_path = base.join("state.db");

    let plugins = plugin_set(
        json!([mock_task("1")]),
        json!({
            "stream_states": ["running", "done"],
            "pane_control": true,
            "dispatch_log": dispatch_log1,
        }),
        &source_log,
        &notify_log,
    )
    .await;
    let clock = manual_clock();
    let mut settings = engine_settings(&repo);
    settings.cleanup_implement = CleanupPolicy::RetentionDays(7);
    let mut engine = Engine::with_clock(
        StateDb::open_with_clock(&db_path, clock.clone()).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;
    let db_probe = db_path.clone();
    run_watch_until(&mut engine, move || {
        StateDb::open(&db_probe)
            .unwrap()
            .find_by_source("mock_src", "1")
            .unwrap()
            .is_some_and(|t| t.state == TaskState::Done)
    })
    .await;
    engine.shutdown(Duration::from_secs(5)).await;

    let db = StateDb::open(&db_path).unwrap();
    let task = db.find_by_source("mock_src", "1").unwrap().unwrap();
    let worktree = task.worktree_path.clone().unwrap();
    clock.advance(time::Duration::days(8));
    // `git worktree remove` refuses a locked worktree — a deterministic,
    // repeatable removal failure.
    git(&repo, &["worktree", "lock", &worktree]);

    let plugins = plugin_set(
        json!([]),
        json!({ "pane_control": true, "dispatch_log": dispatch_log2 }),
        &source_log,
        &notify_log,
    )
    .await;
    let mut settings = engine_settings(&repo);
    settings.cleanup_implement = CleanupPolicy::RetentionDays(7);
    let mut engine = Engine::with_clock(
        StateDb::open_with_clock(&db_path, clock.clone()).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;
    for _ in 0..5 {
        engine.cycle().await.unwrap();
    }
    assert!(
        PathBuf::from(&worktree).exists(),
        "the locked worktree could not be removed"
    );
    assert_eq!(
        recorded_releases(&dispatch_log2).len(),
        1,
        "release must not be re-sent on every failing sweep"
    );

    // Unlock → the next sweep finishes the removal without another release.
    git(&repo, &["worktree", "unlock", &worktree]);
    engine.cycle().await.unwrap();
    engine.shutdown(Duration::from_secs(5)).await;
    assert!(!PathBuf::from(&worktree).exists(), "removed after unlock");
    assert_eq!(recorded_releases(&dispatch_log2).len(), 1);

    let _ = std::fs::remove_dir_all(&base);
}
