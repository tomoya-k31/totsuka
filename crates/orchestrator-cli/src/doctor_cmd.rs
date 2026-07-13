//! `totsuka doctor` — environment diagnosis (§5.1, F-24): git, config, state
//! DB, installed plugins (with a live probe), LLM key resolution, and orphan
//! worktrees (with an interactive cleanup proposal).

use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use orchestrator_core::adapters::git::SystemGitRunner;
use orchestrator_core::adapters::plugin_host;
use orchestrator_core::config::{self, RootConfig};
use orchestrator_core::worktree::WorktreeManager;
use serde::Serialize;

use crate::common::{CliError, Cx, plugin_init_config, plugin_spec, secret_resolver};
use crate::init_cmd::git_version;

/// One diagnostic result. `action` follows the "cause + next action" rule (§7).
#[derive(Debug, Serialize)]
struct Check {
    name: String,
    ok: bool,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<String>,
}

impl Check {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: true,
            detail: detail.into(),
            action: None,
        }
    }
    fn fail(name: &str, detail: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            ok: false,
            detail: detail.into(),
            action: Some(action.into()),
        }
    }
}

/// Execute `totsuka doctor`.
pub fn run(cx: &Cx, json: bool) -> Result<(), CliError> {
    let mut checks = Vec::new();

    // git availability (worktrees are mandatory).
    match git_version() {
        Some(version) => checks.push(Check::ok("git", format!("git {version}"))),
        None => checks.push(Check::fail(
            "git",
            "git not found on PATH",
            "install git (worktree management requires it)",
        )),
    }

    // Config presence + full offline validation.
    let cfg = match cx.load_config() {
        Ok(cfg) => {
            let env: HashMap<String, String> = std::env::vars().collect();
            let env_fn = |k: &str| env.get(k).cloned();
            let store = cx.store();
            let findings = config::validate(&cfg, &env_fn, |name| {
                store
                    .manifest_of(name)
                    .ok()
                    .flatten()
                    .map(|m| m.capabilities.outputs)
            });
            if config::has_errors(&findings) {
                let first = findings
                    .iter()
                    .find(|f| f.severity == config::FindingSeverity::Error)
                    .map(|f| f.message.clone())
                    .unwrap_or_default();
                checks.push(Check::fail(
                    "config",
                    format!("{} has errors (first: {first})", cx.config_path.display()),
                    "run `totsuka config validate` for the full list",
                ));
            } else {
                checks.push(Check::ok(
                    "config",
                    format!("{} is valid", cx.config_path.display()),
                ));
            }
            Some(cfg)
        }
        Err(e) => {
            checks.push(Check::fail(
                "config",
                e.to_string(),
                "run `totsuka init`, then edit the generated config.toml",
            ));
            None
        }
    };

    // State DB.
    let db = match cx.open_state_db() {
        Ok(db) => {
            checks.push(Check::ok(
                "state-db",
                format!("{} opens", cx.state_db_path().display()),
            ));
            Some(db)
        }
        Err(e) => {
            checks.push(Check::fail(
                "state-db",
                e.to_string(),
                "run `totsuka run` once to create it",
            ));
            None
        }
    };

    if let Some(cfg) = &cfg {
        check_plugins(cx, cfg, &mut checks);
        check_llm_key(cfg, &mut checks);
        check_orphans(cx, cfg, db.as_ref(), json, &mut checks)?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&checks)?);
    } else {
        for check in &checks {
            if check.ok {
                println!("ok:   {} — {}", check.name, check.detail);
            } else {
                println!(
                    "FAIL: {} — {} → {}",
                    check.name,
                    check.detail,
                    check.action.as_deref().unwrap_or("see docs")
                );
            }
        }
    }
    if checks.iter().any(|c| !c.ok) {
        return Err("doctor found problems → follow the actions above".into());
    }
    Ok(())
}

