//! Integration tests for the run main loop (#63) against real mock-plugin
//! subprocesses and a real git repository.
//!
//! Covers the issue's acceptance criteria:
//! 1. fetch → worktree → dispatch → done → cleanup, end to end.
//! 2. One-shot leaves waiting tasks and re-running does not double-ingest.
//! 3. A restart after an interrupted run recovers the in-flight task (§5.3).
//! 4. `--dry-run` reports decisions with zero side effects.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use orchestrator_core::adapters::StateDb;
use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::OpenAiRouter;
use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::state::TaskState;
use orchestrator_core::domain::workflow::Workflow;
use orchestrator_core::repo_select::SelectConfig;
use orchestrator_core::run::{Engine, EngineSettings, PluginSet, RepoSettings};
use orchestrator_core::scheduler::Limits;
use orchestrator_core::worktree::{CleanupPolicy, DEFAULT_BRANCH_TEMPLATE};
use plugin_protocol::manifest::Manifest;
use serde_json::json;

/// Path to the compiled mock plugin binary.
fn mock_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin"))
}

/// Run a git command, asserting success. Signing is disabled so the seed
/// commit never blocks on a local signing agent.
fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Set up `origin.git` (bare) with one commit on `main`, and a clone of it.
fn setup_repo(base: &Path) -> PathBuf {
    let origin = base.join("origin.git");
    git(
        base,
        &["init", "--bare", "-b", "main", origin.to_str().unwrap()],
    );
    let seed = base.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-b", "main"]);
    git(&seed, &["config", "user.email", "t@example.com"]);
    git(&seed, &["config", "user.name", "T"]);
    git(&seed, &["commit", "--allow-empty", "-m", "init"]);
    git(
        &seed,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&seed, &["push", "origin", "main"]);

    let clone = base.join("clone");
    git(
        base,
        &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
    );
    clone
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("totsuka-run-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Launch the mock plugin under a manifest of the given kind.
async fn launch(kind: &str, name: &str, init_config: serde_json::Value) -> Plugin {
    let manifest = Manifest::from_toml_str(&format!(
        r#"
name = "{name}"
kind = "{kind}"
version = "0.1.0"
protocol_version = "^0.1"
"#
    ))
    .unwrap();
    Plugin::launch(PluginSpec {
        name: name.to_string(),
        program: mock_plugin(),
        args: vec![],
        manifest,
        init_config,
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
fn settings(repo_path: &Path) -> EngineSettings {
    EngineSettings {
        workflows: workflows(),
        repos: vec![RepoSettings {
            name: "clone".to_string(),
            path: repo_path.to_path_buf(),
            summary: None,
            worktree_location: None,
        }],
        limits: Limits::global(2),
        branch_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
        location_template: "{repo}/../wt/{branch}".to_string(),
        cleanup_implement: CleanupPolicy::Immediate,
        cleanup_plan: CleanupPolicy::Immediate,
        env: HashMap::new(),
        select: SelectConfig::default(),
        poll_intervals: HashMap::new(),
        readme_cache_dir: None,
    }
}

/// One fetchable task in the mock source's config shape. The `source` field
/// deliberately differs from the plugin *instance* name (`mock_src`): real
/// plugins stamp their own source name (e.g. `github` whatever the instance
/// is called), and the engine must normalize it before matching.
fn mock_task(id: &str) -> serde_json::Value {
    json!({ "id": id, "source": "github", "title": format!("task {id}") })
}

/// Build a full plugin set: source (with tasks), agent (config-driven),
/// notifier (recording to `notify_log`).
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
            json!({ "tasks": tasks, "notify_log": source_log }),
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
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
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
        settings(&repo),
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

    assert_eq!(summary.stats.fetched, 1);
    assert_eq!(summary.stats.ingested, 1);
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

#[tokio::test]
async fn one_shot_leaves_waiting_task_and_rerun_does_not_double_ingest() {
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
        settings(&repo),
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
    .expect("one-shot must exit once the task is waiting")
    .unwrap();
    assert_eq!(summary.stats.dispatched, 1);
    assert_eq!(summary.waiting.len(), 1, "waiting task remains (§5.1)");

    // Re-run: the same task is fetched again but not re-ingested (F-73) nor
    // re-dispatched (it is waiting, not queued).
    let summary2 = tokio::time::timeout(
        Duration::from_secs(60),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("second one-shot settles immediately")
    .unwrap();
    assert_eq!(summary2.stats.ingested, 1, "no second ingest");
    assert_eq!(summary2.stats.dispatched, 1, "no second dispatch");
    assert_eq!(summary2.waiting.len(), 1);

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
    // agent only ever reports `running`).
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
            settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        // One cycle dispatches; dropping the engine simulates SIGKILL (no
        // graceful shutdown, no event processing).
        engine.cycle(None).await.unwrap();
        let db = StateDb::open(&db_path).unwrap();
        let task = db.find_by_source("mock_src", "9").unwrap().unwrap();
        assert_eq!(task.state, TaskState::Dispatched);
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
        settings(&repo),
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

#[tokio::test]
async fn dry_run_reports_decisions_with_zero_side_effects() {
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
        settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let entries = engine.dry_run().await.unwrap();
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.workflow, "implement");
    assert_eq!(entry.agent, "mock_agent");
    assert_eq!(entry.mode, "implement");
    assert!(
        entry.repo.contains("clone") && entry.repo.contains("only one configured repository"),
        "repo decision must carry its rationale: {}",
        entry.repo
    );
    assert!(entry.already_ingested.is_none());

    // Zero side effects: nothing ingested, no worktree, no notifications, no
    // source write-backs.
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
            settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        engine.cycle(None).await.unwrap();
    }

    // Restart: the session is lost → needs confirmation (§5.3), and the
    // one-shot loop must still exit instead of waiting forever for a
    // notification that can never arrive.
    let plugins = plugin_set(json!([]), json!({}), &source_log, &notify_log).await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings(&repo),
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
            settings(&repo),
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;
        engine.cycle(None).await.unwrap();
    }

    // Restart: re-attach reports Done. Agents do not replay terminal states on
    // re-subscribe, so recovery itself must finalize: complete + write-back +
    // cleanup + notify.
    let plugins = plugin_set(json!([]), json!({}), &source_log, &notify_log).await;
    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings(&repo),
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
        settings(&repo),
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    // Progress could never be observed → the dispatch must fail the task and
    // the one-shot must exit (not hold the slot forever).
    let summary = tokio::time::timeout(
        Duration::from_secs(30),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("one-shot must exit")
    .unwrap();
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
