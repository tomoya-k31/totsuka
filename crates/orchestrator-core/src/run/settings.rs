//! Interpreted engine configuration: the types [`Engine`](super::Engine) is
//! built from, and the translation from a parsed [`RootConfig`] into them.
//!
//! Split out of `run/mod.rs` (#464) unchanged — this is the layer that turns
//! what the operator wrote into what the loop consumes, and it has no
//! behaviour of its own beyond that translation.

use super::*;

/// Errors that abort the run loop (per-task failures are handled in-loop).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// State DB failure — the loop cannot proceed without persistence.
    #[error(transparent)]
    Db(#[from] StateError),
}

/// A repository the engine can target (paths already expanded).
#[derive(Debug, Clone, PartialEq)]
pub struct RepoSettings {
    /// Repository name (config `[[repositories]].name`).
    pub name: String,
    /// Absolute local clone path.
    pub path: PathBuf,
    /// Free-text summary for LLM selection (F-61).
    pub summary: Option<String>,
    /// Per-repo worktree location template override (F-22).
    pub worktree_location: Option<String>,
    /// Per-repo default AI tool (#196); resolved at dispatch time
    /// (workflow pin > this > global default), same carry-unresolved pattern
    /// as `worktree_location`.
    pub tool: Option<String>,
}

/// Interpreted engine configuration, assembled from [`RootConfig`] by
/// [`settings_from_config`] (or built directly in tests).
#[derive(Debug, Clone)]
pub struct EngineSettings {
    /// Workflows in definition order (F-81).
    pub workflows: Vec<Workflow>,
    /// Target repositories.
    pub repos: Vec<RepoSettings>,
    /// Concurrency limits (F-40–F-42).
    pub limits: Limits,
    /// Worktree directory-name template, filling `{worktree_name}` in
    /// `location_template` (F-22 addendum).
    pub worktree_name_template: String,
    /// Global worktree location template (F-22).
    pub location_template: String,
    /// Cleanup policy for implement-mode worktrees (F-23).
    pub cleanup_implement: CleanupPolicy,
    /// Cleanup policy for plan-mode worktrees (F-85).
    pub cleanup_plan: CleanupPolicy,
    /// Environment for `${ENV}` expansion in worktree templates.
    pub env: HashMap<String, String>,
    /// Repo-selection tuning (F-14).
    pub select: SelectConfig,
    /// README head cache directory (`$XDG_CACHE_HOME/totsuka`), if any.
    pub readme_cache_dir: Option<PathBuf>,
    /// Minimum interval between worktree-retention sweeps (#210). Not exposed
    /// in config (no user knob); tests set [`Duration::ZERO`] to sweep every
    /// cycle.
    pub worktree_sweep_interval: Duration,
    /// One-shot's quiet-period floor before an empty `settled()` is trusted:
    /// every source is push-only, so a task submitted moments after launch may
    /// not have arrived yet. Not exposed in config (no user knob), same as
    /// [`worktree_sweep_interval`](Self::worktree_sweep_interval);
    /// tests that drive submissions in-process set [`Duration::ZERO`] so a
    /// one-shot run does not spend the grace waiting for a task that has
    /// already arrived.
    pub one_shot_grace: Duration,
    /// Resolved AI-tool registry (#196): built-ins overlaid with `[tools]`
    /// entries, keyed by tool name. Dispatch resolves each task's tool here
    /// and sends the assembled [`ToolLaunchSpec`](plugin_protocol::methods::ToolLaunchSpec) to the agent plugin.
    pub tools: std::collections::HashMap<String, ToolProfile>,
    /// Global default tool name (#196) when neither the workflow nor the
    /// selected repository picks one. `"claude"` unless `default_tool` is set.
    pub default_tool: String,
    /// Resolved prompt sets (#314): built-in defaults overlaid with
    /// `[prompts]` and each workflow's `prompts` / legacy `rubric`. Resolved
    /// once here so dispatch never re-reads config per task.
    pub prompts: crate::prompts::PromptSet,
    /// How crashed plugins are brought back (#495). Not exposed in config
    /// beyond the per-plugin `restart` switch; see [`RestartPolicy`].
    pub plugin_restart: RestartPolicy,
    /// Plugin instance names whose `[plugins.{name}].restart` is `false`
    /// (#495) — detected and reported when they die, never relaunched.
    pub restart_disabled: std::collections::HashSet<String>,
    /// Claude Code hook runtime (#131/#138): receiver endpoint, auth token,
    /// spool dir, per-workflow `--settings` paths, and the escalation
    /// threshold. A normal `totsuka run` always sets this (the CLI builds it
    /// even when `[hooks]` is unset — a default socket path is used, so a config
    /// with no hook-capable agent simply never receives a POST). `None` only for
    /// `--dry-run` (read-only: no receiver, no dispatch) and hook-disabled
    /// tests; when `None` the receiver never starts and dispatch never resolves
    /// a hook launch (no `--settings`, no `TOTSUKA_*` env).
    pub hook: Option<HookRuntime>,
}

