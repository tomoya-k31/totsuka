//! `totsuka run` — the main loop (#63, §5.1).
//!
//! Assembles the [`Engine`] from the system
//! environment: config load + validation, logging, the single-instance lock
//! (F-74), plugin launch (enabled entries only, F-58, with secrets resolved
//! F-65), startup recovery (§5.3), then one-shot / `--watch` / `--dry-run`.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use orchestrator_core::adapters::git::{DEFAULT_GIT_TIMEOUT, SystemGitRunner};
use orchestrator_core::adapters::llm::gateway_classifier;
use orchestrator_core::adapters::plugin_host::Plugin;
use orchestrator_core::adapters::{HostError, LockError, RunLock, StateDb};
use orchestrator_core::config::{self, PluginKind, RootConfig, secret_resolver};
use orchestrator_core::logging::{self, LogConfig};
use orchestrator_core::platform::PlatformProcessProbe;
use orchestrator_core::plugins::{check_workflow_options, plugin_spec};
use orchestrator_core::ports::SecretString;
use orchestrator_core::run::{Engine, HookRuntime, PluginSet, RunSummary, settings_from_config};

use crate::common::{CliError, Cx, EXIT_ALREADY_RUNNING, EXIT_CONFIG, ExitWith, print_json};

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
    /// Take secret values from stdin's first line, never from a store (#754).
    pub secrets_stdin: bool,
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
        secrets_stdin,
        json,
    } = args;
    let paths = &cx.paths;
    let env: HashMap<String, String> = std::env::vars().collect();
    let env_fn = |k: &str| env.get(k).cloned();

    // Before anything else (#754): a malformed line stops the run before a
    // single file is read. A dry run reads it too — it still launches the
    // plugins, and an unread pipe would block or EPIPE the launcher's write.
    if secrets_stdin {
        crate::common::install_supplied_secrets().map_err(needs_fix)?;
    }

    // Config load (incl. `TOTSUKA_*` overrides, F-66 layer 2) + full
    // validation (static + workflow semantics).
    let cfg = cx.load_config(&env).map_err(needs_fix)?;
    let findings = cx.validate_config(&cfg, &env);
    if config::has_errors(&findings) {
        for finding in &findings {
            eprintln!("config error: {}", finding.message);
        }
        return Err(needs_fix("configuration is invalid → fix the errors above"));
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
        Some(
            RunLock::acquire(
                &paths.state_dir().join("run.lock"),
                &PlatformProcessProbe::default(),
            )
            .map_err(|e| match e {
                LockError::AlreadyRunning { .. } => {
                    ExitWith::new(EXIT_ALREADY_RUNNING, e.to_string()).into()
                }
                LockError::Io(_) => CliError::from(e),
            })?,
        )
    };

    // Stop requests (#753), installed right after the lock rather than where
    // the loop awaits them: once installed, a signal that arrives during the
    // startup below (plugin launch, recovery) is buffered and ends the run
    // gracefully as soon as it starts, instead of killing the process with
    // `health.json` and the lock left behind. Dry runs stop at startup and
    // keep the default actions.
    let stop = (!dry_run).then(stop_requested).transpose()?;

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

    // `[tools.<name>].env_file` (#744): resolved once, at startup — the same
    // secret-store approval as every other reference covers it, and no agent
    // launch touches the store again. Before the plugins launch, so a file that
    // cannot be used stops the run before anything has been started (Copilot
    // review, #746). A dry run launches no agent and resolves nothing.
    let tool_env = if dry_run {
        Default::default()
    } else {
        config::env_file::resolve_tool_env(&cfg.tools, &env_fn, &secret_resolver(&env))
            .map_err(needs_fix)?
    };

    let db = StateDb::open(&paths.state_dir().join("state.db"))?;
    let plugins = launch_plugins(cx, &cfg, &env).await?;

    // Repository classifier (F-12), if configured.
    let llm = match &cfg.llm {
        Some(llm_cfg) => {
            let api_key = match &llm_cfg.api_key_ref {
                Some(reference) => secret_resolver(&env)
                    .resolve(reference)
                    .map_err(needs_fix)?,
                None => SecretString::new(""),
            };
            Some(gateway_classifier(llm_cfg, api_key))
        }
        None => None,
    };

    let mut settings = settings_from_config(&cfg, &env, paths).map_err(needs_fix)?;
    settings.readme_cache_dir = Some(paths.cache_dir().to_path_buf());
    settings.tool_env = tool_env;
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
            Some(p) => config::expand_path(p, &env_fn).map_err(needs_fix)?,
            None => paths.runtime_dir().join("agent-events.sock"),
        };
        // The default socket was `claude-events.sock` before the #196 rename;
        // a stale one left by an older orchestrator would linger forever.
        let legacy_socket = paths.runtime_dir().join("claude-events.sock");
        if legacy_socket != socket_path {
            let _ = std::fs::remove_file(&legacy_socket);
        }
        // Generated on first start and reused after, so a restart does not
        // 401 the hooks of agents that outlived the previous run (#785).
        let token_path = orchestrator_core::hooks::token::path(paths);
        let auth_token = Some(
            orchestrator_core::hooks::token::load_or_create(&token_path)
                .map_err(|e| format!("hook token {}: {e}", token_path.display()))?,
        );
        let spool_dir = Some(match &cfg.hooks.spool_dir {
            Some(p) => config::expand_path(p, &env_fn).map_err(needs_fix)?,
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
        // Runtime health (F-110), next to `run.lock` — the same category of
        // fact, and for the same reason it is a file rather than a row. Set
        // inside the same `!dry_run` guard as the hook runtime: a dry run
        // dispatches nothing and has no health to report.
        settings.health_path = Some(orchestrator_core::adapters::run_health::path_in(
            paths.state_dir(),
        ));
    }

    let git = SystemGitRunner::with_timeout(
        cfg.worktree
            .git_timeout_secs
            .map_or(DEFAULT_GIT_TIMEOUT, Duration::from_secs),
    );
    let mut engine = Engine::new(db, settings, plugins, git, llm).await;

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
        .run(watch, stop.expect("installed for every non-dry run"))
        .await?;
    engine.shutdown(SHUTDOWN_GRACE).await;
    print_summary(&summary, json)?;
    Ok(())
}

