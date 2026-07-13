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
use orchestrator_core::run::output::{PrCreator, PrError, PrRequest};
use orchestrator_core::run::{Engine, EngineSettings, PluginSet, RepoSettings};
use orchestrator_core::scheduler::Limits;
use orchestrator_core::worktree::{CleanupPolicy, DEFAULT_BRANCH_TEMPLATE};
use plugin_protocol::manifest::Manifest;
use serde_json::json;
use std::sync::{Arc, Mutex};

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
fn engine_settings(repo_path: &Path) -> EngineSettings {
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
        pr_title_template: "totsuka: {title}".to_string(),
        pr_body_template: "Task {title} ({url})\n\n{summary}".to_string(),
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
            engine_settings(&repo),
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
        engine_settings(&repo),
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
            engine_settings(&repo),
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
        engine.cycle(None).await.unwrap();
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

    let summary = tokio::time::timeout(
        Duration::from_secs(60),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("one-shot settles")
    .unwrap();
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

    let summary = tokio::time::timeout(
        Duration::from_secs(60),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("settles")
    .unwrap();
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

    let summary = tokio::time::timeout(
        Duration::from_secs(60),
        engine.run(false, std::future::pending()),
    )
    .await
    .expect("settles")
    .unwrap();
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

    // Dispatch (records the session), then drop the engine before finalize.
    engine.cycle(None).await.unwrap();
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