/// Everything the engine needs to drive hook-based agents for one run
/// (#131/#138). Assembled by the CLI (`run_cmd`): it resolves the Bearer token
/// via the platform secret store, expands the socket/spool paths, and looks up
/// each workflow's rendered settings file. `None` in tests and in configs with
/// no hook-capable agent.
#[derive(Debug, Clone)]
pub struct HookRuntime {
    /// UDS path the receiver binds and hooks POST to (also injected as
    /// `TOTSUKA_HOOK_ENDPOINT`). Created `0600`; stale sockets are unlinked.
    pub socket_path: PathBuf,
    /// Bearer token every POST must present (`Authorization: Bearer <token>`),
    /// also injected as `TOTSUKA_HOOK_TOKEN`. `None` disables the check (0600
    /// socket only); the CLI logs a warning in that case.
    pub auth_token: Option<SecretString>,
    /// Directory the hooks spool NDJSON to when a POST fails (E-07), also
    /// injected as `TOTSUKA_HOOK_SPOOL_DIR`. The engine drains it after
    /// `recover()` and on every cycle. `None` disables at-least-once recovery.
    pub spool_dir: Option<PathBuf>,
    /// Per-workflow rendered `orchestrator-<workflow>.json` path (baked into
    /// `ToolLaunchSpec.args` as `--settings <path>`), keyed by workflow name
    /// (H-01/H-03).
    pub settings_paths: HashMap<String, PathBuf>,
    /// Consecutive UNKNOWN stops before a task escalates (D-02).
    pub block_retry_limit: u32,
}

/// Interpret a parsed [`RootConfig`] into [`EngineSettings`].
///
/// `env` supplies `${ENV}`/`~` expansion for repository paths and worktree
/// templates (injectable for tests). `paths` supplies the XDG-resolved bases
/// for defaults the operator did not configure; it is passed in rather than
/// re-resolved here so the engine and the CLI (state DB, logs, hook spool)
/// always agree on one set of directories.
pub fn settings_from_config(
    cfg: &RootConfig,
    env: &HashMap<String, String>,
    paths: &Paths,
) -> Result<EngineSettings, ResolveError> {
    let env_fn = |k: &str| env.get(k).cloned();

    let mut repos = Vec::with_capacity(cfg.repositories.len());
    for repo in &cfg.repositories {
        repos.push(RepoSettings {
            name: repo.name.clone(),
            path: crate::config::expand_path(&repo.path.to_string_lossy(), &env_fn)?,
            summary: repo.summary.clone(),
            worktree_location: repo.worktree_location.clone(),
            tool: repo.tool.clone(),
        });
    }

    let limits = Limits {
        global: cfg.max_concurrency.unwrap_or(DEFAULT_GLOBAL_CONCURRENCY),
        per_repo: cfg
            .repositories
            .iter()
            .filter_map(|r| r.max_concurrency.map(|n| (r.name.clone(), n)))
            .collect(),
        per_agent: cfg
            .plugins
            .iter()
            .filter(|(_, p)| p.kind == PluginKind::AgentIde)
            .filter_map(|(name, p)| p.max_concurrency.map(|n| (name.clone(), n)))
            .collect(),
    };

    Ok(EngineSettings {
        workflows: Workflow::from_configs(&cfg.workflows),
        repos,
        limits,
        worktree_name_template: DEFAULT_WORKTREE_NAME_TEMPLATE.to_string(),
        location_template: cfg
            .worktree
            .location
            .clone()
            .unwrap_or_else(|| default_location_template(paths)),
        // Implement-mode default is `manual`: a worktree may hold committed but
        // unpushed work until the output policy (#65) publishes it.
        cleanup_implement: cleanup_policy(cfg.worktree.cleanup, CleanupPolicy::Manual),
        // Plan-mode default is `immediate` (F-85): design output is published
        // to the source, the worktree carries nothing unique.
        cleanup_plan: cleanup_policy(cfg.worktree.plan_cleanup, CleanupPolicy::Immediate),
        env: env.clone(),
        select: SelectConfig {
            max_tokens: cfg.llm.as_ref().and_then(|l| l.max_tokens),
            ..SelectConfig::default()
        },
        readme_cache_dir: None,
        worktree_sweep_interval: WORKTREE_SWEEP_INTERVAL,
        one_shot_grace: ONE_SHOT_GRACE,
        tools: crate::tool::registry_from_config(&cfg.tools),
        default_tool: cfg
            .default_tool
            .clone()
            .unwrap_or_else(|| "claude".to_string()),
        prompts: crate::prompts::PromptSet::from_config(cfg),
        plugin_restart: RestartPolicy::default(),
        restart_disabled: cfg
            .plugins
            .iter()
            .filter(|(_, p)| !p.restart)
            .map(|(name, _)| name.clone())
            .collect(),
        // The hook runtime needs the resolved token, expanded paths, and the
        // per-workflow settings files — all CLI-level (secret store, `Paths`,
        // the `hooks` module). `run_cmd` fills this in before building the
        // engine; interpreting config alone leaves it unset.
        hook: None,
    })
}