/// Resolves on the first stop request: SIGINT, SIGTERM or SIGHUP (#753).
///
/// All three mean the same graceful stop. SIGTERM is what launchd /
/// `brew services` / `kill` send, and SIGHUP is a closed terminal; `ctrl_c()`
/// alone left both on their default action, which kills the process before
/// `engine.shutdown` runs. SIGHUP is not "reload" — there is nothing to reload.
///
/// The listeners are registered here, not when the future is first polled, so
/// a signal that arrives before then is kept (tokio buffers it).
#[cfg(unix)]
fn stop_requested() -> std::io::Result<impl std::future::Future<Output = ()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    Ok(async move {
        let name = tokio::select! {
            _ = interrupt.recv() => "SIGINT",
            _ = terminate.recv() => "SIGTERM",
            _ = hangup.recv() => "SIGHUP",
        };
        tracing::info!(signal = name, "stop requested; shutting down gracefully");
    })
}

#[cfg(not(unix))]
fn stop_requested() -> std::io::Result<impl std::future::Future<Output = ()>> {
    Ok(async {
        let _ = tokio::signal::ctrl_c().await;
    })
}

/// Launch every enabled plugin from the store (F-58), passing its
/// secret-resolved `[<name>]` table as the `initialize` config (F-65, #554).
async fn launch_plugins(
    cx: &Cx,
    cfg: &RootConfig,
    env: &HashMap<String, String>,
) -> Result<PluginSet, CliError> {
    let mut set = PluginSet::default();
    let mut claims: BTreeMap<String, Vec<plugin_protocol::methods::WorkflowOption>> =
        BTreeMap::new();
    for (name, plugin_cfg) in cfg.plugins.iter().filter(|(_, p)| p.enabled) {
        let spec = plugin_spec(&cx.store(), cfg, name, env).map_err(needs_fix)?;
        // Keep the spec: it is everything a relaunch needs (#495), and
        // re-deriving it later would re-resolve the plugin's secrets — a
        // Keychain/1Password round trip per crash, on the engine loop.
        set.specs.insert(name.clone(), spec.clone());
        let plugin = Plugin::launch(spec).await.map_err(launch_error)?;
        claims.insert(name.clone(), plugin.claimed_options().to_vec());
        match plugin_cfg.kind {
            PluginKind::TaskSource => set.sources.insert(name.clone(), plugin),
            PluginKind::AgentIde => set.agents.insert(name.clone(), plugin),
            PluginKind::Notifier => set.notifiers.insert(name.clone(), plugin),
        };
    }
    // Every plugin answered (a failed launch returned above), so a workflow
    // key nobody claims is a real one — refuse to run rather than carry a
    // setting that does nothing (#554). This is the check `config validate`
    // does too; it lives here as well because `run` never calls that, and a
    // typo that only `config validate` catches is a typo nothing catches for
    // an operator who does not run it.
    let issues = check_workflow_options(cfg, &claims);
    if !issues.is_empty() {
        let listed = issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n  ");
        return Err(needs_fix(format!(
            "config.toml has workflow keys no plugin owns:\n  {listed}"
        )));
    }
    Ok(set)
}

/// A startup failure a restart cannot fix: config, a secret reference, a
/// plugin's install (#755). Exits [`EXIT_CONFIG`] so a supervisor stops
/// instead of looping. Applied per call site, not by error type in `main`:
/// the same types (`SecretError`, …) also occur after startup, where a
/// restart may well help.
fn needs_fix(err: impl std::fmt::Display) -> CliError {
    ExitWith::new(EXIT_CONFIG, err.to_string()).into()
}

/// Classify a failed plugin launch (#755). An incompatible protocol, a binary
/// that is missing or not executable, and a plugin rejecting its own config
/// (`CONFIG_INVALID`) all need a person; anything else (a crash or timeout
/// during `initialize`, other spawn IO) stays the generic exit 1.
fn launch_error(err: HostError) -> CliError {
    let fixable = match &err {
        HostError::ProtocolMismatch { .. } => true,
        HostError::Spawn { source, .. } => matches!(
            source.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ),
        HostError::Rpc { code, .. } => *code == plugin_protocol::error_code::CONFIG_INVALID,
        _ => false,
    };
    if fixable { needs_fix(err) } else { err.into() }
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
        "run summary: submitted {} / dispatched {} / done {} / failed {} / skipped {}",
        s.submitted, s.dispatched, s.done, s.failed, s.skipped
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
