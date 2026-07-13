//! `totsuka run` — the main loop (#63, §5.1).
//!
//! Assembles the [`Engine`](orchestrator_core::run::Engine) from the system
//! environment: config load + validation, logging, the single-instance lock
//! (F-74), plugin launch (enabled entries only, F-58, with secrets resolved
//! F-65), startup recovery (§5.3), then one-shot / `--watch` / `--dry-run`.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::llm::{OpenAiConfig, OpenAiRouter};
use orchestrator_core::adapters::plugin_host::{Plugin, PluginSpec};
use orchestrator_core::adapters::{RunLock, StateDb};
use orchestrator_core::config::{self, PluginKind, PluginRawConfig, RootConfig, SecretResolver};
use orchestrator_core::logging::{self, LogConfig};
use orchestrator_core::paths::Paths;
use orchestrator_core::platform::{PlatformProcessProbe, PlatformSecretStore};
use orchestrator_core::plugins::PluginStore;
use orchestrator_core::ports::SecretString;
use orchestrator_core::run::{Engine, PluginSet, RunSummary, settings_from_config};
use serde_json::Value;

/// A boxed error for CLI operations.
type CliError = Box<dyn std::error::Error>;

/// Default per-call plugin RPC timeout when `timeout_secs` is omitted.
const DEFAULT_PLUGIN_TIMEOUT: Duration = Duration::from_secs(120);

/// Grace period for plugin shutdown at the end of a run.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Execute `totsuka run`.
pub fn run(watch: bool, dry_run: bool) -> Result<(), CliError> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(run_async(watch, dry_run))
}

async fn run_async(watch: bool, dry_run: bool) -> Result<(), CliError> {
    let paths = Paths::from_system()?;
    let env: HashMap<String, String> = std::env::vars().collect();
    let env_fn = |k: &str| env.get(k).cloned();

    // Config load + full validation (static + workflow semantics).
    let config_path = paths.config_dir().join("config.toml");
    let cfg = match std::fs::read_to_string(&config_path) {
        Ok(s) => RootConfig::from_toml_str(&s)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(format!(
                "config not found at {} → create it (see docs) before running",
                config_path.display()
            )
            .into());
        }
        Err(e) => return Err(e.into()),
    };
    let store = PluginStore::new(paths.data_dir().join("plugins"));
    let findings = config::validate(&cfg, &env_fn, |name| {
        store
            .manifest_of(name)
            .ok()
            .flatten()
            .map(|m| m.capabilities.outputs)
    });
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

    let db = StateDb::open(&paths.state_dir().join("state.db"))?;
    let plugins = launch_plugins(&cfg, &store, paths.config_dir(), &env).await?;

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

    let mut engine = Engine::new(db, settings, plugins, SystemGitRunner, llm).await;

    if dry_run {
        let entries = engine.dry_run().await?;
        if entries.is_empty() {
            println!("dry-run: no tasks match any workflow trigger.");
        } else {
            for e in &entries {
                let ingested = e
                    .already_ingested
                    .as_deref()
                    .map(|s| format!(" [already ingested: {s}]"))
                    .unwrap_or_default();
                println!(
                    "{}#{} {} → workflow `{}` (mode {}) → repo {} → agent `{}`{}",
                    e.source, e.task_id, e.title, e.workflow, e.mode, e.repo, e.agent, ingested
                );
            }
        }
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
    cfg: &RootConfig,
    store: &PluginStore,
    config_dir: &std::path::Path,
    env: &HashMap<String, String>,
) -> Result<PluginSet, CliError> {
    let mut set = PluginSet::default();
    for (name, plugin_cfg) in cfg.plugins.iter().filter(|(_, p)| p.enabled) {
        let manifest = store.manifest_of(name)?.ok_or_else(|| {
            format!("plugin `{name}` is enabled but not installed → `totsuka plugin install <dir>`")
        })?;
        let init_config = plugin_init_config(config_dir, name, env)?;
        let spec = PluginSpec {
            name: name.clone(),
            program: store.plugin_dir(name).join(&manifest.name),
            args: vec![],
            manifest,
            init_config,
            timeout: plugin_cfg
                .timeout_secs
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_PLUGIN_TIMEOUT),
        };
        let plugin = Plugin::launch(spec).await?;
        match plugin_cfg.kind {
            PluginKind::TaskSource => set.sources.insert(name.clone(), plugin),
            PluginKind::AgentIde => set.agents.insert(name.clone(), plugin),
            PluginKind::Notifier => set.notifiers.insert(name.clone(), plugin),
        };
    }
    Ok(set)
}

/// Load `plugins/{name}.toml` (empty object if absent) and resolve secret
/// references in its string values (F-65).
fn plugin_init_config(
    config_dir: &std::path::Path,
    name: &str,
    env: &HashMap<String, String>,
) -> Result<Value, CliError> {
    let path = config_dir.join("plugins").join(format!("{name}.toml"));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => PluginRawConfig::from_toml_str(&s)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => PluginRawConfig::from_toml_str("")?,
        Err(e) => return Err(e.into()),
    };
    let mut value = raw.to_json()?;
    let resolver = secret_resolver(env);
    resolve_strings(&mut value, &resolver).map_err(|e| format!("in {}: {e}", path.display()))?;
    Ok(value)
}

/// The platform secret resolver over a snapshot of the environment.
fn secret_resolver(
    env: &HashMap<String, String>,
) -> SecretResolver<PlatformSecretStore, impl Fn(&str) -> Option<String> + '_> {
    SecretResolver::new(PlatformSecretStore::default(), |k: &str| {
        env.get(k).cloned()
    })
}

/// Recursively resolve `${ENV}` / `keychain:` references in every string leaf.
fn resolve_strings<E>(
    value: &mut Value,
    resolver: &SecretResolver<PlatformSecretStore, E>,
) -> Result<(), config::ResolveError>
where
    E: Fn(&str) -> Option<String>,
{
    match value {
        Value::String(s) => {
            *s = resolver.resolve(s)?.expose().to_string();
        }
        Value::Array(items) => {
            for item in items {
                resolve_strings(item, resolver)?;
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                resolve_strings(item, resolver)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Print the one-shot / watch exit summary (§5.1).
fn print_summary(summary: &RunSummary) {
    if summary.interrupted {
        println!("interrupted — in-flight tasks stay in the state DB and resume on next run");
    }
    let s = &summary.stats;
    println!(
        "run summary: fetched {} / ingested {} / dispatched {} / done {} / failed {}",
        s.fetched, s.ingested, s.dispatched, s.done, s.failed
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
