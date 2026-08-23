//! `totsuka run` — the main loop (#63, §5.1).
//!
//! Assembles the [`Engine`] from the system
//! environment: config load + validation, logging, the single-instance lock
//! (F-74), plugin launch (enabled entries only, F-58, with secrets resolved
//! F-65), startup recovery (§5.3), then one-shot / `--watch` / `--dry-run`.

use std::collections::HashMap;
use std::time::Duration;

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::{OpenAiConfig, OpenAiRouter};
use orchestrator_core::adapters::plugin_host::Plugin;
use orchestrator_core::adapters::{RunLock, StateDb};
use orchestrator_core::config::{self, PluginKind, RootConfig, secret_resolver};
use orchestrator_core::logging::{self, LogConfig};
use orchestrator_core::platform::PlatformProcessProbe;
use orchestrator_core::plugins::claims::ClaimRegistry;
use orchestrator_core::plugins::plugin_spec;
use orchestrator_core::ports::SecretString;
use orchestrator_core::run::{Engine, HookRuntime, PluginSet, RunSummary, settings_from_config};

use crate::common::{CliError, Cx, print_json};

/// Grace period for plugin shutdown at the end of a run.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// What `totsuka run` was asked to do.
#[derive(Debug, Clone, Copy)]
pub struct RunArgs {
    /// Keep polling instead of exiting after one cycle (F-06).
    pub watch: bool,
    /// Report what would happen without executing (mutually exclusive with
    /// [`json`](Self::json), see the flag's docs in `main`).
    pub dry_run: bool,
    /// Global `--debug`: raises this run's file log level.
    pub debug: bool,
    /// One-shot's quiet-period floor override (test affordance).
    pub one_shot_grace_ms: Option<u64>,
    /// Emit the summary as JSON on stdout instead of prose (#462).
    pub json: bool,
}

/// Execute `totsuka run`.
pub fn run(cx: &Cx, args: RunArgs) -> Result<(), CliError> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_async(cx, args))
}

