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
protocol_version = ">=0.1.6, <0.6"
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
        triggers: vec![],
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
            first_backoff: Duration::ZERO,
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
    // Delivery alone is not enough of a guarantee to rest on. `notify` is
    // fire-and-forget down a pipe, so a dead notifier still returns `Ok` —
    // and the sharpest case is a single configured notifier being the plugin
    // that died, where the escalation about it is handed to it. The log line
    // is the record that survives that, so it is asserted here rather than
    // left as an incidental.
    assert!(
        escalations[0]["params"]["body"]
            .as_str()
            .unwrap()
            .contains("staying down"),
        "the escalation must say the plugin is not coming back: {:?}",
        escalations[0]
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
