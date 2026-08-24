//! Plugin liveness and restart, end to end (#495).
//!
//! The behaviour under test is what the run loop does when a plugin process
//! dies without being asked to. Before #495 that was only noticed for
//! `agent_ide`, and only via its notification stream; a dead `task_source`
//! produced no event at all.
//!
//! Every test here drives a **real subprocess** that really exits — the
//! `suicide` mode of `mock_plugin` — because the interesting part is the
//! transport noticing an EOF it did not ask for.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::adapters::state_db::StateDb;
use orchestrator_core::config::RootConfig;
use orchestrator_core::domain::state::TaskState;
use orchestrator_core::domain::workflow::Workflow;
use orchestrator_core::repo_select::SelectConfig;
use orchestrator_core::run::{
    Engine, EngineSettings, PluginSet, RepoSettings, RestartPolicy, RunSummary,
};
use orchestrator_core::scheduler::Limits;
use orchestrator_core::worktree::{CleanupPolicy, DEFAULT_WORKTREE_NAME_TEMPLATE};
use plugin_protocol::manifest::Manifest;
use serde_json::json;

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::OpenAiRouter;
use orchestrator_core::logging::{LogFormat, RedactingLayer};

const RUN_TIMEOUT: Duration = Duration::from_secs(30);

fn mock_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_mock_plugin"))
}

fn spec(kind: &str, name: &str, init_config: serde_json::Value) -> PluginSpec {
    let manifest = Manifest::from_toml_str(&format!(
        r#"
name = "{name}"
kind = "{kind}"
version = "0.1.0"
protocol_version = ">=0.6.0, <0.7"
"#
    ))
    .unwrap();
    PluginSpec {
        name: name.to_string(),
        program: mock_plugin(),
        args: vec![],
        manifest,
        init_config,
        repositories: vec![],
        llm: None,
        workflows: vec![],
        poll_interval_secs: None,
        timeout: Duration::from_secs(10),
    }
}

/// Launch a plugin **and record its spec**, which is what makes it
/// restartable. A `PluginSet` built without specs is detected-only by design,
/// so a test that forgets this would silently assert nothing.
async fn install(set: &mut PluginSet, kind: &str, name: &str, config: serde_json::Value) {
    let spec = spec(kind, name, config);
    set.specs.insert(name.to_string(), spec.clone());
    let plugin = Plugin::launch(spec).await.expect("launch mock plugin");
    match kind {
        "task_source" => set.sources.insert(name.to_string(), plugin),
        "agent_ide" => set.agents.insert(name.to_string(), plugin),
        _ => set.notifiers.insert(name.to_string(), plugin),
    };
}

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
"#,
    )
    .unwrap();
    Workflow::from_configs(&cfg.workflows)
}

/// Settings with a **zero backoff** — the seam that keeps these tests instant.
/// Restart *ordering* is what is under test; the sleep is production pacing.
fn settings(max_attempts: u32) -> EngineSettings {
    settings_with_backoff(max_attempts, Duration::ZERO)
}

/// [`settings`] with a real backoff, for the one test that needs the plugin to
/// still be **down** when a dispatch is attempted. Zero backoff relaunches so
/// fast that no crash window exists to observe.
fn settings_with_backoff(max_attempts: u32, first_backoff: Duration) -> EngineSettings {
    EngineSettings {
        workflows: workflows(),
        repos: vec![RepoSettings {
            name: "clone".to_string(),
            path: PathBuf::from("/nonexistent"),
            summary: None,
            worktree_location: None,
            tool: None,
        }],
        limits: Limits::global(2),
        worktree_name_template: DEFAULT_WORKTREE_NAME_TEMPLATE.to_string(),
        location_template: "{repo}/../wt/{worktree_name}".to_string(),
        cleanup_implement: CleanupPolicy::Immediate,
        cleanup_plan: CleanupPolicy::Immediate,
        env: HashMap::new(),
        select: SelectConfig::default(),
        readme_cache_dir: None,
        worktree_sweep_interval: Duration::ZERO,
        one_shot_grace: Duration::ZERO,
        tools: orchestrator_core::tool::builtin_registry(),
        default_tool: "claude".to_string(),
        prompts: Default::default(),
        plugin_restart: RestartPolicy {
            max_attempts,
            window: Duration::from_secs(300),
            first_backoff,
        },
        restart_disabled: Default::default(),
        hook: None,
    }
}