/// Map a config cleanup policy to the worktree policy, with a default. The
/// `keep_*` presets (#210) desugar to `RetentionDays` here — [`CleanupPolicy`]
/// never learns about them.
fn cleanup_policy(config: Option<CleanupPolicyConfig>, default: CleanupPolicy) -> CleanupPolicy {
    match config {
        None => default,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Immediate)) => CleanupPolicy::Immediate,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Manual)) => CleanupPolicy::Manual,
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Keep7d)) => {
            CleanupPolicy::RetentionDays(7)
        }
        Some(CleanupPolicyConfig::Named(CleanupPolicyName::Keep28d)) => {
            CleanupPolicy::RetentionDays(28)
        }
        Some(CleanupPolicyConfig::Retention { retention_days }) => {
            CleanupPolicy::RetentionDays(retention_days)
        }
    }
}

/// The launched plugins, split by kind (enabled entries only, F-58).
#[derive(Debug, Default)]
pub struct PluginSet {
    /// task_source plugins by instance name.
    pub sources: HashMap<String, Plugin>,
    /// agent_ide plugins by instance name.
    pub agents: HashMap<String, Plugin>,
    /// notifier plugins by instance name.
    pub notifiers: HashMap<String, Plugin>,
    /// The launch spec each plugin was started from, by instance name — all a
    /// restart needs (#495).
    ///
    /// Deliberately a **separate map** rather than a field on a wrapper
    /// around [`Plugin`]: the three maps above are built by hand in a few
    /// dozen tests, and none of them care about restarting. A name missing
    /// here is detected as dead and reported, but never relaunched, which is
    /// exactly what those tests want.
    pub specs: HashMap<String, crate::adapters::plugin_host::PluginSpec>,
}