/// Installed + protocol-compatible + live-probe for every enabled plugin.
fn check_plugins(cx: &Cx, cfg: &RootConfig, checks: &mut Vec<Check>) {
    let enabled: Vec<&String> = cfg
        .plugins
        .iter()
        .filter(|(_, p)| p.enabled)
        .map(|(name, _)| name)
        .collect();
    if enabled.is_empty() {
        checks.push(Check::ok("plugins", "no plugins enabled"));
        return;
    }
    let env: HashMap<String, String> = std::env::vars().collect();
    let mut specs = Vec::new();
    for name in enabled {
        match plugin_spec(cx, cfg, name, &env) {
            Ok(spec) => match plugin_init_config(cx, name, &env) {
                Ok(init) => specs.push((spec, init)),
                Err(e) => checks.push(Check::fail(
                    &format!("plugin:{name}"),
                    e.to_string(),
                    "fix the plugin config (secret references must resolve)",
                )),
            },
            Err(e) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                e.to_string(),
                "install it with `totsuka plugin install <dir>`",
            )),
        }
    }
    if specs.is_empty() {
        return;
    }
    // Live probe: launch, initialize, config/validate, shutdown (F-59).
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        checks.push(Check::fail(
            "plugins",
            "could not start an async runtime for plugin probes",
            "re-run; report if it persists",
        ));
        return;
    };
    for (name, result) in runtime.block_on(plugin_host::validate_all(specs)) {
        match result {
            Ok(v) if v.valid => {
                checks.push(Check::ok(
                    &format!("plugin:{name}"),
                    "launches and accepts its config",
                ));
            }
            Ok(v) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                v.errors.join("; "),
                format!("fix plugins/{name}.toml"),
            )),
            Err(e) => checks.push(Check::fail(
                &format!("plugin:{name}"),
                e.to_string(),
                "check the binary and protocol compatibility",
            )),
        }
    }
}

/// The LLM API key reference must resolve (no network call).
fn check_llm_key(cfg: &RootConfig, checks: &mut Vec<Check>) {
    let Some(llm) = &cfg.llm else {
        checks.push(Check::ok(
            "llm",
            "no [llm] configured (repo selection falls back to hints/pending)",
        ));
        return;
    };
    let Some(reference) = &llm.api_key_ref else {
        checks.push(Check::ok("llm", "[llm] configured without api_key_ref"));
        return;
    };
    let env: HashMap<String, String> = std::env::vars().collect();
    match secret_resolver(&env).resolve(reference) {
        Ok(_) => checks.push(Check::ok("llm", "api_key_ref resolves")),
        Err(e) => checks.push(Check::fail(
            "llm",
            format!("api_key_ref does not resolve: {e}"),
            "export the variable or store the key in the Keychain",
        )),
    }
}

/// Detect orphan worktrees (F-24) and, interactively, offer to remove them.
fn check_orphans(
    _cx: &Cx,
    cfg: &RootConfig,
    db: Option<&orchestrator_core::adapters::StateDb>,
    json: bool,
    checks: &mut Vec<Check>,
) -> Result<(), CliError> {
    let Some(db) = db else {
        return Ok(());
    };
    let env: HashMap<String, String> = std::env::vars().collect();
    let env_fn = |k: &str| env.get(k).cloned();
    let known: HashSet<PathBuf> = db
        .list_tasks()?
        .into_iter()
        .filter_map(|t| t.worktree_path.map(PathBuf::from))
        .collect();
    let manager = WorktreeManager::new(SystemGitRunner);

    let mut orphans: Vec<(String, PathBuf, PathBuf)> = Vec::new();
    for repo in &cfg.repositories {
        let Ok(path) =
            orchestrator_core::config::expand_path(&repo.path.to_string_lossy(), &env_fn)
        else {
            continue;
        };
        if let Ok(found) = manager.detect_orphans(&path, &known) {
            for orphan in found {
                orphans.push((repo.name.clone(), path.clone(), orphan));
            }
        }
    }

    if orphans.is_empty() {
        checks.push(Check::ok("worktrees", "no orphan worktrees"));
        return Ok(());
    }

    let listing = orphans
        .iter()
        .map(|(repo, _, path)| format!("{repo}: {}", path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    // Interactive cleanup proposal (§5.1) — only on a TTY and never in --json.
    if !json && io::stdin().is_terminal() {
        for (repo_name, repo_path, orphan) in &orphans {
            print!(
                "remove orphan worktree {} (repo {repo_name})? [y/N]: ",
                orphan.display()
            );
            io::stdout().flush()?;
            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                let out = std::process::Command::new("git")
                    .current_dir(repo_path)
                    .args(["worktree", "remove", &orphan.display().to_string()])
                    .output()?;
                if out.status.success() {
                    println!("removed {}", orphan.display());
                } else {
                    println!(
                        "could not remove (dirty?): {} → remove manually with `git worktree remove --force`",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                }
            }
        }
        checks.push(Check::ok(
            "worktrees",
            format!("orphans handled: {listing}"),
        ));
    } else {
        checks.push(Check::fail(
            "worktrees",
            format!("orphan worktrees: {listing}"),
            "run `totsuka doctor` in a terminal to clean them up interactively",
        ));
    }
    Ok(())
}