/// Drive a watch-mode run until `cond` holds, then stop it.
async fn run_until(
    engine: &mut Engine<SystemGitRunner, OpenAiRouter>,
    cond: impl Fn() -> bool,
) -> RunSummary {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let mut stop_tx = Some(stop_tx);
    let run_fut = engine.run(true, async move {
        let _ = stop_rx.await;
    });
    tokio::pin!(run_fut);
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        tokio::select! {
            result = &mut run_fut => return result.expect("run loop error"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if stop_tx.is_some() && cond() {
                    let _ = stop_tx.take().unwrap().send(());
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "condition not reached within {}s",
                    RUN_TIMEOUT.as_secs()
                );
            }
        }
    }
}

/// Drive a watch-mode run for a fixed window, then stop it. For assertions
/// whose subject is that a plugin merely *started*, with no event to wait for.
async fn run_for(
    engine: &mut Engine<SystemGitRunner, OpenAiRouter>,
    window: Duration,
) -> RunSummary {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(window).await;
        let _ = stop_tx.send(());
    });
    engine
        .run(true, async move {
            let _ = stop_rx.await;
        })
        .await
        .expect("run loop error")
}

/// How many times the mock plugin has been launched, from its shared counter.
fn launches(counter: &Path) -> u64 {
    std::fs::read_to_string(counter)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(0)
}

fn notifications(log: &Path) -> Vec<serde_json::Value> {
    test_support::read_ndjson_log(log)
}

