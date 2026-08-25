//! `totsuka init` — config skeleton generation + environment check (§5.1).
//!
//! Never overwrites existing files: re-running is always safe.

use std::path::Path;
use std::process::Command;

use crate::common::{CliError, Cx};

/// The generated `config.toml` skeleton (§4.6 example, commented out).
const CONFIG_TEMPLATE: &str = r#"# totsuka configuration (https://github.com/tomoya-k31/totsuka)
# Uncomment and adjust. One file: `[plugins.<name>]` says which plugins run,
# and a top-level `[<name>]` table holds that plugin's own settings.

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
# project = "my-board"          # where tasks for this repository are filed

# The trackers you file into: a GitHub Project, a Notion database. `name` and
# `source` are totsuka's — a repository points at one by `name`, and `source`
# says which plugin owns it. Every other key belongs to that plugin.
# [[projects]]
# name = "my-board"
# source = "github"
# owner = "my-org"
# project_number = 1

# [worktree]
# Default: <state dir>/worktrees/{repo_name}/{worktree_name}, where <state dir> is
# $XDG_STATE_HOME/totsuka (or $HOME/.local/state/totsuka when XDG_STATE_HOME is
# unset). Set this only to override it; ${ENV} references are expanded, and an
# unset variable is an error.
# location = "~/.worktrees/{repo_name}/{worktree_name}"
# cleanup = "manual"            # or "immediate" / { retention_days = 5 }
# plan_cleanup = "immediate"

# A plugin's own settings go in a top-level table named after it. The
# Orchestrator does not interpret what is inside; the plugin validates it
# (`totsuka config validate`). A name with no `[plugins.<name>]` entry is an
# error, so a typo here is caught rather than read as settings nobody asked for.
# [github]
# token = "cmd:gh auth token"

# [llm]
# base_url = "https://openrouter.ai/api/v1"
# model = "anthropic/claude-haiku-4-5"
# api_key_ref = "keychain:totsuka/openrouter"   # or 1Password: "op://Dev/Openrouter/api_key"

# [[workflows]]
# name = "implement"
# source = "github"
# trigger = { project_status = "Ready to implement" }
# profile = "implement"          # resolves mode / output / verification
# agent = "herdr"
# on_success = { set_status = "In review" }
#
# `profile` resolves `output` for you. Do not write `output = "source"` for a
# github or notion workflow: those plugins publish nothing (the agent writes
# the deliverable itself), so the config would be rejected.
"#;

/// Create the XDG directories totsuka writes into (§5.6).
///
/// Shared with `totsuka setup`, which needs the same directories to exist
/// before it writes anything — so a fresh machine does not have to run `init`
/// first just to make `setup` work.
pub fn ensure_dirs(cx: &Cx) -> Result<(), CliError> {
    for (label, dir) in [
        ("config", cx.config_path.parent().unwrap_or(Path::new("."))),
        ("data", cx.paths.data_dir()),
        ("state", cx.paths.state_dir()),
        ("cache", cx.paths.cache_dir()),
    ] {
        std::fs::create_dir_all(dir)?;
        println!("ok: {label} directory {}", dir.display());
    }
    Ok(())
}

/// Execute `totsuka init`.
pub fn run(cx: &Cx) -> Result<(), CliError> {
    // 1. Directories (XDG, §5.6).
    ensure_dirs(cx)?;

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
        "next: `totsuka setup` fills the config in interactively — or edit it by hand, install \
         plugins (`totsuka plugin install --bundled --all --enable`), then `totsuka run --dry-run`"
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
