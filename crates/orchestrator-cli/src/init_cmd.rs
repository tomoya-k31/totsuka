//! `totsuka init` — config skeleton generation + environment check (§5.1).
//!
//! Never overwrites existing files: re-running is always safe.

use std::path::Path;
use std::process::Command;

use crate::common::{CliError, Cx};

/// The generated `config.toml` skeleton (§4.6 example, commented out).
const CONFIG_TEMPLATE: &str = r#"# totsuka configuration (https://github.com/tomoya-k31/totsuka)
# Uncomment and adjust. Plugin-specific settings live in plugins/{name}.toml.

# Global maximum concurrent tasks (F-40).
# max_concurrency = 4

# [plugins.github]
# enabled = true
# kind = "task_source"
# poll_interval_secs = 60

# [plugins.herdr]
# enabled = true
# kind = "agent_ide"
# max_concurrency = 3
# timeout_secs = 120

# [[repositories]]
# name = "my-repo"
# path = "~/Workspace/my-repo"
# summary = "What lives in this repository (used for LLM repo selection)"

# [worktree]
# location = "${XDG_STATE_HOME}/totsuka/worktrees/{repo_name}/{branch}"
# cleanup = "manual"            # or "immediate" / { retention_days = 5 }
# plan_cleanup = "immediate"

# [llm]
# base_url = "https://openrouter.ai/api/v1"
# model = "anthropic/claude-haiku-4-5"
# api_key_ref = "keychain:totsuka/openrouter"   # or 1Password: "op://Dev/Openrouter/api_key"

# [[workflows]]
# name = "implement"
# source = "github"
# trigger = { project_status = "実装待ち" }
# mode = "implement"
# agent = "herdr"
# output = "pull_request"
# on_success = { set_status = "レビュー待ち" }
"#;

/// Execute `totsuka init`.
pub fn run(cx: &Cx) -> Result<(), CliError> {
    // 1. Directories (XDG, §5.6).
    for (label, dir) in [
        ("config", cx.config_path.parent().unwrap_or(Path::new("."))),
        ("plugin config", &cx.plugin_config_dir()),
        ("data", cx.paths.data_dir()),
        ("state", cx.paths.state_dir()),
        ("cache", cx.paths.cache_dir()),
    ] {
        std::fs::create_dir_all(dir)?;
        println!("ok: {label} directory {}", dir.display());
    }

    // 2. config.toml skeleton — never overwrite.
    if cx.config_path.exists() {
        println!(
            "skipped: {} already exists (left untouched)",
            cx.config_path.display()
        );
    } else {
        std::fs::write(&cx.config_path, CONFIG_TEMPLATE)?;
        println!("created: {}", cx.config_path.display());
    }

    // 3. Environment checks.
    match git_version() {
        Some(version) => println!("ok: git {version}"),
        None => println!("warning: git not found on PATH → install git (worktrees require it)"),
    }

    println!(
        "next: edit the config, install plugins (`totsuka plugin install <dir>`), then `totsuka run --dry-run`"
    );
    Ok(())
}

/// The installed git version string, if git is on PATH.
pub fn git_version() -> Option<String> {
    let out = Command::new("git").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.trim()
            .strip_prefix("git version ")
            .unwrap_or(text.trim())
            .to_string(),
    )
}