#[derive(Clone)]
struct CaptureBuf(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureBuf {
    type Writer = CaptureGuard;
    fn make_writer(&'a self) -> CaptureGuard {
        CaptureGuard(self.0.clone())
    }
}

struct CaptureGuard(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for CaptureGuard {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The process's one capture buffer. A `tracing` global default can be set
/// **once per process**, so this cannot be per-test.
static CAPTURED: std::sync::OnceLock<std::sync::Arc<std::sync::Mutex<Vec<u8>>>> =
    std::sync::OnceLock::new();

/// A test's view of the capture: everything written **after** it started.
struct Capture {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    from: usize,
}

impl Clone for Capture {
    fn clone(&self) -> Self {
        Self {
            buf: self.buf.clone(),
            from: self.from,
        }
    }
}

/// Capture this process's `tracing` output from here on.
///
/// **`set_global_default`, not `with_default`.** A thread-local default is
/// invisible to the engine, whose work runs on tokio worker threads — a test
/// using one would capture nothing and pass, which is the failure this exists
/// to prevent.
///
/// **But a global can only be installed once per process**, and the two
/// runners disagree about what a process is: `cargo nextest` (the local loop,
/// ADR-0049) gives each test its own, while CI's `cargo test --workspace`
/// shares one across a binary's tests. The first version installed a fresh
/// buffer per call and ignored the resulting error, so under CI the second
/// test to call it read an empty buffer and failed — green locally, red in CI.
///
/// So: install once, and hand each test an offset into the shared buffer.
fn capture_logs() -> Capture {
    use tracing_subscriber::layer::SubscriberExt;

    let buf = CAPTURED
        .get_or_init(|| {
            let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry().with(RedactingLayer::new(
                CaptureBuf(buf.clone()),
                LogFormat::Json,
                false,
                false,
            ));
            // Only this call site sets it, so a failure here means something
            // else in the binary did — worth failing loudly rather than
            // silently capturing nothing.
            tracing::subscriber::set_global_default(subscriber)
                .expect("no other global tracing subscriber may be installed in this binary");
            buf
        })
        .clone();
    let from = buf.lock().unwrap().len();
    Capture { buf, from }
}

/// What was logged since [`capture_logs`] was called.
fn captured(capture: &Capture) -> String {
    let buf = capture.buf.lock().unwrap();
    String::from_utf8_lossy(&buf[capture.from.min(buf.len())..]).into_owned()
}

/// Whether **one line** since [`capture_logs`] contains all of `needles`.
///
/// One line, not the whole slice: under `cargo test` sibling tests write into
/// the same buffer concurrently, so `text.contains(a) && text.contains(b)` can
/// be satisfied by two different tests' lines. Requiring co-occurrence on a
/// line, with at least one needle naming this test's own plugin, is what makes
/// the assertion about this test.
fn logged_line_with(capture: &Capture, needles: &[&str]) -> bool {
    captured(capture)
        .lines()
        .any(|line| needles.iter().all(|n| line.contains(n)))
}

/// **The regression this issue is about.** A `task_source` that dies used to
/// produce no engine event whatsoever — the run stayed up as a process that
/// would never receive another task. It must now be noticed and relaunched.
#[tokio::test]
async fn a_dead_task_source_is_noticed_and_comes_back() {
    let dir = test_support::scratch("supervise_source");
    let counter = dir.join("launches");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "task_source",
        "mock_src",
        json!({ "task_submit": true, "suicide": { "counter": counter, "times": 1 } }),
    )
    .await;

    let mut engine = Engine::new(
        db,
        settings(5),
        plugins,
        SystemGitRunner,
        None::<OpenAiRouter>,
    )
    .await;

    // Launch 1 dies right after `initialize`; launch 2 is the relaunch and
    // survives, so a stable count of 2 means the cycle completed.
    let probe = counter.clone();
    let summary = run_until(&mut engine, move || launches(&probe) >= 2).await;

    assert_eq!(
        launches(&counter),
        2,
        "the source should have been launched exactly twice: the original and one relaunch"
    );
    assert_eq!(
        summary.stats.plugin_restarts, 1,
        "the restart must be visible in the run summary, not only in the log"
    );
    assert_eq!(
        summary.stats.plugin_crashes, 1,
        "and so must the death it repaired — equal counts mean nothing is still down"
    );
    engine.shutdown(Duration::from_secs(2)).await;
}

/// A notifier is the kind with no streams at all, so nothing but the child's
/// own exit can reveal its death. Before #495 there was no path for it.
#[tokio::test]
async fn a_dead_notifier_is_noticed_and_comes_back() {
    let dir = test_support::scratch("supervise_notifier");
    let counter = dir.join("launches");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "notifier",
        "mock_notify",
        json!({ "suicide": { "counter": counter, "times": 1 } }),
    )
    .await;

    let mut engine = Engine::new(
        db,
        settings(5),
        plugins,
        SystemGitRunner,
        None::<OpenAiRouter>,
    )
    .await;
    let probe = counter.clone();
    let summary = run_until(&mut engine, move || launches(&probe) >= 2).await;

    assert_eq!(launches(&counter), 2);
    assert_eq!(summary.stats.plugin_restarts, 1);
    engine.shutdown(Duration::from_secs(2)).await;
}

/// **The half that matters more than the restart.** A plugin that will never
/// come back must stop being retried *and say so* — otherwise #495 has only
/// replaced one silent failure with another.
#[tokio::test]
async fn giving_up_escalates_instead_of_retrying_forever() {
    let logs = capture_logs();
    let dir = test_support::scratch("supervise_exhausted");
    let counter = dir.join("launches");
    let notify_log = dir.join("notify.ndjson");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    // `times` far above the budget: every relaunch dies the same way.
    install(
        &mut plugins,
        "task_source",
        "mock_src",
        json!({ "task_submit": true, "suicide": { "counter": counter, "times": 99 } }),
    )
    .await;
    install(
        &mut plugins,
        "notifier",
        "mock_notify",
        json!({ "notify_log": notify_log }),
    )
    .await;

    let mut engine = Engine::new(
        db,
        settings(2),
        plugins,
        SystemGitRunner,
        None::<OpenAiRouter>,
    )
    .await;

    let probe = notify_log.clone();
    run_until(&mut engine, move || {
        notifications(&probe)
            .iter()
            .any(|n| n["params"]["event"] == "escalated")
    })
    .await;

    let escalations: Vec<_> = notifications(&notify_log)
        .into_iter()
        .filter(|n| n["params"]["event"] == "escalated")
        .collect();
    assert!(
        !escalations.is_empty(),
        "exhausting the restart budget must reach the operator"
    );
    assert!(
        escalations[0]["params"]["body"]
            .as_str()
            .unwrap()
            .contains("staying down"),
        "the escalation must say the plugin is not coming back: {:?}",
        escalations[0]
    );

    // **Delivery alone is not enough of a guarantee to rest on**, so the log
    // line is asserted separately — and against the captured log, not the
    // notification, which are two different records.
    //
    // `notify` is fire-and-forget down a pipe: a dead plugin's stdin still
    // accepts a write while the writer task drains, so a lost notification
    // returns `Ok` and leaves no error. The sharpest case is a single
    // configured notifier being the plugin that died, where the escalation
    // *about* it is handed *to* it — nobody hears it. The log is what
    // survives that, which is exactly why it must be checked here rather
    // than trusted because it was seen once by hand.
    assert!(
        logged_line_with(&logs, &["escalating", "mock_src"]),
        "the escalation must reach the log naming the plugin, not only the \
         notifier: {}",
        captured(&logs)
    );
    let params = &escalations[0]["params"];
    assert!(
        params["title"].as_str().unwrap().contains("mock_src"),
        "the escalation must name the plugin: {params}"
    );
    // A plugin death belongs to no task, and pinning it to whichever task
    // happened to be running would misattribute it.
    assert!(params["task_id"].is_null(), "{params}");
    assert!(params["workflow"].is_null(), "{params}");

    // 1 original launch + `max_attempts` relaunches, and then it stops. The
    // upper bound is the assertion: an unbounded retry loop would keep
    // climbing while the run stayed up.
    let total = launches(&counter);
    assert_eq!(
        total, 3,
        "expected the original launch plus 2 relaunches, got {total}"
    );
    engine.shutdown(Duration::from_secs(2)).await;
}

/// `[plugins.{name}].restart = false` suppresses the relaunch **and keeps the
/// detection** — the shape someone debugging a plugin by hand needs.
///
/// The first version of this test only asserted that nothing was relaunched,
/// which its own name says is half the claim. It passed while the escalation
/// was missing entirely, because it installed no notifier to receive one.
/// **Assert the detection you promise**, not just the absence of the thing you
/// suppressed.
#[tokio::test]
async fn restart_can_be_disabled_without_losing_detection() {
    let dir = test_support::scratch("supervise_disabled");
    let counter = dir.join("launches");
    let notify_log = dir.join("notify.ndjson");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "task_source",
        "mock_src",
        json!({ "task_submit": true, "suicide": { "counter": counter, "times": 1 } }),
    )
    .await;
    install(
        &mut plugins,
        "notifier",
        "mock_notify",
        json!({ "notify_log": notify_log }),
    )
    .await;

    let mut settings = settings(5);
    settings.restart_disabled = ["mock_src".to_string()].into_iter().collect();
    let mut engine =
        Engine::new(db, settings, plugins, SystemGitRunner, None::<OpenAiRouter>).await;

    let probe = notify_log.clone();
    let summary = run_until(&mut engine, move || {
        notifications(&probe)
            .iter()
            .any(|n| n["params"]["event"] == "escalated")
    })
    .await;

    // Suppressed: the relaunch.
    assert_eq!(
        launches(&counter),
        1,
        "a disabled plugin must stay down after its single launch"
    );
    assert_eq!(summary.stats.plugin_restarts, 0);
    // Kept: everything else. The crash is counted whatever happens next, so
    // `plugin_restarts == 0` alone can never be read as "nothing died".
    assert_eq!(
        summary.stats.plugin_crashes, 1,
        "the death must be counted even though nothing was relaunched"
    );
    let escalations: Vec<_> = notifications(&notify_log)
        .into_iter()
        .filter(|n| n["params"]["event"] == "escalated")
        .collect();
    assert_eq!(
        escalations.len(),
        1,
        "a plugin left down on purpose is still news, and exactly once"
    );
    assert!(
        escalations[0]["params"]["title"]
            .as_str()
            .unwrap()
            .contains("mock_src")
    );
    engine.shutdown(Duration::from_secs(2)).await;
}

