//! `totsuka run` — the main loop (#63, §5.1).
//!
//! Assembles the [`Engine`](orchestrator_core::run::Engine) from the system
//! environment: config load + validation, logging, the single-instance lock
//! (F-74), plugin launch (enabled entries only, F-58, with secrets resolved
//! F-65), startup recovery (§5.3), then one-shot / `--watch` / `--dry-run`.

use std::collections::HashMap;
use std::time::Duration;

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::{OpenAiConfig, OpenAiRouter};
use orchestrator_core::adapters::plugin_host::Plugin;
use orchestrator_core::adapters::{RunLock, StateDb};
use orchestrator_core::config::{self, PluginKind, RootConfig};
use orchestrator_core::logging::{self, LogConfig};
use orchestrator_core::platform::PlatformProcessProbe;
use orchestrator_core::ports::SecretString;
use orchestrator_core::run::{Engine, HookRuntime, PluginSet, RunSummary, settings_from_config};

use crate::common::{CliError, Cx, plugin_spec, secret_resolver};

/// Grace period for plugin shutdown at the end of a run.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Execute `totsuka run`.
pub fn run(cx: &Cx, watch: bool, dry_run: bool, debug: bool) -> Result<(), CliError> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_async(cx, watch, dry_run, debug))
}

async fn run_async(cx: &Cx, watch: bool, dry_run: bool, debug: bool) -> Result<(), CliError> {
    let paths = &cx.paths;
    let env: HashMap<String, String> = std::env::vars().collect();
    let env_fn = |k: &str| env.get(k).cloned();

    // Config load + full validation (static + workflow semantics).
    let cfg = cx.load_config()?;
    let store = cx.store();
    // Hook capability is not yet declared in plugin manifests (protocol
    // 0.1.3, #132); `None` = unknown skips the `[hooks].auth_token_ref`
    // advisory until manifests can declare it.
    let findings = config::validate(
        &cfg,
        &env_fn,
        |name| {
            store
                .manifest_of(name)
                .ok()
                .flatten()
                .map(|m| m.capabilities.outputs)
        },
        |_| None,
    );
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
        // --debug wins over the configured level (§7).
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
    crate::hooks::install(paths, &cfg)?;

    let db = StateDb::open(&paths.state_dir().join("state.db"))?;
    let plugins = launch_plugins(cx, &cfg, &env).await?;

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

    let mut settings = settings_from_config(&cfg, &env)?;
    settings.readme_cache_dir = Some(paths.cache_dir().to_path_buf());

    // Hook runtime (#131/#138): the UDS receiver endpoint + Bearer token, the
    // spool directory, and the per-workflow rendered `--settings` paths that
    // dispatch injects into hook-capable agents. Read-only dry runs skip it
    // (no dispatch, no receiver). It starts even when `[hooks]` is unset — a
    // config with no hook-capable agent simply never receives a POST.
    if !dry_run {
        let socket_path = match &cfg.hooks.socket_path {
            Some(p) => config::expand_path(p, &env_fn)?,
            None => paths.runtime_dir().join("claude-events.sock"),
        };
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
                    crate::hooks::settings_path(paths, &wf.name),
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
    print_summary(&summary);
    Ok(())
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
        let spec = plugin_spec(cx, cfg, name, env)?;
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
fn print_summary(summary: &RunSummary) {
    if summary.interrupted {
        println!("interrupted — in-flight tasks stay in the state DB and resume on next run");
    }
    let s = &summary.stats;
    println!(
        "run summary: submitted {} / dispatched {} / done {} / failed {}",
        s.submitted, s.dispatched, s.done, s.failed
    );
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
}
