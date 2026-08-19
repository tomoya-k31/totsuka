//! Integration tests for hook-signal handling (#138): `Engine::on_signal`,
//! verification, escalation, the timeout sweep, spool recovery, and the
//! dispatch-side hook wiring. Runs against real mock-plugin subprocesses.
//!
//! The orca/mock `AgentState::Done` regression and the restart-crossing
//! `Verifying` safety are covered by the existing `run_loop.rs` /
//! `recovery` suites, which this PR leaves unchanged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orchestrator_core::adapters::clock::ManualClock;
use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::OpenAiRouter;
use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::adapters::state_db::{HookEventInsert, TaskMessageInsert};
use orchestrator_core::adapters::{NewTask, StateDb};
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::signal::{
    AgentSignal, JobId, SignalEvent, SignalSource, StopStatus,
};
use orchestrator_core::domain::state::{TaskEvent, TaskState};
use orchestrator_core::domain::workflow::Workflow;
use orchestrator_core::ports::{Clock, SecretString};
use orchestrator_core::repo_select::SelectConfig;
use orchestrator_core::run::{Engine, EngineSettings, HookRuntime, PluginSet, RepoSettings};
use orchestrator_core::scheduler::Limits;
use orchestrator_core::worktree::{CleanupPolicy, DEFAULT_WORKTREE_NAME_TEMPLATE};
use plugin_protocol::manifest::Manifest;
use serde_json::json;
use test_support::{bare_origin_and_clone as setup_repo, scratch};

fn mock_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin"))
}