/// **The starvation this fix is really about.**
///
/// `plan_dispatch` fixes the plan for a whole cycle, so a slot released inside
/// `dispatch_one`'s park arm comes back too late to help anyone. With
/// `max_concurrency = 1`, a task parked on a crashed agent would take the only
/// global slot every 200 ms tick and a task for a **healthy** agent would never
/// dispatch for the entire outage — parking without moving the gate ahead of
/// slot acquisition trades one bug for a worse one.
///
/// This pins the gate in `dispatch_ready`: `dispatch_one`'s park arm alone
/// makes the next test pass, so without this one the move would be untested.
#[tokio::test]
async fn a_parked_task_does_not_starve_a_healthy_agent() {
    let dir = test_support::scratch("supervise_starvation");
    let repo = test_support::bare_origin_and_clone(&dir);
    let counter = dir.join("launches");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "task_source",
        "src_down",
        json!({ "submit_tasks": [{ "id": "for-the-dead-agent", "source": "src_down", "title": "a" }] }),
    )
    .await;
    install(
        &mut plugins,
        "task_source",
        "src_up",
        json!({ "submit_tasks": [{ "id": "for-the-live-agent", "source": "src_up", "title": "b" }] }),
    )
    .await;
    // Down for the whole run.
    install(
        &mut plugins,
        "agent_ide",
        "agent_down",
        json!({ "suicide": { "counter": counter, "times": 99 } }),
    )
    .await;
    install(&mut plugins, "agent_ide", "agent_up", json!({})).await;

    let cfg = RootConfig::from_toml_str(
        r#"
[[workflows]]
name = "wf-down"
source = "src_down"
trigger = {}
mode = "implement"
agent = "agent_down"
output = "none"

[[workflows]]
name = "wf-up"
source = "src_up"
trigger = {}
mode = "implement"
agent = "agent_up"
output = "none"
"#,
    )
    .unwrap();

    let mut settings = settings_with_backoff(5, Duration::from_secs(30));
    settings.workflows = Workflow::from_configs(&cfg.workflows);
    settings.repos = vec![RepoSettings {
        name: "clone".to_string(),
        path: repo.clone(),
        summary: None,
        worktree_location: None,
        tool: None,
    }];
    // **One** global slot: the parked task and the healthy one compete for it.
    settings.limits = Limits::global(1);
    settings.location_template = format!("{}/../wt/{{worktree_name}}", repo.display());

    let mut engine =
        Engine::new(db, settings, plugins, SystemGitRunner, None::<OpenAiRouter>).await;

    // Stop as soon as anything dispatches. Without the gate ahead of slot
    // acquisition nothing ever does, and the harness times out.
    let probe = dir.join("state.db");
    let summary = run_until(&mut engine, move || {
        StateDb::open(&probe).ok().is_some_and(|db| {
            // "left the queue", not "is Dispatched": the mock streams
            // `running` immediately, so `Dispatched` is a state the probe can
            // miss entirely between polls.
            db.tasks_in_state(TaskState::Queued)
                .map(|t| t.len() < 2)
                .unwrap_or(false)
        })
    })
    .await;

    assert!(
        summary.stats.dispatched >= 1,
        "a task for a healthy agent must dispatch while another is parked: {summary:?}"
    );
    engine.shutdown(Duration::from_secs(2)).await;
}