/// How hard the engine tries to bring a crashed plugin back (#495).
///
/// Not exposed in `config.toml` beyond the per-plugin on/off switch
/// (`[plugins.{name}].restart`): the shape of the backoff is not something an
/// operator has the information to tune, and every knob here would be one
/// more thing that can be set to a value nobody tested. Tests set
/// [`first_backoff`](Self::first_backoff) to [`Duration::ZERO`], the same
/// seam [`worktree_sweep_interval`](EngineSettings::worktree_sweep_interval)
/// and [`one_shot_grace`](EngineSettings::one_shot_grace) already use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Attempts allowed inside [`window`](Self::window) before the plugin is
    /// given up on and escalated.
    pub max_attempts: u32,
    /// The sliding window the attempt count is measured over. A plugin that
    /// crashes once a day forever is not the failure this budget is for.
    pub window: Duration,
    /// Delay before the first attempt; doubled for each subsequent one.
    pub first_backoff: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            window: Duration::from_secs(300),
            first_backoff: Duration::from_secs(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// XDG bases resolved from a fake environment, mirroring what the CLI
    /// hands to [`settings_from_config`].
    fn test_paths(pairs: &[(&str, &str)]) -> Paths {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        Paths::from_env(|k| map.get(k).cloned()).unwrap()
    }

    #[test]
    fn settings_interpret_limits_and_cleanup() {
        let cfg = RootConfig::from_toml_str(
            r#"
max_concurrency = 2

[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3

[[repositories]]
name = "web"
path = "~/repos/web"
max_concurrency = 1

[worktree]
cleanup = "immediate"
plan_cleanup = { retention_days = 2 }
"#,
        )
        .unwrap();
        let env = HashMap::from([("HOME".to_string(), "/home/t".to_string())]);
        let settings =
            settings_from_config(&cfg, &env, &test_paths(&[("HOME", "/home/t")])).unwrap();

        assert_eq!(settings.limits.global, 2);
        assert_eq!(settings.limits.per_repo.get("web"), Some(&1));
        assert_eq!(settings.limits.per_agent.get("herdr"), Some(&3));
        assert_eq!(settings.repos[0].path, PathBuf::from("/home/t/repos/web"));
        assert_eq!(settings.cleanup_implement, CleanupPolicy::Immediate);
        assert_eq!(settings.cleanup_plan, CleanupPolicy::RetentionDays(2));
    }

    #[test]
    fn settings_defaults_are_safe() {
        let cfg = RootConfig::from_toml_str("").unwrap();
        let paths = test_paths(&[("HOME", "/home/t"), ("XDG_STATE_HOME", "/xdg/state")]);
        let settings = settings_from_config(&cfg, &HashMap::new(), &paths).unwrap();
        assert_eq!(settings.limits.global, DEFAULT_GLOBAL_CONCURRENCY);
        // Implement keeps work (manual); plan cleans immediately (F-85).
        // #210 deliberately did NOT change these defaults.
        assert_eq!(settings.cleanup_implement, CleanupPolicy::Manual);
        assert_eq!(settings.cleanup_plan, CleanupPolicy::Immediate);
        assert_eq!(
            settings.location_template,
            "/xdg/state/totsuka/worktrees/{repo_name}/{worktree_name}"
        );
        assert_eq!(
            settings.worktree_name_template,
            DEFAULT_WORKTREE_NAME_TEMPLATE
        );
        assert_eq!(settings.worktree_sweep_interval, WORKTREE_SWEEP_INTERVAL);
        // Promoting this from a const to a settings field must not change what
        // production runs with: tests shrink it, `settings_from_config` must
        // not.
        assert_eq!(settings.one_shot_grace, ONE_SHOT_GRACE);
    }

    /// The default worktree location must resolve on a machine with no
    /// `XDG_STATE_HOME` (the macOS norm). It used to be the literal template
    /// `"${XDG_STATE_HOME}/totsuka/worktrees/..."`, which `expand_env` rejects
    /// when the variable is unset — `totsuka run` started fine and then failed
    /// *every* dispatch at worktree creation.
    #[test]
    fn default_location_falls_back_to_home_without_xdg_state_home() {
        let cfg = RootConfig::from_toml_str("").unwrap();
        // Deliberately no XDG_STATE_HOME, in `paths` or in the expansion env.
        let env = HashMap::from([("HOME".to_string(), "/home/t".to_string())]);
        let settings =
            settings_from_config(&cfg, &env, &test_paths(&[("HOME", "/home/t")])).unwrap();

        assert_eq!(
            settings.location_template,
            "/home/t/.local/state/totsuka/worktrees/{repo_name}/{worktree_name}"
        );
        // The template no longer carries a `${ENV}` reference, so rendering
        // cannot fail on an unset variable.
        assert!(!settings.location_template.contains("${"));
    }

    /// An operator-supplied `[worktree].location` keeps full `${ENV}` support —
    /// only the *default* stopped going through env expansion.
    #[test]
    fn explicit_location_still_wins_over_the_default() {
        let cfg = RootConfig::from_toml_str(
            r#"
[worktree]
location = "${MY_ROOT}/wt/{worktree_name}"
"#,
        )
        .unwrap();
        let settings =
            settings_from_config(&cfg, &HashMap::new(), &test_paths(&[("HOME", "/home/t")]))
                .unwrap();
        assert_eq!(settings.location_template, "${MY_ROOT}/wt/{worktree_name}");
    }

    #[test]
    fn cleanup_presets_map_to_retention_days() {
        // `keep_7d` / `keep_28d` (#210) are config-layer sugar: they desugar
        // to `RetentionDays` here and `CleanupPolicy` never sees them.
        let cfg = RootConfig::from_toml_str(
            r#"
[worktree]
cleanup = "keep_7d"
plan_cleanup = "keep_28d"
"#,
        )
        .unwrap();
        let settings =
            settings_from_config(&cfg, &HashMap::new(), &test_paths(&[("HOME", "/home/t")]))
                .unwrap();
        assert_eq!(settings.cleanup_implement, CleanupPolicy::RetentionDays(7));
        assert_eq!(settings.cleanup_plan, CleanupPolicy::RetentionDays(28));
    }
}