async fn run_async(cx: &Cx, args: RunArgs) -> Result<(), CliError> {
    let RunArgs {
        watch,
        dry_run,
        debug,
        one_shot_grace_ms,
        json,
    } = args;
    let paths = &cx.paths;
    let env: HashMap<String, String> = std::env::vars().collect();
    let env_fn = |k: &str| env.get(k).cloned();

    // Config load (incl. `TOTSUKA_*` overrides, F-66 layer 2) + full
    // validation (static + workflow semantics).
    let cfg = cx.load_config(&env)?;
    let findings = cx.validate_config(&cfg, &env);
    if config::has_errors(&findings) {
        for finding in &findings {
            eprintln!("config error: {}", finding.message);
        }
        return Err("configuration is invalid → fix the errors above".into());
    }
    for finding in &findings {
        eprintln!("config warning: {}", finding.message);
    }

    // Logging (§5.2).
    let mut log_config = LogConfig::new(logging::default_log_dir(paths.state_dir()));
    if let Some(level) = cfg.log.level.as_deref().and_then(logging::parse_level) {
        log_config.level = level;
    }
    log_config.log_prompts = cfg.log.log_prompts;
    if let Some(max_files) = cfg.log.max_files {
        log_config.max_files = max_files;
    }
    if debug {
        // --debug wins over the configured level (§7). Applied after
        // `load_config`, so it also wins over `TOTSUKA_LOG_LEVEL` — that
        // ordering *is* the "CLI > env" guarantee of F-66.
        log_config.level = logging::parse_level("debug").expect("debug is a valid level");
    }
    let _log_guard = logging::init(&log_config)?;

    // Single-instance lock (F-74). Dry runs are read-only and skip it.
    let _lock = if dry_run {
        None
    } else {
        Some(RunLock::acquire(
            &paths.state_dir().join("run.lock"),
            &PlatformProcessProbe::default(),
        )?)
    };

    // Refresh the static hook scripts + per-workflow settings under
    // $XDG_DATA_HOME/totsuka/hooks/ (H-01/H-03, #137). Idempotent by content
    // hash, so a matching second startup rewrites nothing.
    orchestrator_core::hooks::install(paths, &cfg)?;

    // Codex hook registration (#196 Phase 2): keep the totsuka entries in
    // $CODEX_HOME/hooks.json in sync. Internally a no-op unless the config
    // references a codex-kind tool, so claude-only setups never touch it.
    let codex_home = orchestrator_core::hooks::codex::codex_home(env_fn);
    orchestrator_core::hooks::codex::sync_registration(codex_home.as_deref(), paths, &cfg)?;

    // OpenCode assets (#196 Phase 3): the completion-detection JS plugin and
    // the totsuka-plan agent under $XDG_CONFIG_HOME/opencode/. Same gating —
    // untouched unless an opencode-kind tool is referenced.
    let opencode_dir = orchestrator_core::hooks::opencode::opencode_config_dir(env_fn);
    orchestrator_core::hooks::opencode::sync_assets(opencode_dir.as_deref(), &cfg)?;

    let db = StateDb::open(&paths.state_dir().join("state.db"))?;
    let plugins = launch_plugins(cx, &cfg, &env).await?;
    warn_on_claim_conflicts(&plugins);

    // AI Gateway router (F-12), if configured.
    let llm = match &cfg.llm {
        Some(llm_cfg) => {
            let mut openai = OpenAiConfig::new(&llm_cfg.base_url, &llm_cfg.model);
            if let Some(secs) = llm_cfg.timeout_secs {
                openai.timeout = Duration::from_secs(secs);
            }
            let api_key = match &llm_cfg.api_key_ref {
                Some(reference) => secret_resolver(&env).resolve(reference)?,
                None => SecretString::new(""),
            };
            Some(OpenAiRouter::new(openai, api_key))
        }
        None => None,
    };

    let mut settings = settings_from_config(&cfg, &env, paths)?;
    settings.readme_cache_dir = Some(paths.cache_dir().to_path_buf());
    // CLI flags are layer 1 of the precedence stack (see config/env_overrides),
    // so a value that is neither config nor environment belongs here (#281).
    if let Some(ms) = one_shot_grace_ms {
        settings.one_shot_grace = Duration::from_millis(ms);
    }

    // Hook runtime (#131/#138): the UDS receiver endpoint + Bearer token, the
    // spool directory, and the per-workflow rendered `--settings` paths that
    // dispatch injects into hook-capable agents. Read-only dry runs skip it
    // (no dispatch, no receiver). It starts even when `[hooks]` is unset — a
    // config with no hook-capable agent simply never receives a POST.
    if !dry_run {
        let socket_path = match &cfg.hooks.socket_path {
            Some(p) => config::expand_path(p, &env_fn)?,
            None => paths.runtime_dir().join("agent-events.sock"),
        };
        // The default socket was `claude-events.sock` before the #196 rename;
        // a stale one left by an older orchestrator would linger forever.
        let legacy_socket = paths.runtime_dir().join("claude-events.sock");
        if legacy_socket != socket_path {
            let _ = std::fs::remove_file(&legacy_socket);
        }
        let auth_token = match &cfg.hooks.auth_token_ref {
            Some(reference) => Some(secret_resolver(&env).resolve(reference)?),
            None => {
                eprintln!(
                    "hook auth token not configured ([hooks].auth_token_ref) → hook POSTs are accepted without a Bearer token (0600 socket only)"
                );
                None
            }
        };
        let spool_dir = Some(match &cfg.hooks.spool_dir {
            Some(p) => config::expand_path(p, &env_fn)?,
            None => paths.state_dir().join("hooks").join("spool"),
        });
        let settings_paths = cfg
            .workflows
            .iter()
            .map(|wf| {
                (
                    wf.name.clone(),
                    orchestrator_core::hooks::settings_path(paths, &wf.name),
                )
            })
            .collect();
        settings.hook = Some(HookRuntime {
            socket_path,
            auth_token,
            spool_dir,
            settings_paths,
            block_retry_limit: cfg
                .hooks
                .block_retry_limit
                .unwrap_or(config::DEFAULT_BLOCK_RETRY_LIMIT),
        });
    }

    let mut engine = Engine::new(db, settings, plugins, SystemGitRunner, llm).await;

    if dry_run {
        // Every task_source is push-only since protocol 0.2.0, so there is
        // nothing to fetch ahead of time — `dry_run` always reports no
        // preview available.
        engine.dry_run().await?;
        println!(
            "dry-run: push sources (task/submit) cannot be previewed — nothing is fetched \
             ahead of time. Run without --dry-run to see live ingestion."
        );
        engine.shutdown(SHUTDOWN_GRACE).await;
        return Ok(());
    }

    // Startup recovery (§5.3) + orphan worktree warning (F-24).
    let report = engine.recover().await?;
    for outcome in report.needs_confirmation() {
        eprintln!(
            "task {} could not be resumed → `totsuka task retry {}` or `totsuka task cancel {}`",
            outcome.task_id, outcome.task_id, outcome.task_id
        );
    }
    engine.warn_orphan_worktrees()?;

    let summary = engine
        .run(watch, async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    engine.shutdown(SHUTDOWN_GRACE).await;
    print_summary(&summary, json)?;
    Ok(())
}

/// Warn about repositories claimed as a tracker target by more than one source
/// (#542).
///
/// A warning, not a refusal. The run is still useful — every other repository
/// routes correctly, and the contested one routes to one of the two places the
/// operator actually configured. Refusing to start would take a whole session
/// away over a config line that only affects `triage` tasks for that one
/// repository.
///
/// **No plugin can see this on its own.** Each one's `config/validate` checks
/// only its own list, so the github and notion configs are both individually
/// valid; the conflict exists only in the union.
fn warn_on_claim_conflicts(plugins: &PluginSet) {
    let mut names: Vec<&String> = plugins.sources.keys().collect();
    names.sort();
    let registry = ClaimRegistry::from_sources(
        names
            .into_iter()
            .map(|name| (name.as_str(), plugins.sources[name].claimed_repos())),
    );
    for conflict in registry.conflicts() {
        tracing::warn!("{conflict}");
    }
}

/// Launch every enabled plugin from the store (F-58), passing its
/// secret-resolved `plugins/{name}.toml` as the `initialize` config (F-64/65).
async fn launch_plugins(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
) -> Result<PluginSet, CliError> {
    let mut set = PluginSet::default();
    for (name, plugin_cfg) in cfg.plugins.iter().filter(|(_, p)| p.enabled) {
        let spec = plugin_spec(&cx.store(), &cx.plugin_config_dir(), cfg, name, env)?;
        // Keep the spec: it is everything a relaunch needs (#495), and
        // re-deriving it later would re-resolve the plugin's secrets — a
        // Keychain/1Password round trip per crash, on the engine loop.
        set.specs.insert(name.clone(), spec.clone());
        let plugin = Plugin::launch(spec).await?;
        match plugin_cfg.kind {
            PluginKind::TaskSource => set.sources.insert(name.clone(), plugin),
            PluginKind::AgentIde => set.agents.insert(name.clone(), plugin),
            PluginKind::Notifier => set.notifiers.insert(name.clone(), plugin),
        };
    }
    Ok(set)
}

/// Print the one-shot / watch exit summary (§5.1).
///
/// `json` emits the [`RunSummary`] as one document on stdout and nothing else
/// (#462), so a caller can act on the run instead of grepping prose:
/// `totsuka run --json | jq -e '.stats.failed == 0'`. The prose path is
/// unchanged.
///
/// **`run`'s exit code is deliberately not derived from the summary.** A run
/// that correctly recorded a failing task did its job, so `failed > 0` still
/// exits 0; `--json` is what lets the caller decide otherwise.
fn print_summary(summary: &RunSummary, json: bool) -> Result<(), CliError> {
    if json {
        return print_json(summary);
    }
    if summary.interrupted {
        println!("interrupted — in-flight tasks stay in the state DB and resume on next run");
    }
    let s = &summary.stats;
    println!(
        "run summary: submitted {} / dispatched {} / done {} / failed {}",
        s.submitted, s.dispatched, s.done, s.failed
    );
    if s.plugin_restarts > 0 {
        // The restart itself is deliberately quiet, so this line is the only
        // place a flapping plugin becomes visible without reading the log.
        println!(
            "plugin restarts: {} (a plugin crashed and was relaunched — check the log)",
            s.plugin_restarts
        );
    }
    // Only the plugins worth mentioning (#497): a healthy run would otherwise
    // print a table of zeroes every time, which trains people to skip it.
    // `--json` always carries the full accounting.
    for (name, report) in &summary.plugins {
        let failed: usize = report
            .methods
            .values()
            .map(|m| m.calls - m.outcomes.get("ok").copied().unwrap_or(0))
            .sum();
        if failed == 0 && report.crashes == 0 {
            continue;
        }
        let calls: usize = report.methods.values().map(|m| m.calls).sum();
        println!(
            "plugin {name}: {failed}/{calls} call(s) failed, {} crash(es), {} restart(s) → `totsuka run --json` for the per-method breakdown",
            report.crashes, report.restarts
        );
    }
    let list = |ids: &[i64]| {
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !summary.waiting.is_empty() {
        println!(
            "waiting for input: task {} → answer in the agent, then re-run",
            list(&summary.waiting)
        );
    }
    if !summary.pending.is_empty() {
        println!(
            "pending repo confirmation: task {} → confirm, then re-run",
            list(&summary.pending)
        );
    }
    if !summary.queued.is_empty() {
        println!(
            "still queued: task {} (see warnings above)",
            list(&summary.queued)
        );
    }
    Ok(())
}