/// #499: a task that arrives while its agent is between instances **waits**
/// instead of burning its dispatch-retry budget.
///
/// The pure-function tests fix the rule; this one fixes that the rule is
/// wired to a real crash. The agent stays down for the whole run (a long
/// first backoff), so every dispatch attempt meets a dead plugin — which
/// before #499 meant `DISPATCH_RETRY_LIMIT` attempts one `SETTLE_TICK` apart
/// and a terminal `Failed` inside the first backoff second.
///
/// Nothing here needs a worktree: refusal happens at the gate, before any
/// side effect.
#[tokio::test]
async fn a_task_queued_during_a_crash_window_is_not_failed() {
    let logs = capture_logs();
    let dir = test_support::scratch("supervise_crash_window");
    let counter = dir.join("launches");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "task_source",
        "mock_src",
        // Held back so the task arrives **after** the agent has crashed and
        // the crash has been observed. Submitting immediately races the
        // detection, and the test would then pass or fail on timing.
        json!({
            "submit_delay_ms": 800,
            "submit_tasks": [{
                "id": "crash-window-1",
                "source": "mock_src",
                "title": "queued while the agent is down",
            }],
        }),
    )
    .await;
    // Dies on every launch, so the window never closes during this run.
    install(
        &mut plugins,
        "agent_ide",
        "mock_agent",
        json!({ "suicide": { "counter": counter, "times": 99 } }),
    )
    .await;

    // A backoff long enough that the agent is still down for every dispatch
    // attempt in the window below.
    let mut engine = Engine::new(
        db,
        settings_with_backoff(5, Duration::from_secs(30)),
        plugins,
        SystemGitRunner,
        None::<OpenAiRouter>,
    )
    .await;

    // Wait for the **park itself**, not for a wall-clock window. A fixed
    // window makes the result depend on CI load; worse, stopping merely
    // because the task was submitted would let `failed == 0` pass without the
    // dispatch ever having been attempted — a green test proving nothing.
    let probe = logs.clone();
    let summary = run_until(&mut engine, move || {
        // Keyed on **this test's** agent: another test parking its own agent
        // writes the same message into the shared buffer under `cargo test`,
        // and stopping on that would end this run before its task arrived.
        logged_line_with(&probe, &["leaving the task queued", "mock_agent"])
    })
    .await;

    assert_eq!(
        summary.stats.submitted, 1,
        "the task must have arrived at all: {summary:?}"
    );
    // Before #499 this was 1: three dispatch attempts against the dead handle,
    // one `SETTLE_TICK` apart, all inside the first backoff second.
    assert_eq!(
        summary.stats.failed, 0,
        "a task must not be failed while its agent is between instances: {summary:?}"
    );
    assert_eq!(
        summary.queued.len(),
        1,
        "it must still be queued, waiting for the agent: {summary:?}"
    );
}