async fn launch(kind: &str, name: &str, init_config: serde_json::Value) -> Plugin {
    let manifest = Manifest::from_toml_str(&format!(
        r#"
name = "{name}"
kind = "{kind}"
version = "0.1.0"
protocol_version = ">=0.1.6, <0.6"
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
    test_support::read_ndjson_log(path)
}

/// A safety-net ceiling so a wiring regression fails the test instead of
/// hanging CI.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Drive a watch-mode run until `cond` holds (checked every 100ms, capped by
/// `RUN_TIMEOUT`), then stop it. The mock source pushes its task via
/// `task/submit`, arriving as an event on its own schedule, so `cond`
/// observes durable state (e.g. re-opening the state DB) rather than
/// borrowing `engine`, which `run` holds mutably for the loop's duration.
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

/// One workflow `wf` (source `mock_src`, agent `mock_agent`) with the given
/// verification mode and output policy.
fn workflows(verification: &str, output: &str) -> Vec<Workflow> {
    let cfg = RootConfig::from_toml_str(&format!(
        r#"
[[workflows]]
name = "wf"
source = "mock_src"
trigger = {{}}
mode = "implement"
agent = "mock_agent"
output = "{output}"
verification = "{verification}"
on_success = {{ set_status = "done" }}
on_failure = {{ set_status = "failed" }}
"#
    ))
    .unwrap();
    Workflow::from_configs(&cfg.workflows)
}

fn engine_settings(wfs: Vec<Workflow>, hook: Option<HookRuntime>) -> EngineSettings {
    EngineSettings {
        workflows: wfs,
        repos: vec![RepoSettings {
            name: "clone".to_string(),
            path: PathBuf::from("/nonexistent"),
            summary: None,
            worktree_location: None,
            tool: None,
        }],
        limits: Limits::global(4),
        worktree_name_template: DEFAULT_WORKTREE_NAME_TEMPLATE.to_string(),
        location_template: "{repo}/../wt/{worktree_name}".to_string(),
        cleanup_implement: CleanupPolicy::Manual,
        cleanup_plan: CleanupPolicy::Immediate,
        env: HashMap::new(),
        select: SelectConfig::default(),
        readme_cache_dir: None,
        // Sweep every cycle, as before the interval existed (#210).
        worktree_sweep_interval: Duration::ZERO,
        // These tests drive the loop in watch mode (`run_until`), where the
        // one-shot grace is never consulted (#281).
        one_shot_grace: Duration::ZERO,
        tools: orchestrator_core::tool::builtin_registry(),
        default_tool: "claude".to_string(),
        prompts: Default::default(),
        plugin_restart: Default::default(),
        restart_disabled: Default::default(),
        hook,
    }
}

/// A minimal plugin set: a config-driven agent + a recording notifier.
async fn plugin_set(agent_config: serde_json::Value, notify_log: &Path) -> PluginSet {
    let mut set = PluginSet::default();
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

fn new_task(source_task_id: &str, last_signal_at: Option<&str>) -> NewTask {
    NewTask {
        source: "mock_src".into(),
        source_task_id: source_task_id.into(),
        workflow: "wf".into(),
        mode: "implement".into(),
        repo: Some("clone".into()),
        priority: 0,
        title: "hook task".into(),
        url: None,
        source_payload: None,
        last_signal_at: last_signal_at.map(str::to_string),
    }
}

/// Seed a task straight to `Running`, recording a session, and return
/// `(task_id, session_row)`.
fn seed_running(db: &StateDb, sid: &str) -> (i64, i64) {
    let id = db.upsert_task(&new_task("1", None)).unwrap();
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();
    let row = db.record_session(id, "mock_agent", sid).unwrap();
    (id, row)
}

fn stop(
    task_id: i64,
    row: i64,
    prompt_id: &str,
    status: StopStatus,
    msg: Option<&str>,
) -> AgentSignal {
    AgentSignal {
        source: SignalSource::AgentHook,
        job_id: JobId::new(task_id, row),
        tool_session_id: "cc-1".into(),
        prompt_id: prompt_id.into(),
        event: SignalEvent::Stop {
            status,
            reason: None,
            last_assistant_message: msg.map(str::to_string),
            transcript_path: Some("/t.jsonl".into()),
        },
        payload: json!({ "hook_event_name": "Stop" }),
    }
}

/// A `QuestionPending` signal (#487): the agent opened an interactive question
/// dialog. `prompt_id` is the per-question idempotency key (claude: the
/// PreToolUse `tool_use_id`; opencode: the plugin's `callID`).
fn question(task_id: i64, row: i64, prompt_id: &str, msg: &str) -> AgentSignal {
    AgentSignal {
        source: SignalSource::AgentHook,
        job_id: JobId::new(task_id, row),
        tool_session_id: "cc-1".into(),
        prompt_id: prompt_id.into(),
        event: SignalEvent::QuestionPending {
            message: Some(msg.to_string()),
        },
        payload: json!({ "hook_event_name": "QuestionPending" }),
    }
}

fn heartbeat(task_id: i64, row: i64, prompt_id: &str) -> AgentSignal {
    AgentSignal {
        source: SignalSource::AgentHook,
        job_id: JobId::new(task_id, row),
        tool_session_id: "cc-1".into(),
        prompt_id: prompt_id.into(),
        event: SignalEvent::Heartbeat,
        payload: json!({ "hook_event_name": "Stop", "background_tasks": [{ "id": "bg" }] }),
    }
}

const GRACE: Duration = Duration::from_secs(5);

/// Fixed epoch for deterministic-clock tests (#174).
const T0: &str = "2026-01-01T00:00:00Z";

/// A manually driven clock frozen at [`T0`]; timeout-sweep tests advance it
/// across the workflow timeout instead of seeding magic ancient anchors.
fn manual_clock() -> Arc<ManualClock> {
    let t0 =
        time::OffsetDateTime::parse(T0, &time::format_description::well_known::Rfc3339).unwrap();
    Arc::new(ManualClock::new(t0))
}

/// A `tracing` writer that keeps everything in memory, so a test can assert on
/// what was logged — which is the whole promise here: the probe must not be
/// reported as an anomaly. Asserting only "nothing changed" would pass with the
/// fix reverted, because the unknown-task path changes nothing either.
#[derive(Clone, Default)]
struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn the_doctor_liveness_probe_is_not_logged_as_an_anomaly() {
    // `totsuka doctor` proves the hook receiver is alive by POSTing a real
    // signal whose job id names no task on purpose (`JobId::DOCTOR_PROBE`). It
    // takes the unknown-task path — no state change, nothing persisted — but a
    // routine health check must not warn: running `doctor` against a live
    // `totsuka run` would put a line in that run's log every time, which reads
    // as a fault (it cost two people an investigation during the live
    // acceptance run for #481).
    //
    // The control in the same test is a genuinely unknown job id, which must
    // still warn.
    let base = scratch("hook_doctor_probe");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let logs = CapturedLogs::default();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .finish(),
    );

    let probe = AgentSignal {
        job_id: JobId::DOCTOR_PROBE,
        ..stop(id, row, "p1", StopStatus::Completed, Some("ignored"))
    };
    engine.on_signal(probe).await.unwrap();
    assert_eq!(
        logs.text(),
        "",
        "the probe is routine: nothing at WARN or above"
    );

    let stale = AgentSignal {
        job_id: JobId::new(999_999, 1),
        ..stop(id, row, "p2", StopStatus::Completed, Some("ignored"))
    };
    engine.on_signal(stale).await.unwrap();
    assert!(
        logs.text().contains("hook signal for unknown task"),
        "a job id that is not the probe still warns: {:?}",
        logs.text()
    );
    drop(guard);

    let task = engine.db().get_task(id).unwrap().unwrap();
    assert_eq!(
        task.state,
        TaskState::Running,
        "neither signal touched any task's state"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

/// The wire value both sides depend on. `doctor` sends it and the receiver
/// recognises it; pinning the string keeps the two from drifting apart into a
/// warning nobody can explain.
#[test]
fn the_doctor_probe_job_id_is_job_0_0() {
    assert_eq!(JobId::DOCTOR_PROBE.to_string(), "job-0-0");
    assert!("job-0-0".parse::<JobId>().unwrap().is_doctor_probe());
    assert!(!JobId::new(1, 0).is_doctor_probe());
    assert!(!JobId::new(0, 1).is_doctor_probe());
}

#[tokio::test]
async fn completed_llm_publishes_to_done() {
    let base = scratch("hook_llm_done");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(stop(
            id,
            row,
            "p1",
            StopStatus::Completed,
            Some("all done <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();

    let task = engine.db().get_task(id).unwrap().unwrap();
    assert_eq!(
        task.state,
        TaskState::Done,
        "llm COMPLETED publishes straight to Done"
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes.iter().any(|n| n["params"]["event"] == "done"),
        "done notification delivered: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// `verification = "llm"` on a tool without prompt-type Stop hooks must park
/// the task in `Verifying` (human), not publish it (#301).
///
/// llm verification is an in-session prompt-type hook that only Claude-kind
/// tools have. Before this, the `Llm` arm shared its branch with `None`, so a
/// codex/opencode task published **unverified** output while the config still
/// said `llm` — the config warning promised a fallback to human that nothing
/// implemented. The sibling `completed_llm_publishes_to_done` above is the
/// control: same signal, claude tool, straight to `Done`.
#[tokio::test]
async fn completed_llm_on_tool_without_prompt_hooks_parks_in_verifying() {
    for tool in ["codex", "opencode"] {
        let base = scratch(&format!("hook_llm_degrade_{tool}"));
        let notify_log = base.join("notify.ndjson");
        let db = StateDb::open(&base.join("state.db")).unwrap();
        let (id, row) = seed_running(&db, "sess-1");

        // Resolution order is workflow pin > repo > global, and the fixture
        // workflow pins nothing — so the global default decides.
        let mut settings = engine_settings(workflows("llm", "none"), None);
        settings.default_tool = tool.to_string();

        let mut engine = Engine::new(
            db,
            settings,
            plugin_set(json!({}), &notify_log).await,
            SystemGitRunner,
            no_llm(),
        )
        .await;

        engine
            .on_signal(stop(
                id,
                row,
                "p1",
                StopStatus::Completed,
                Some("all done <<STATUS:COMPLETED>>"),
            ))
            .await
            .unwrap();

        let task = engine.db().get_task(id).unwrap().unwrap();
        assert_eq!(
            task.state,
            TaskState::Verifying,
            "`{tool}` has no prompt-type Stop hook → llm degrades to human, \
             so COMPLETED must park in Verifying instead of publishing"
        );

        engine.shutdown(GRACE).await;
        let notes = read_log(&notify_log);
        assert!(
            notes
                .iter()
                .any(|n| n["params"]["event"] == "verification_pending"),
            "`{tool}`: the degraded path must still ask a human to verify: {notes:?}"
        );
        assert!(
            !notes.iter().any(|n| n["params"]["event"] == "done"),
            "`{tool}`: nothing may be published before verification: {notes:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// The same #301 degradation, reached through `profile` instead of an explicit
/// `verification = "llm"` (#394).
///
/// A profile supplies `verification`, so the raw config field is `None`. If any
/// step of the chain read that field instead of the resolved value, this task
/// would take the `None` arm and publish **unverified** — the exact failure
/// #301 fixed, re-introduced through the new notation and invisible because the
/// run still looks successful.
#[tokio::test]
async fn a_profile_workflow_also_degrades_to_human_on_a_tool_without_prompt_hooks() {
    let base = scratch("hook_llm_degrade_profile");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let cfg = RootConfig::from_toml_str(
        r#"
[[workflows]]
name = "wf"
source = "mock_src"
trigger = {}
profile = "answer"
agent = "mock_agent"
output = "none"
on_success = { set_status = "done" }
on_failure = { set_status = "failed" }
"#,
    )
    .unwrap();
    let mut settings = engine_settings(Workflow::from_configs(&cfg.workflows), None);
    settings.default_tool = "codex".to_string();

    let mut engine = Engine::new(
        db,
        settings,
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(stop(
            id,
            row,
            "p1",
            StopStatus::Completed,
            Some("all done <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();

    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Verifying,
        "profile = answer resolves verification to llm, so codex must degrade to human"
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        !notes.iter().any(|n| n["params"]["event"] == "done"),
        "nothing may be published before verification: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn completed_human_waits_for_verify_then_pass_reaches_done() {
    let base = scratch("hook_human_verify");
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");
    let db = StateDb::open(&db_path).unwrap();
    // `sess-done` so the recovery re-attach reports the agent already done →
    // the approved (Publishing) task finalizes.
    let (id, row) = seed_running(&db, "sess-done-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("human", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(stop(
            id,
            row,
            "p1",
            StopStatus::Completed,
            Some("draft <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Verifying,
        "human verification parks the task in Verifying"
    );

    // Simulate `totsuka task verify --pass`.
    engine
        .db()
        .apply_event(id, TaskEvent::ApproveVerification, None)
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Publishing
    );

    // The next run's recover cycle finalizes the Publishing task.
    engine.recover().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Done,
        "recover finalizes the approved task"
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes
            .iter()
            .any(|n| n["params"]["event"] == "verification_pending"),
        "verification-pending notification delivered: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn verify_fail_returns_to_running() {
    let base = scratch("hook_verify_fail");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("human", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(stop(
            id,
            row,
            "p1",
            StopStatus::Completed,
            Some("x <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();
    // `totsuka task verify --fail`.
    engine
        .db()
        .apply_event(id, TaskEvent::VerificationFailed, None)
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Running,
        "rejection returns the task to Running for correction"
    );
    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn three_unknown_stops_escalate_with_snapshot() {
    let base = scratch("hook_escalate");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        // A diagnostics-capable agent so the escalation captures a pane snapshot.
        plugin_set(
            json!({ "diagnostics_snapshot": true, "snapshot_text": "PANE" }),
            &notify_log,
        )
        .await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    // Distinct prompt_ids so the dedup key differs and all three are counted.
    for (i, p) in ["p1", "p2", "p3"].iter().enumerate() {
        engine
            .on_signal(stop(id, row, p, StopStatus::Unknown, None))
            .await
            .unwrap();
        let state = engine.db().get_task(id).unwrap().unwrap().state;
        if i < 2 {
            assert_eq!(state, TaskState::Running, "below threshold stays Running");
        } else {
            assert_eq!(state, TaskState::Escalated, "3rd UNKNOWN escalates (D-02)");
        }
    }

    // The escalation event carries the pane snapshot (R-10).
    let has_snapshot = engine
        .db()
        .list_events(id)
        .unwrap()
        .into_iter()
        .filter_map(|e| e.detail)
        .any(|d| d.get("diagnostics").and_then(|v| v.as_str()) == Some("PANE"));
    assert!(
        has_snapshot,
        "diagnostics snapshot recorded in the escalate detail"
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes.iter().any(|n| n["params"]["event"] == "escalated"),
        "escalated notification delivered: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn focus_task_delegates_to_a_pane_control_agent() {
    // F-94: `POST /focus` → `Engine::focus_task` → the agent's `session/focus`
    // with the opaque session id, gated on `pane_control`.
    let base = scratch("hook_focus_ok");
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, _row) = seed_running(&db, "sess-focus");

    let engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(
            json!({ "pane_control": true, "dispatch_log": dispatch_log }),
            &notify_log,
        )
        .await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let outcome = engine.focus_task(id).await;
    assert!(outcome.focused, "outcome was {outcome:?}");

    // The delegation carried the session id verbatim (opaque, F-37).
    let calls = read_log(&dispatch_log);
    let focus_call = calls
        .iter()
        .find(|c| c["method"] == "session/focus")
        .expect("a session/focus call");
    assert_eq!(focus_call["params"]["session_id"], "sess-focus");

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn focus_task_degrades_without_pane_control_or_task() {
    // No `pane_control` → a reasoned no, not an error; same for an unknown
    // task and a vanished pane (`focused: false` from the plugin).
    let base = scratch("hook_focus_degrade");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, _row) = seed_running(&db, "sess-gone");

    let engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        // pane_control defaults to false in the mock.
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let outcome = engine.focus_task(id).await;
    assert!(!outcome.focused);
    assert!(
        outcome
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("pane_control"),
        "outcome was {outcome:?}"
    );

    let outcome = engine.focus_task(9999).await;
    assert!(!outcome.focused);
    assert!(
        outcome
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("not found"),
        "outcome was {outcome:?}"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn focus_task_reports_a_closed_pane_as_not_focused() {
    // The mock answers `focused: false` for a session id containing `gone`
    // (the pane vanished after the task finished) — still not an error.
    let base = scratch("hook_focus_gone");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, _row) = seed_running(&db, "sess-gone");

    let engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({ "pane_control": true }), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let outcome = engine.focus_task(id).await;
    assert!(!outcome.focused);
    assert!(
        outcome.reason.as_deref().unwrap_or("").contains("closed"),
        "outcome was {outcome:?}"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn needs_input_parks_in_waiting_input() {
    let base = scratch("hook_needs_input");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(stop(id, row, "p1", StopStatus::NeedsInput, None))
        .await
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::WaitingInput
    );
    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes
            .iter()
            .any(|n| n["params"]["event"] == "waiting_input"),
        "waiting_input notification delivered: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #487: a `QuestionPending` parks exactly like `Stop{NeedsInput}` — the turn
/// has not ended (the dialog is open), so this signal is the only thing that
/// can park the task (ADR-0038 D6). The question text reaches the operator as
/// the `waiting_input` notification payload, and a later COMPLETED — the human
/// answered "approve" in the dialog and the agent finished its turn — still
/// publishes from `WaitingInput`.
#[tokio::test]
async fn question_pending_parks_and_a_later_completed_still_publishes() {
    let base = scratch("hook_question_park");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(question(id, row, "toolu_01", "Approve completion?"))
        .await
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::WaitingInput,
        "an open question dialog parks the task"
    );

    // The human answered and the agent ended its turn with COMPLETED: the
    // park must not strand the task.
    engine
        .on_signal(stop(
            id,
            row,
            "p2",
            StopStatus::Completed,
            Some("approved <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Done,
        "COMPLETED after the question park still publishes"
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        notes.iter().any(|n| n["params"]["event"] == "waiting_input"
            && n["params"]["body"]
                .as_str()
                .is_some_and(|m| m.contains("Approve completion?"))),
        "the question text is what the operator reads: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #487: while already parked, a *new* question (distinct `prompt_id`)
/// re-notifies without a state change — unlike a second NEEDS_INPUT, which is
/// a silent no-op (the known ADR-0043 gap). A literal redelivery of the same
/// question (same `prompt_id`) is dropped by the idempotency key and must not
/// re-notify.
#[tokio::test]
async fn a_second_question_renotifies_and_a_redelivery_does_not() {
    let base = scratch("hook_question_renotify");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    engine
        .on_signal(question(id, row, "toolu_01", "first?"))
        .await
        .unwrap();
    // A curl retry / spool re-send of the SAME question: dropped (D-05).
    engine
        .on_signal(question(id, row, "toolu_01", "first?"))
        .await
        .unwrap();
    // A NEW question while still parked: notify again, state unchanged.
    engine
        .on_signal(question(id, row, "toolu_02", "second?"))
        .await
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::WaitingInput
    );

    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    let waiting: Vec<_> = notes
        .iter()
        .filter(|n| n["params"]["event"] == "waiting_input")
        .collect();
    assert_eq!(
        waiting.len(),
        2,
        "one notification per distinct question, none for the redelivery: {notes:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn duplicate_signal_transitions_once() {
    let base = scratch("hook_dup");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("human", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let sig = stop(
        id,
        row,
        "p1",
        StopStatus::Completed,
        Some("x <<STATUS:COMPLETED>>"),
    );
    engine.on_signal(sig.clone()).await.unwrap();
    // The exact same signal (identical idempotency key) is dropped by the DB.
    engine.on_signal(sig).await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Verifying,
        "the duplicate does not re-transition"
    );
    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn timeout_sweep_escalates_silent_task() {
    let base = scratch("hook_timeout");
    let notify_log = base.join("notify.ndjson");
    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    // Seed a task that proves itself alive *after* being dispatched, then let
    // the test drive the clock across the 30-minute default timeout (#174).
    // The signal has to come after the dispatch: a dispatch starts a new
    // execution and clears the anchor (#382), and seeding one beforehand would
    // build a state that never occurs.
    let id = db.upsert_task(&new_task("1", None)).unwrap();
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();
    db.touch_last_signal(id).unwrap();
    db.record_session(id, "mock_agent", "sess-1").unwrap();

    let mut engine = Engine::with_clock(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;

    // Exactly at the timeout boundary: the comparison is strict (`>`), so the
    // task is still within its window.
    clock.advance(time::Duration::seconds(1800));
    engine.sweep_signal_timeouts().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Running,
        "exactly at the timeout is not yet silent"
    );

    // One second past the boundary: escalate.
    clock.advance(time::Duration::seconds(1));
    engine.sweep_signal_timeouts().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Escalated,
        "a silent task past its timeout escalates (D-03)"
    );
    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(notes.iter().any(|n| n["params"]["event"] == "escalated"));
    let _ = std::fs::remove_dir_all(&base);
}

/// A retried task must not be judged by the previous execution's silence
/// (#382). Live, a task that signalled, failed, and was `task retry`d ~27
/// minutes later escalated **3ms after being dispatched** — the sweep compared
/// `now` against an anchor belonging to the attempt before.
#[tokio::test]
async fn a_redispatched_task_is_not_escalated_for_the_previous_attempts_silence() {
    let base = scratch("hook_retry_anchor");
    let notify_log = base.join("notify.ndjson");
    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    let id = db.upsert_task(&new_task("1", None)).unwrap();

    // First execution: dispatched, proved itself alive, then failed. The
    // anchor is stamped AFTER the dispatch — seeding it beforehand would let
    // the first dispatch clear it, leaving nothing for the second one to clear
    // and turning this into a test that passes without the fix.
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();
    db.touch_last_signal(id).unwrap();
    db.apply_event(id, TaskEvent::Fail, None).unwrap();

    // A human retries it well past the workflow timeout.
    clock.advance(time::Duration::seconds(1801));
    db.retry_task(id, None).unwrap();
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();

    let mut engine = Engine::with_clock(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;

    engine.sweep_signal_timeouts().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Dispatched,
        "the fresh execution has not been silent — the old anchor must not count against it"
    );

    // And D-03 still applies once THIS execution proves itself alive and then
    // goes quiet: the fix clears the anchor, it does not disable the sweep.
    engine.db().touch_last_signal(id).unwrap();
    clock.advance(time::Duration::seconds(1801));
    engine.sweep_signal_timeouts().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Escalated,
        "a silent task past its timeout still escalates (D-03)"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Same workflow as [`workflows`] but with a short, explicit `timeout_secs`
/// so a test can observe a task crossing its timeout *during* the run,
/// rather than seeding it already-expired (which the initial startup
/// `cycle()` would sweep regardless of whether periodic re-ticking works).
fn workflows_with_timeout(timeout_secs: u64) -> Vec<Workflow> {
    let cfg = RootConfig::from_toml_str(&format!(
        r#"
[[workflows]]
name = "wf"
source = "mock_src"
trigger = {{}}
mode = "implement"
agent = "mock_agent"
output = "none"
verification = "llm"
timeout_secs = {timeout_secs}
on_success = {{ set_status = "done" }}
on_failure = {{ set_status = "failed" }}
"#
    ))
    .unwrap();
    Workflow::from_configs(&cfg.workflows)
}

/// `timeout_secs = 0` opts a workflow out of the D-03 sweep (#439). Before
/// this, a written-out `0` meant "escalate on the first sweep" — `> 0` is true
/// for any positive silence — which no config could have wanted.
#[tokio::test]
async fn a_zero_timeout_disables_the_silence_sweep() {
    let base = scratch("hook_timeout_zero");
    let notify_log = base.join("notify.ndjson");
    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    // Same shape as `timeout_sweep_escalates_silent_task`: alive after the
    // dispatch, then silent for far longer than any plausible timeout.
    let id = db.upsert_task(&new_task("1", None)).unwrap();
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();
    db.touch_last_signal(id).unwrap();
    db.record_session(id, "mock_agent", "sess-1").unwrap();

    let mut engine = Engine::with_clock(
        db,
        engine_settings(workflows_with_timeout(0), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;

    // A week of silence: far past the default 30 minutes, and past the old
    // behaviour's instant trip point.
    clock.advance(time::Duration::days(7));
    engine.sweep_signal_timeouts().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Running,
        "timeout_secs = 0 must exempt the task from the D-03 sweep"
    );
    engine.shutdown(GRACE).await;
    let notes = read_log(&notify_log);
    assert!(
        !notes.iter().any(|n| n["params"]["event"] == "escalated"),
        "no escalation notification may fire for an opted-out workflow"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn watch_mode_periodic_tick_escalates_silent_task_without_events() {
    // Regression test (0.2.0, #190): the old poll-driven `cycle()` call used
    // to double as a periodic maintenance heartbeat before Orchestrator-side
    // polling was removed. Without a replacement heartbeat, a long-running
    // `--watch` process would never re-check signal timeouts (D-03) or
    // worktree retention (F-23) unless a push event happened to arrive.
    //
    // The task gets a *fresh* `last_signal_at` and a 1-second workflow timeout
    // — not yet timed out when the startup `cycle()` runs, only becoming so a
    // moment later. No event is ever sent, so only a periodic re-check (not the
    // startup sweep) can catch it. The anchor is stamped after the dispatch
    // because a dispatch clears it (#382).
    let base = scratch("hook_watch_tick");
    let db_path = base.join("state.db");
    let notify_log = base.join("notify.ndjson");
    let id = {
        let db = StateDb::open(&db_path).unwrap();
        let id = db.upsert_task(&new_task("1", None)).unwrap();
        db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
        db.apply_event(id, TaskEvent::Start, None).unwrap();
        db.touch_last_signal(id).unwrap();
        db.record_session(id, "mock_agent", "sess-1").unwrap();
        id
    };

    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        engine_settings(workflows_with_timeout(1), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let db_probe = db_path.clone();
    run_until(&mut engine, move || {
        let db = StateDb::open(&db_probe).unwrap();
        db.get_task(id).unwrap().unwrap().state == TaskState::Escalated
    })
    .await;
    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn spool_replay_applies_signal_and_deletes_file() {
    let base = scratch("hook_spool");
    let notify_log = base.join("notify.ndjson");
    let spool_dir = base.join("spool");
    std::fs::create_dir_all(&spool_dir).unwrap();
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let hook = HookRuntime {
        socket_path: base.join("sock"),
        auth_token: None,
        spool_dir: Some(spool_dir.clone()),
        settings_paths: HashMap::new(),
        block_retry_limit: 3,
    };
    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), Some(hook)),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    // A spooled NDJSON line exactly as on-stop.sh would emit it.
    let line = format!(
        r#"{{"job_id":"{}","session_id":"cc-1","prompt_id":"sp","hook_event_name":"Stop","status":"COMPLETED","last_assistant_message":"spooled <<STATUS:COMPLETED>>","background_tasks":[]}}"#,
        JobId::new(id, row)
    );
    let spool_file = spool_dir.join("1700000000-1.jsonl");
    std::fs::write(&spool_file, format!("{line}\n")).unwrap();

    engine.replay_spool().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Done,
        "the spooled completion is applied"
    );
    assert!(
        !spool_file.exists(),
        "the spool file is deleted after replay"
    );

    // Re-spooling the same line is harmless: the idempotency key drops it and
    // the task stays Done.
    std::fs::write(&spool_file, format!("{line}\n")).unwrap();
    engine.replay_spool().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Done
    );
    assert!(!spool_file.exists());

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn unknown_task_signal_does_not_corrupt_state() {
    let base = scratch("hook_unknown_task");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    // A signal for a task that does not exist (job-999-1): accepted at the
    // boundary, parked here (E-09) — no panic, no state change to the real task.
    engine
        .on_signal(stop(
            999,
            1,
            "p1",
            StopStatus::Completed,
            Some("x <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();
    assert!(engine.db().get_task(999).unwrap().is_none());
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Running,
        "the real task is untouched"
    );
    // A well-formed signal for the real task still works afterwards.
    engine
        .on_signal(stop(
            id,
            row,
            "p2",
            StopStatus::Completed,
            Some("ok <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Done
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn dispatch_wires_job_id_and_hook_launch_spec() {
    let base = scratch("hook_dispatch");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");
    let db_path = base.join("state.db");

    // A hook-capable agent (resume_session) records its dispatch params.
    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t",
                                "instructions": "回答は日本語で作成してください。" }] }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "resume_session": true, "stream_states": ["running"], "dispatch_log": dispatch_log }),
        )
        .await,
    );
    plugins.notifiers.insert(
        "mock_notify".to_string(),
        launch(
            "notifier",
            "mock_notify",
            json!({ "notify_log": notify_log }),
        )
        .await,
    );

    let hook = HookRuntime {
        socket_path: base.join("claude.sock"),
        auth_token: Some(SecretString::new("tok3n")),
        spool_dir: Some(base.join("spool")),
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    let mut settings = engine_settings(workflows("llm", "none"), Some(hook));
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: None,
    }];
    settings.location_template = "{repo}/../wt/{worktree_name}".to_string();

    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;

    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    let row = engine.db().latest_session(task.id).unwrap().unwrap().id;
    let expected_job = JobId::new(task.id, row).to_string();

    engine.shutdown(GRACE).await;

    let dispatches = read_log(&dispatch_log);
    let params = &dispatches
        .iter()
        .find(|d| d["method"] == "task/dispatch")
        .expect("dispatch recorded")["params"];
    assert_eq!(
        params["job_id"], expected_job,
        "job_id minted from task + session row"
    );
    // #411: the separate `hook` spec is gone; the settings path is baked into
    // the argv and the hook env rides `tool_launch.env`.
    let tool = &params["tool_launch"];
    let args: Vec<&str> = tool["args"]
        .as_array()
        .expect("tool_launch args")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let settings_at = args
        .iter()
        .position(|a| *a == "--settings")
        .expect("--settings is in the argv");
    assert_eq!(
        args[settings_at + 1],
        base.join("orchestrator-wf.json").display().to_string()
    );
    assert_eq!(tool["env"]["TOTSUKA_JOB_ID"], expected_job);
    assert_eq!(
        tool["env"]["TOTSUKA_HOOK_ENDPOINT"],
        base.join("claude.sock").display().to_string()
    );
    assert_eq!(tool["env"]["TOTSUKA_HOOK_TOKEN"], "tok3n");
    assert_eq!(
        tool["env"]["TOTSUKA_HOOK_SPOOL_DIR"],
        base.join("spool").display().to_string()
    );
    // A hook dispatch delivers the task's instructions AND the marker
    // convention invisibly via TOTSUKA_PROMPT_CONTEXT (the UserPromptSubmit
    // hook turns it into additionalContext) — the marker still reaches the
    // model UP FRONT, so the FIRST Stop carries a marker and on-stop.sh never
    // blocks into a regenerated duplicate answer. Nothing rides the visible
    // extra_context anymore.
    assert!(
        params["extra_context"].is_null(),
        "hook dispatch carries no visible extra_context: {}",
        params["extra_context"]
    );
    let ctx = tool["env"]["TOTSUKA_PROMPT_CONTEXT"]
        .as_str()
        .expect("the launch env carries the prompt context");
    assert!(
        ctx.contains("回答は日本語で作成してください。"),
        "prompt context carries the task's instructions: {ctx}"
    );
    assert!(
        ctx.contains("<<STATUS:COMPLETED>>") && ctx.contains("NEEDS_INPUT"),
        "prompt context states the marker convention: {ctx}"
    );
    // ...and, for an implement-mode dispatch into a freshly created (therefore
    // detached) worktree, the instruction to create the branch. The condition
    // guarding this is read from the worktree's own HEAD rather than from the
    // task record, because the record is fetched once per dispatch and is
    // stale in exactly the case that matters: a re-creation whose recorded
    // branch no longer exists gets a detached worktree while the in-memory
    // copy still names one.
    assert!(
        ctx.contains("DETACHED HEAD") && ctx.contains("git switch -c"),
        "prompt context tells the agent to create the branch: {ctx}"
    );
    // Delivery contract (slack-reply real-machine finding): only the
    // marker-bearing final message is published, so it must be self-contained,
    // and no marker may be emitted while background tasks are still running
    // (that Stop is a heartbeat and the session gets re-invoked).
    assert!(
        ctx.contains("ONLY the message carrying the marker")
            && ctx.contains("do NOT emit a marker"),
        "prompt context states the delivery contract: {ctx}"
    );
    // #196: the fully-resolved tool launch is the only launch channel since
    // 0.4.0 (#411). Its argv must equal what herdr's `launch_command` built
    // for the same inputs before the tool registry existed (Phase 1 behavior
    // invariance: base `claude` + `--settings <workflow settings>`), and its
    // env must carry the hook environment the agent reports completion with.
    assert_eq!(tool["program"], "claude");
    assert_eq!(
        tool["args"],
        json!([
            "--settings",
            base.join("orchestrator-wf.json").display().to_string()
        ]),
        "the settings path is the whole argv for a claude implement dispatch"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #196 Phase 2: a repo pinned to the built-in `codex` tool dispatches a
/// codex argv — sandbox/approval flags instead of `--settings` (codex hooks
/// are registered globally and reached via the `TOTSUKA_*` env, which must
/// still ride the launch spec verbatim).
#[tokio::test]
async fn dispatch_with_codex_tool_builds_codex_argv() {
    let base = scratch("codex_dispatch");
    let repo = setup_repo(&base);
    let dispatch_log = base.join("dispatch.ndjson");
    let db_path = base.join("state.db");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t" }] }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "resume_session": true, "stream_states": ["running"], "dispatch_log": dispatch_log }),
        )
        .await,
    );

    let hook = HookRuntime {
        socket_path: base.join("agent-events.sock"),
        auth_token: None,
        spool_dir: None,
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    let mut settings = engine_settings(workflows("none", "none"), Some(hook));
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: Some("codex".to_string()),
    }];
    settings.location_template = "{repo}/../wt/{worktree_name}".to_string();

    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
    engine.shutdown(GRACE).await;

    let dispatches = read_log(&dispatch_log);
    let params = &dispatches
        .iter()
        .find(|d| d["method"] == "task/dispatch")
        .expect("dispatch recorded")["params"];
    let tool = &params["tool_launch"];
    assert_eq!(tool["program"], "codex");
    assert_eq!(
        tool["args"],
        json!([
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "never"
        ]),
        "codex implement argv: sandbox flags, no --settings, no approval prompt (#420)"
    );
    // The hook env still rides the codex launch verbatim (codex hooks are
    // registered globally and reached through `TOTSUKA_*`). Asserted as the
    // whole map, not one key: before #411 this compared `tool_launch.env`
    // against `hook.env` on the same dispatch, and dropping to a single key
    // when `hook` went away would have quietly stopped checking that nothing
    // extra leaks in. This runtime sets no token and no spool dir, so the
    // endpoint and the job id are the entire expected env.
    let env = tool["env"].as_object().expect("tool_launch env");
    let mut keys: Vec<&str> = env.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        // This runtime sets no token and no spool dir; codex supports
        // invisible injection, so the prompt context rides the env too.
        [
            "TOTSUKA_HOOK_ENDPOINT",
            "TOTSUKA_JOB_ID",
            "TOTSUKA_PROMPT_CONTEXT"
        ],
        "the codex launch env is exactly the hook env — no more, no less"
    );
    assert_eq!(
        env["TOTSUKA_HOOK_ENDPOINT"],
        json!(base.join("agent-events.sock").display().to_string())
    );
    assert_eq!(env["TOTSUKA_JOB_ID"], params["job_id"]);
    let _ = std::fs::remove_dir_all(&base);
}

/// #196 Phase 3: a repo pinned to the built-in `opencode` tool dispatches the
/// plain opencode TUI argv, and — because opencode has no invisible-injection
/// channel — the task instructions + marker convention ride the **visible**
/// extra_context instead of `TOTSUKA_PROMPT_CONTEXT`.
#[tokio::test]
async fn dispatch_with_opencode_tool_routes_context_visibly() {
    let base = scratch("opencode_dispatch");
    let repo = setup_repo(&base);
    let dispatch_log = base.join("dispatch.ndjson");
    let db_path = base.join("state.db");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t",
                                "instructions": "回答は日本語で作成してください。" }] }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "resume_session": true, "stream_states": ["running"], "dispatch_log": dispatch_log }),
        )
        .await,
    );

    let hook = HookRuntime {
        socket_path: base.join("agent-events.sock"),
        auth_token: None,
        spool_dir: None,
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    let mut settings = engine_settings(workflows("none", "none"), Some(hook));
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: Some("opencode".to_string()),
    }];
    settings.location_template = "{repo}/../wt/{worktree_name}".to_string();

    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
    engine.shutdown(GRACE).await;

    let dispatches = read_log(&dispatch_log);
    let params = &dispatches
        .iter()
        .find(|d| d["method"] == "task/dispatch")
        .expect("dispatch recorded")["params"];
    let tool = &params["tool_launch"];
    assert_eq!(tool["program"], "opencode");
    assert_eq!(
        tool["args"],
        json!(["--auto"]),
        "implement mode launches the plain TUI, unattended (#420)"
    );
    // Visible routing: instructions + marker convention in extra_context …
    let ctx = params["extra_context"]
        .as_str()
        .expect("visible extra_context for a non-injecting tool");
    assert!(ctx.contains("回答は日本語で作成してください。"), "{ctx}");
    assert!(ctx.contains("<<STATUS:COMPLETED>>"), "{ctx}");
    // … and no invisible channel; the rest of the hook env still rides.
    assert!(
        tool["env"].get("TOTSUKA_PROMPT_CONTEXT").is_none(),
        "no invisible channel for opencode: {}",
        tool["env"]
    );
    assert!(tool["env"].get("TOTSUKA_JOB_ID").is_some());
    let _ = std::fs::remove_dir_all(&base);
}

/// #487: the confirm self-report has a question-tool variant, selected at
/// dispatch time by profile × tool. A `design` dispatch to claude is told to
/// ask through `AskUserQuestion`; the same workflow dispatched to codex —
/// which has no question tool outside plan mode — keeps the marker protocol
/// with numbered choices. Driven through the real dispatch path because the
/// selection lives in `wire_hooks`, not in `Prompts::resolve_for`.
#[tokio::test]
async fn a_design_dispatch_selects_the_question_variant_per_tool() {
    for (tool_pin, expect, reject) in [
        (
            None, // default_tool = claude
            "ask the human to confirm through the AskUserQuestion tool",
            "1. Approve completion / 2. Request changes",
        ),
        (
            Some("codex".to_string()),
            "1. Approve completion / 2. Request changes",
            "AskUserQuestion",
        ),
    ] {
        let base = scratch("question_variant_dispatch");
        let repo = setup_repo(&base);
        let dispatch_log = base.join("dispatch.ndjson");
        let db_path = base.join("state.db");

        let mut plugins = PluginSet::default();
        plugins.sources.insert(
            "mock_src".to_string(),
            launch(
                "task_source",
                "mock_src",
                json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t" }] }),
            )
            .await,
        );
        plugins.agents.insert(
            "mock_agent".to_string(),
            launch(
                "agent_ide",
                "mock_agent",
                json!({ "resume_session": true, "stream_states": ["running"], "dispatch_log": dispatch_log }),
            )
            .await,
        );

        let hook = HookRuntime {
            socket_path: base.join("agent-events.sock"),
            auth_token: None,
            spool_dir: None,
            settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
            block_retry_limit: 3,
        };
        let cfg = RootConfig::from_toml_str(
            r#"
[[workflows]]
name = "wf"
source = "mock_src"
trigger = {}
profile = "design"
agent = "mock_agent"
on_success = { set_status = "done" }
on_failure = { set_status = "failed" }
"#,
        )
        .unwrap();
        let mut settings = engine_settings(Workflow::from_configs(&cfg.workflows), Some(hook));
        settings.prompts = orchestrator_core::prompts::PromptSet::from_config(&cfg);
        settings.repos = vec![RepoSettings {
            name: "clone".to_string(),
            path: repo.clone(),
            summary: None,
            worktree_location: None,
            tool: tool_pin.clone(),
        }];
        settings.location_template = "{repo}/../wt/{worktree_name}".to_string();

        let mut engine = Engine::new(
            StateDb::open(&db_path).unwrap(),
            settings,
            plugins,
            SystemGitRunner,
            no_llm(),
        )
        .await;

        let dispatch_probe = dispatch_log.clone();
        run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
        engine.shutdown(GRACE).await;

        let dispatches = read_log(&dispatch_log);
        let params = &dispatches
            .iter()
            .find(|d| d["method"] == "task/dispatch")
            .expect("dispatch recorded")["params"];
        // Both tools inject invisibly, so the self-report rides the env.
        let ctx = params["tool_launch"]["env"]["TOTSUKA_PROMPT_CONTEXT"]
            .as_str()
            .expect("prompt context rides the launch env");
        assert!(
            ctx.contains(expect),
            "tool {tool_pin:?} must be taught its own asking protocol: {ctx}"
        );
        assert!(
            !ctx.contains(reject),
            "tool {tool_pin:?} must not be taught the other tool's protocol: {ctx}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// The `[[workflows]].initial_prompt` used by the #415 tests below.
const INITIAL_PROMPT: &str = "/grill-me スキルを使用して、詳細設計を行ってください";

/// #415: `[[workflows]].initial_prompt` reaches the pane as the **first**
/// thing the agent reads, ahead of the task body — and only when the agent is
/// starting a fresh conversation.
///
/// Claude injects invisibly, so with no `initial_prompt` this dispatch's
/// visible `extra_context` is `null` (asserted elsewhere); here it is the
/// preamble and nothing else.
#[tokio::test]
async fn an_initial_prompt_leads_a_new_conversation() {
    let base = scratch("initial_prompt_new");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let plugins = resume_plugins(
        json!([{ "id": "1", "source": "github", "title": "t" }]),
        &dispatch_log,
        &notify_log,
    )
    .await;
    let mut settings = resume_settings(&repo, &base);
    for wf in &mut settings.workflows {
        wf.initial_prompt = Some(INITIAL_PROMPT.to_string());
    }
    let mut engine = Engine::new(
        StateDb::open(&base.join("state.db")).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
    engine.shutdown(GRACE).await;

    let params = last_dispatch_params(&dispatch_log);
    assert!(
        params["resume_session_id"].is_null(),
        "a first dispatch resumes nothing: {params}"
    );
    assert_eq!(
        params["extra_context"], INITIAL_PROMPT,
        "the preamble is the whole visible context for an invisibly-injecting tool"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #415: a resume dispatch does **not** re-send it. `/grill-me` and its kind
/// are start-of-work declarations; re-typing one three turns into a
/// conversation restarts the skill and destroys the context it was given.
#[tokio::test]
async fn an_initial_prompt_is_not_repeated_on_a_resume() {
    let base = scratch("initial_prompt_resume");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    seed_finished_conversation(&db, "1", Some("cc-prior"));

    let plugins = resume_plugins(follow_up("1", "1:reply"), &dispatch_log, &notify_log).await;
    let mut settings = resume_settings(&repo, &base);
    for wf in &mut settings.workflows {
        wf.initial_prompt = Some(INITIAL_PROMPT.to_string());
    }
    let mut engine =
        Engine::with_clock(db, settings, plugins, SystemGitRunner, no_llm(), clock).await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
    engine.shutdown(GRACE).await;

    let params = last_dispatch_params(&dispatch_log);
    assert_eq!(params["resume_session_id"], "cc-prior");
    assert!(
        params["extra_context"].is_null(),
        "the agent already read it in this same conversation: {params}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #415 × `SESSION_UNRESUMABLE`: the retry names no session, so it really is a
/// fresh conversation — and the preamble has to come back with it.
///
/// This is the whole reason the `resume.is_none()` test sits **inside**
/// `build_params` rather than being computed once before it. Computed outside,
/// the retry would inherit the resume dispatch's answer and start an agent
/// that never sees the instructions.
#[tokio::test]
async fn an_initial_prompt_returns_when_a_resume_turns_out_to_be_impossible() {
    let base = scratch("initial_prompt_unresumable");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    seed_finished_conversation(&db, "1", Some("cc-gone"));

    let plugins = resume_plugins_with(
        follow_up("1", "1:reply"),
        &dispatch_log,
        &notify_log,
        json!({ "dispatch_error": {
            "code": plugin_protocol::error_code::SESSION_UNRESUMABLE,
            "message": "the agent session could not be resumed",
            "only_when_resuming": true,
        }}),
    )
    .await;
    let mut settings = resume_settings(&repo, &base);
    for wf in &mut settings.workflows {
        wf.initial_prompt = Some(INITIAL_PROMPT.to_string());
    }
    let mut engine =
        Engine::with_clock(db, settings, plugins, SystemGitRunner, no_llm(), clock).await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || dispatches(&dispatch_probe).len() >= 2).await;
    engine.shutdown(GRACE).await;

    let attempts = dispatches(&dispatch_log);
    assert_eq!(attempts.len(), 2, "{attempts:#?}");
    assert_eq!(attempts[0]["params"]["resume_session_id"], "cc-gone");
    assert!(
        attempts[0]["params"]["extra_context"].is_null(),
        "the resume attempt still carries nothing: {}",
        attempts[0]["params"]
    );
    assert!(attempts[1]["params"]["resume_session_id"].is_null());
    assert_eq!(
        attempts[1]["params"]["extra_context"], INITIAL_PROMPT,
        "the agent this actually starts remembers nothing, so it is told again"
    );
    let _ = std::fs::remove_dir_all(&base);
}

/// #415 on a tool with no invisible channel: the preamble leads and everything
/// that used to be the whole visible context — the task's instructions and the
/// marker convention — follows it, in that order.
#[tokio::test]
async fn an_initial_prompt_precedes_the_visible_marker_convention() {
    let base = scratch("initial_prompt_opencode");
    let repo = setup_repo(&base);
    let dispatch_log = base.join("dispatch.ndjson");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t",
                                "instructions": "回答は日本語で作成してください。" }] }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "resume_session": true, "stream_states": ["running"], "dispatch_log": dispatch_log }),
        )
        .await,
    );

    let hook = HookRuntime {
        socket_path: base.join("agent-events.sock"),
        auth_token: None,
        spool_dir: None,
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    let mut settings = engine_settings(workflows("none", "none"), Some(hook));
    for wf in &mut settings.workflows {
        wf.initial_prompt = Some(INITIAL_PROMPT.to_string());
    }
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: Some("opencode".to_string()),
    }];
    settings.location_template = "{repo}/../wt/{worktree_name}".to_string();

    let mut engine = Engine::new(
        StateDb::open(&base.join("state.db")).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
    engine.shutdown(GRACE).await;

    let ctx = last_dispatch_params(&dispatch_log)["extra_context"]
        .as_str()
        .expect("visible extra_context for a non-injecting tool")
        .to_string();
    assert!(
        ctx.starts_with(INITIAL_PROMPT),
        "the preamble leads: {ctx:?}"
    );
    let instructions = ctx
        .find("回答は日本語で作成してください。")
        .expect("instructions still delivered");
    let marker = ctx
        .find("<<STATUS:COMPLETED>>")
        .expect("marker convention still delivered");
    assert!(
        INITIAL_PROMPT.len() < instructions && INITIAL_PROMPT.len() < marker,
        "nothing the completion contract depends on was displaced: {ctx:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn dispatch_without_hook_falls_back_to_visible_extra_context() {
    // A non-hook agent (no resume_session / diagnostics_snapshot) has no
    // invisible channel: the task's instructions fall back to plain string
    // extra_context (and no marker convention — non-hook agents don't report
    // completion through hooks).
    let base = scratch("nohook_dispatch");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t",
                                "instructions": "回答は日本語で作成してください。" }] }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "stream_states": ["running"], "dispatch_log": dispatch_log }),
        )
        .await,
    );
    plugins.notifiers.insert(
        "mock_notify".to_string(),
        launch(
            "notifier",
            "mock_notify",
            json!({ "notify_log": notify_log }),
        )
        .await,
    );

    let mut settings = engine_settings(workflows("llm", "none"), None);
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: None,
    }];
    settings.location_template = "{repo}/../wt/{worktree_name}".to_string();

    let mut engine = Engine::new(
        StateDb::open(&base.join("state.db")).unwrap(),
        settings,
        plugins,
        SystemGitRunner,
        no_llm(),
    )
    .await;
    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;
    engine.shutdown(GRACE).await;

    let dispatches = read_log(&dispatch_log);
    let params = &dispatches
        .iter()
        .find(|d| d["method"] == "task/dispatch")
        .expect("dispatch recorded")["params"];
    assert!(params["hook"].is_null(), "no hook launch spec");
    assert_eq!(
        params["extra_context"], "回答は日本語で作成してください。",
        "instructions fall back to visible extra_context"
    );
    assert!(
        !params["extra_context"]
            .as_str()
            .unwrap()
            .contains("<<STATUS:"),
        "no marker convention on the non-hook path"
    );
    let _ = std::fs::remove_dir_all(&base);
}

// Review follow-up (#149).

#[tokio::test]
async fn duplicate_heartbeat_refreshes_liveness_and_prevents_false_escalation() {
    // Regression: mid-turn Stops all collapse to a single `heartbeat`
    // idempotency key with a re-used prompt_id, so the 2nd+ delivery is a
    // Duplicate. The liveness anchor must still refresh, or the timeout sweep
    // would falsely escalate a task that is very much alive.
    let base = scratch("hook_hb_liveness");
    let notify_log = base.join("notify.ndjson");
    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    // Seed the anchor at T0, then move past the 30-minute default timeout:
    // without the refresh below, the sweep WOULD escalate (#174).
    let id = db.upsert_task(&new_task("1", Some(T0))).unwrap();
    db.apply_event(id, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(id, TaskEvent::Start, None).unwrap();
    let row = db.record_session(id, "mock_agent", "sess-1").unwrap();

    let mut engine = Engine::with_clock(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
        clock.clone(),
    )
    .await;
    clock.advance(time::Duration::seconds(1801));

    // Pre-seed the exact hook_event so the incoming heartbeat is a Duplicate.
    engine
        .db()
        .record_hook_event(&HookEventInsert {
            job_id: JobId::new(id, row).to_string(),
            task_id: id,
            tool_session_id: "cc-1".into(),
            prompt_id: "hb".into(),
            event: "heartbeat".into(),
            status: None,
            payload: "{}".into(),
        })
        .unwrap();

    // The duplicate heartbeat must STILL refresh last_signal_at — to the
    // injected clock's current instant, exactly.
    engine.on_signal(heartbeat(id, row, "hb")).await.unwrap();
    let after = engine.db().get_task(id).unwrap().unwrap();
    assert_eq!(
        after.last_signal_at,
        Some(clock.now_rfc3339()),
        "duplicate heartbeat refreshed the timeout anchor"
    );

    // A task that just proved liveness must not be swept.
    engine.sweep_signal_timeouts().await.unwrap();
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Running,
        "live task must not be falsely escalated"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn failed_hook_dispatch_rolls_back_reserved_session() {
    // A hook-capable agent that crashes mid-dispatch: the pre-dispatch session
    // reservation must be rolled back so no empty-id row leaks for retry.
    let base = scratch("hook_dispatch_fail");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let db_path = base.join("state.db");

    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": [{ "id": "1", "source": "github", "title": "t" }] }),
        )
        .await,
    );
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch(
            "agent_ide",
            "mock_agent",
            json!({ "resume_session": true, "crash_on_dispatch": true }),
        )
        .await,
    );
    plugins.notifiers.insert(
        "mock_notify".to_string(),
        launch(
            "notifier",
            "mock_notify",
            json!({ "notify_log": notify_log }),
        )
        .await,
    );

    let hook = HookRuntime {
        socket_path: base.join("claude.sock"),
        auth_token: None,
        spool_dir: None,
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    let mut settings = engine_settings(workflows("llm", "none"), Some(hook));
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: None,
    }];

    let mut engine = Engine::new(
        StateDb::open(&db_path).unwrap(),
        settings,
        plugins,
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
            .is_some_and(|t| t.state == TaskState::Failed)
    })
    .await;

    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(
        task.state,
        TaskState::Failed,
        "the crashed dispatch fails the task"
    );
    assert!(
        engine.db().latest_session(task.id).unwrap().is_none(),
        "the reserved session row was rolled back (no empty-id leak)"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn spool_replay_quarantines_file_with_corrupt_line() {
    // A file with a good line + a corrupt line: the good line is applied, but
    // the file is quarantined (renamed) rather than deleted, so the corrupt
    // data is preserved for inspection.
    let base = scratch("hook_spool_corrupt");
    let notify_log = base.join("notify.ndjson");
    let spool_dir = base.join("spool");
    std::fs::create_dir_all(&spool_dir).unwrap();
    let db = StateDb::open(&base.join("state.db")).unwrap();
    let (id, row) = seed_running(&db, "sess-1");

    let hook = HookRuntime {
        socket_path: base.join("sock"),
        auth_token: None,
        spool_dir: Some(spool_dir.clone()),
        settings_paths: HashMap::new(),
        block_retry_limit: 3,
    };
    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), Some(hook)),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    let good = format!(
        r#"{{"job_id":"{}","hook_event_name":"Stop","status":"COMPLETED","last_assistant_message":"ok <<STATUS:COMPLETED>>","background_tasks":[]}}"#,
        JobId::new(id, row)
    );
    let file = spool_dir.join("1700000000-1.jsonl");
    std::fs::write(&file, format!("{good}\n{{not json\n")).unwrap();

    engine.replay_spool().await.unwrap();

    // The good line was applied.
    assert_eq!(
        engine.db().get_task(id).unwrap().unwrap().state,
        TaskState::Done,
        "the clean line is still processed"
    );
    // The file is quarantined, not deleted.
    assert!(!file.exists(), "the original file is renamed away");
    assert!(
        spool_dir.join("1700000000-1.jsonl.corrupt").exists(),
        "the corrupt file is quarantined for inspection"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

// ── Slack thread conversation continuity (#140) ──────────────────────────────
//
// A follow-up mention in the same thread is ingested as a new task (unique
// `source_task_id`) but resumes the prior task's Claude session via
// `claude --resume`. Correlation lives in the DB (survives restarts). These
// tests drive a real dispatch through `cycle` against a hook-capable mock
// agent that records its `task/dispatch` params, and assert the
// `resume_session_id` wiring plus the E-09 mis-thread protection.

/// A hook-capable agent (records dispatch params to `dispatch_log`), a source
/// returning `fetched`, and a recording notifier.
async fn resume_plugins(
    fetched: serde_json::Value,
    dispatch_log: &Path,
    notify_log: &Path,
) -> PluginSet {
    resume_plugins_with(fetched, dispatch_log, notify_log, json!({})).await
}

/// [`resume_plugins`] with `agent_extra`'s fields merged into the agent's init
/// config (e.g. `dispatch_error`, to script a failing dispatch).
async fn resume_plugins_with(
    fetched: serde_json::Value,
    dispatch_log: &Path,
    notify_log: &Path,
    agent_extra: serde_json::Value,
) -> PluginSet {
    let mut plugins = PluginSet::default();
    plugins.sources.insert(
        "mock_src".to_string(),
        launch(
            "task_source",
            "mock_src",
            json!({ "task_submit": true, "submit_tasks": fetched }),
        )
        .await,
    );
    let mut agent_config = json!({
        "resume_session": true, "stream_states": ["running"], "dispatch_log": dispatch_log,
    });
    if let Some(extra) = agent_extra.as_object() {
        let base = agent_config.as_object_mut().expect("an object literal");
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    plugins.agents.insert(
        "mock_agent".to_string(),
        launch("agent_ide", "mock_agent", agent_config).await,
    );
    plugins.notifiers.insert(
        "mock_notify".to_string(),
        launch(
            "notifier",
            "mock_notify",
            json!({ "notify_log": notify_log }),
        )
        .await,
    );
    plugins
}

/// Engine settings for a resume test: the `wf` workflow on a real repo clone,
/// with a hook runtime so the hook-dispatch path (job_id + resume) is taken.
fn resume_settings(repo: &Path, base: &Path) -> EngineSettings {
    let hook = HookRuntime {
        socket_path: base.join("claude.sock"),
        auth_token: Some(SecretString::new("tok3n")),
        spool_dir: Some(base.join("spool")),
        settings_paths: HashMap::from([("wf".to_string(), base.join("orchestrator-wf.json"))]),
        block_retry_limit: 3,
    };
    let mut settings = engine_settings(workflows("llm", "none"), Some(hook));
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.to_path_buf(),
        summary: None,
        worktree_location: None,
        tool: None,
    }];
    settings.location_template = "{repo}/../wt/{worktree_name}".to_string();
    settings
}

/// Seed a conversation that has already run once and finished: a task, a
/// delivered first message, a recorded session, and a terminal state — the
/// shape a Slack thread has after its opening message was answered.
///
/// `tool_sid = Some` simulates the SessionStart hook having established the
/// tool session id; `None` leaves it unestablished (pre-hook era).
fn seed_finished_conversation(db: &StateDb, source_task_id: &str, tool_sid: Option<&str>) -> i64 {
    let id = db.upsert_task(&new_task(source_task_id, Some(T0))).unwrap();
    db.append_task_message(&TaskMessageInsert {
        task_id: id,
        message_key: source_task_id.to_string(),
        author: None,
        body: "the opening message".to_string(),
        url: None,
        payload: "{}".to_string(),
    })
    .unwrap();
    db.mark_messages_processed(id).unwrap();
    let row = db
        .record_session(id, "mock_agent", &format!("sess-{source_task_id}"))
        .unwrap();
    if let Some(sid) = tool_sid {
        db.set_tool_session_id(row, sid).unwrap();
    }
    // The earlier run left a worktree and branch recorded, as every real one
    // does. That matters: `recovery::retry_plan` reads (worktree_path,
    // session) and never the task's state, so a reopened conversation always
    // *looks* reusable — the case a follow-up message must not be swallowed by
    // (#242). The path is deliberately gone from disk, the state a cleaned-up
    // worktree leaves behind; #254 re-creates it.
    db.set_worktree(
        id,
        &format!("/nonexistent/wt/mock_src-{source_task_id}"),
        Some(&format!("agent/mock_src-{source_task_id}")),
        "c0ffeec0ffeec0ffeec0ffeec0ffeec0ffeec0ff",
    )
    .unwrap();
    for event in [
        TaskEvent::Dispatch,
        TaskEvent::Start,
        TaskEvent::BeginPublish,
        TaskEvent::Complete,
    ] {
        db.apply_event(id, event, None).unwrap();
    }
    id
}

/// The params of the last recorded `task/dispatch` in `dispatch_log`.
fn last_dispatch_params(dispatch_log: &Path) -> serde_json::Value {
    read_log(dispatch_log)
        .into_iter()
        .rev()
        .find(|d| d["method"] == "task/dispatch")
        .expect("a task/dispatch was recorded")["params"]
        .clone()
}

/// A follow-up message *inside* an existing conversation: since #242 it
/// carries the conversation's own id and a distinct `message_key`, so ingest
/// reopens that task instead of creating a new one.
fn follow_up(conversation_id: &str, message_key: &str) -> serde_json::Value {
    json!([{
        "id": conversation_id, "source": "github", "title": "follow-up",
        "message_key": message_key, "body": "and one more thing",
    }])
}

#[tokio::test]
async fn a_follow_up_message_reopens_the_conversation_and_resumes_its_session() {
    // ① A second message in a finished conversation reopens *that task* and
    // dispatches WITH `resume_session_id` — the session to resume is the
    // task's own latest, not another task's (#242 supersedes #140's D-10).
    let base = scratch("resume_second");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    let conversation = seed_finished_conversation(&db, "1", Some("cc-prior"));

    // The follow-up carries the conversation's own id (#242).
    let plugins = resume_plugins(follow_up("1", "1:reply"), &dispatch_log, &notify_log).await;
    let mut engine = Engine::with_clock(
        db,
        resume_settings(&repo, &base),
        plugins,
        SystemGitRunner,
        no_llm(),
        clock,
    )
    .await;
    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;

    let params = last_dispatch_params(&dispatch_log);
    assert_eq!(
        params["resume_session_id"], "cc-prior",
        "the reopened conversation resumes its own session"
    );
    // The agent is asked only about the new message: the resumed session
    // already holds everything before it.
    assert_eq!(params["task"]["body"], "and one more thing");

    // Still one task, not two — and it has a worktree on disk (re-created by
    // #254 if the earlier run's was cleaned up).
    let tasks = engine.db().list_tasks().unwrap();
    assert_eq!(tasks.len(), 1, "a reply is the same conversation");
    let follow = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert_eq!(follow.id, conversation);
    let wt = follow.worktree_path.expect("a worktree was recorded");
    assert!(Path::new(&wt).exists(), "the worktree exists on disk");

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn unestablished_prior_session_falls_back_to_fresh_dispatch() {
    // ② Prior task never got a tool_session_id (pre-hook era) → no resume,
    // normal fresh dispatch, no warning.
    let base = scratch("resume_fallback");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    seed_finished_conversation(&db, "1", None); // session, but no tool id

    let plugins = resume_plugins(follow_up("1", "1:reply"), &dispatch_log, &notify_log).await;
    let mut engine = Engine::with_clock(
        db,
        resume_settings(&repo, &base),
        plugins,
        SystemGitRunner,
        no_llm(),
        clock,
    )
    .await;
    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;

    let params = last_dispatch_params(&dispatch_log);
    assert!(
        params.get("resume_session_id").is_none(),
        "no established prior session → no resume (field omitted): {params}"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn a_reopened_conversation_resumes_its_latest_session() {
    // ③ A conversation dispatched more than once has several session rows;
    // the reopen resumes the newest, never an earlier one.
    let base = scratch("resume_latest");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    let conversation = seed_finished_conversation(&db, "1", Some("cc-1"));
    // A second run of the same conversation left a newer session behind.
    let newer = db
        .record_session(conversation, "mock_agent", "sess-2")
        .unwrap();
    db.set_tool_session_id(newer, "cc-2").unwrap();

    let plugins = resume_plugins(follow_up("1", "1:reply"), &dispatch_log, &notify_log).await;
    let mut engine = Engine::with_clock(
        db,
        resume_settings(&repo, &base),
        plugins,
        SystemGitRunner,
        no_llm(),
        clock,
    )
    .await;
    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;

    let params = last_dispatch_params(&dispatch_log);
    assert_eq!(
        params["resume_session_id"], "cc-2",
        "the newest session wins, not the first one"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn distinct_conversations_do_not_cross_resume() {
    // ④ Another conversation's session is never resumed. Since #242 this is
    // structural — the session is looked up on the task itself, and there is
    // no cross-task search left to go wrong — so the test guards against
    // reintroducing one.
    let base = scratch("resume_isolation");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    seed_finished_conversation(&db, "1", Some("cc-other-conversation"));

    // A message opening a *different* conversation.
    let plugins = resume_plugins(follow_up("2", "2:first"), &dispatch_log, &notify_log).await;
    let mut engine = Engine::with_clock(
        db,
        resume_settings(&repo, &base),
        plugins,
        SystemGitRunner,
        no_llm(),
        clock,
    )
    .await;
    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || !read_log(&dispatch_probe).is_empty()).await;

    let params = last_dispatch_params(&dispatch_log);
    assert!(
        params.get("resume_session_id").is_none(),
        "a different conversation never cross-resumes: {params}"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn reply_destination_is_task_id_origin_never_the_shared_session_id() {
    // E-09: even when two tasks share the *same* Claude session id (a resumed
    // conversation), a Stop signal is routed to the task named by its
    // `job_id` — never guessed from the session id. So the reply destination is
    // always the completing task's own `source_task_id`, and a resumed session
    // can never mis-route a reply into the prior task's thread.
    let base = scratch("resume_e09");
    let notify_log = base.join("notify.ndjson");
    let db = StateDb::open(&base.join("state.db")).unwrap();

    // Two tasks sharing tool_session_id "cc-shared".
    let prior = db.upsert_task(&new_task("1", None)).unwrap();
    db.apply_event(prior, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(prior, TaskEvent::Start, None).unwrap();
    let prior_row = db.record_session(prior, "mock_agent", "sess-1").unwrap();
    db.set_tool_session_id(prior_row, "cc-shared").unwrap();

    let follow = db.upsert_task(&new_task("2", None)).unwrap();
    db.apply_event(follow, TaskEvent::Dispatch, None).unwrap();
    db.apply_event(follow, TaskEvent::Start, None).unwrap();
    let follow_row = db.record_session(follow, "mock_agent", "sess-2").unwrap();
    db.set_tool_session_id(follow_row, "cc-shared").unwrap();

    let mut engine = Engine::new(
        db,
        engine_settings(workflows("llm", "none"), None),
        plugin_set(json!({}), &notify_log).await,
        SystemGitRunner,
        no_llm(),
    )
    .await;

    // A completion for the follow-up: job_id carries the follow-up's task id.
    engine
        .on_signal(stop(
            follow,
            follow_row,
            "p1",
            StopStatus::Completed,
            Some("done <<STATUS:COMPLETED>>"),
        ))
        .await
        .unwrap();

    // Only the follow-up (the job_id's task) advanced; the prior — which shares
    // the session id — is untouched.
    assert_eq!(
        engine.db().get_task(follow).unwrap().unwrap().state,
        TaskState::Done,
        "the job_id's task is the reply destination"
    );
    assert_eq!(
        engine.db().get_task(prior).unwrap().unwrap().state,
        TaskState::Running,
        "the other task sharing the session id is never routed to"
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}

/// Every recorded `task/dispatch`, oldest first.
fn dispatches(dispatch_log: &Path) -> Vec<serde_json::Value> {
    read_log(dispatch_log)
        .into_iter()
        .filter(|d| d["method"] == "task/dispatch")
        .collect()
}

#[tokio::test]
async fn an_unresumable_session_is_dispatched_once_more_without_it() {
    // #242/#261: resuming can always fail (the tool's session store is outside
    // the worktree and outside our control). When the agent plugin says so with
    // `SESSION_UNRESUMABLE`, the task must still go out — resuming is an
    // optimization, not a precondition — so core drops the session and
    // dispatches once more. The conversation's context is lost with it; the
    // work is not.
    let base = scratch("resume_unresumable");
    let repo = setup_repo(&base);
    let notify_log = base.join("notify.ndjson");
    let dispatch_log = base.join("dispatch.ndjson");

    let clock = manual_clock();
    let db = StateDb::open_with_clock(&base.join("state.db"), clock.clone()).unwrap();
    seed_finished_conversation(&db, "1", Some("cc-gone"));

    // The agent refuses exactly the dispatches that name a session — the shape
    // a real `claude --resume <missing id>` produces (agent-ide-herdr maps its
    // vanished pane to this code, #261).
    let plugins = resume_plugins_with(
        follow_up("1", "1:reply"),
        &dispatch_log,
        &notify_log,
        json!({ "dispatch_error": {
            "code": plugin_protocol::error_code::SESSION_UNRESUMABLE,
            "message": "the agent session could not be resumed",
            "only_when_resuming": true,
        }}),
    )
    .await;
    let mut engine = Engine::with_clock(
        db,
        resume_settings(&repo, &base),
        plugins,
        SystemGitRunner,
        no_llm(),
        clock,
    )
    .await;
    let dispatch_probe = dispatch_log.clone();
    run_until(&mut engine, move || dispatches(&dispatch_probe).len() >= 2).await;

    let attempts = dispatches(&dispatch_log);
    assert_eq!(
        attempts.len(),
        2,
        "one refused attempt, then one retry — and no more: naming no session, \
         the retry cannot fail the same way, so this must never loop: {attempts:#?}"
    );
    assert_eq!(
        attempts[0]["params"]["resume_session_id"], "cc-gone",
        "the first attempt is the ordinary resume"
    );
    assert!(
        attempts[1]["params"].get("resume_session_id").is_none(),
        "the retry names no session: {}",
        attempts[1]["params"]
    );
    // The whole launch spec is rebuilt, not just the field: the resume id is
    // baked into the argv (#196), so a retry that only cleared the field would
    // still launch `--resume <gone>` and fail identically.
    let argv = |attempt: &serde_json::Value| {
        attempt["params"]["tool_launch"]["args"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    assert!(
        argv(&attempts[0]).iter().any(|a| a == "--resume"),
        "the first attempt's argv resumes: {:?}",
        argv(&attempts[0])
    );
    assert!(
        !argv(&attempts[1]).iter().any(|a| a == "--resume"),
        "the retry's argv must not: {:?}",
        argv(&attempts[1])
    );

    // The task moved on rather than failing, and the message it was carrying
    // went out with it (a failed dispatch would have left it queued).
    let task = engine
        .db()
        .find_by_source("mock_src", "1")
        .unwrap()
        .unwrap();
    assert!(
        matches!(task.state, TaskState::Dispatched | TaskState::Running),
        "the task is under way, not failed: {}",
        task.state
    );

    engine.shutdown(GRACE).await;
    let _ = std::fs::remove_dir_all(&base);
}