/// #497: the run summary accounts for RPCs **per plugin**, so a slow or
/// failing one can be named without reading the log.
#[tokio::test]
async fn the_summary_accounts_for_rpcs_per_plugin() {
    let dir = test_support::scratch("observability_calls");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "notifier",
        "mock_notify",
        json!({ "notify_log": dir.join("notify.ndjson") }),
    )
    .await;

    let mut engine = Engine::new(
        db,
        settings(5),
        plugins,
        SystemGitRunner,
        None::<OpenAiRouter>,
    )
    .await;
    let summary = run_for(&mut engine, Duration::from_millis(200)).await;

    let report = summary
        .plugins
        .get("mock_notify")
        .expect("the launched plugin must appear in the summary");
    // `initialize` is the one call every plugin takes, so it is the one method
    // whose presence is guaranteed without staging traffic.
    let init = report
        .methods
        .get("initialize")
        .expect("initialize must be accounted for");
    assert_eq!(init.calls, 1);
    assert_eq!(init.outcomes.get("ok").copied(), Some(1));
    // A latency is recorded even when it rounds to zero milliseconds — the
    // field being present is what says "this was measured".
    assert!(init.p50_ms.is_some(), "p50 must be reported: {init:?}");
    assert!(init.p95_ms.is_some());
    engine.shutdown(Duration::from_secs(2)).await;
}

/// **The interaction that makes this worth having.** A restart (#495) builds a
/// whole new `Plugin`, so its counters start at zero — the plugin that crashed
/// most would otherwise report the fewest calls.
#[tokio::test]
async fn a_restart_does_not_reset_the_accounting() {
    let dir = test_support::scratch("observability_restart");
    let counter = dir.join("launches");
    let db = StateDb::open(&dir.join("state.db")).unwrap();

    let mut plugins = PluginSet::default();
    install(
        &mut plugins,
        "task_source",
        "mock_src",
        json!({ "suicide": { "counter": counter, "times": 1 } }),
    )
    .await;

    let mut engine = Engine::new(
        db,
        settings(5),
        plugins,
        SystemGitRunner,
        None::<OpenAiRouter>,
    )
    .await;
    let probe = counter.clone();
    let summary = run_until(&mut engine, move || launches(&probe) >= 2).await;

    let report = summary.plugins.get("mock_src").expect("plugin in summary");
    let init = report.methods.get("initialize").expect("initialize");
    // Two launches means two `initialize` calls. Counting 1 would mean the
    // dead instance's history was dropped along with the instance.
    assert_eq!(
        init.calls, 2,
        "the pre-crash instance's calls must survive the restart: {report:?}"
    );
    assert_eq!(report.crashes, 1, "and the crash is attributed by name");
    assert_eq!(report.restarts, 1);
    engine.shutdown(Duration::from_secs(2)).await;
}
