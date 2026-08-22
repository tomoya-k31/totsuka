//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/orca.toml` as JSON (F-64/F-65).
//!
//! Prompt text lives in the sibling `defaults.toml` rather than in Rust string
//! literals (#317), so adjusting the wording is an edit to a data file instead
//! of a code change. Every prompt default there is still just a
//! `#[serde(default = "...")]` fallback: the live value comes from
//! `plugins/orca.toml` whenever the operator sets it. Note the two files use
//! different key names — `defaults.toml` groups prompts under `[prompts]`,
//! while `plugins/orca.toml` is a flat table of [`OrcaConfig`] fields.

use std::sync::LazyLock;

use serde::Deserialize;

/// The embedded prompt defaults, parsed once on first use.
///
/// A malformed `defaults.toml` is a build-time authoring error, not a runtime
/// condition — it ships inside the binary and no input can change it — so this
/// panics rather than degrading. The first use is deserializing `initialize`'s
/// config, so without a test the panic would land there; `embedded_defaults_parse`
/// forces it in CI instead.
static DEFAULTS: LazyLock<Defaults> = LazyLock::new(|| {
    toml::from_str(include_str!("defaults.toml")).expect("embedded defaults.toml must parse")
});

/// Top level of `defaults.toml`.
///
/// `deny_unknown_fields`, like [`OrcaConfig`]: a key that no longer backs
/// anything is dead prompt text that still reads as live, so a rename must
/// fail the build rather than leave the stale copy sitting in the file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Defaults {
    prompts: DefaultPrompts,
}

/// The `[prompts]` table of `defaults.toml`. Field names are the TOML keys, and
/// are deliberately *not* the `plugins/orca.toml` override keys — each one names
/// its counterpart below.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DefaultPrompts {
    /// Backs [`OrcaConfig::plan_prompt_prefix`] (overridden as
    /// `plan_prompt_prefix`, not `plan_prefix`).
    plan_prefix: String,
}

/// orca agent_ide settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrcaConfig {
    /// The `orca` executable (name on PATH or absolute path).
    #[serde(default = "default_orca_bin")]
    pub orca_bin: String,
    /// The agent orca launches in the worktree terminal (F-31).
    #[serde(default = "default_agent")]
    pub agent: String,
    /// The `--setup` mode for `worktree create` (`run`/`skip`/`inherit`).
    #[serde(default = "default_setup")]
    pub setup: String,
    /// Explicit `--repo` selector (e.g. `id:…`/`path:/abs`/`branch:…`). When
    /// unset, the dispatch worktree path is used as a `path:` selector.
    #[serde(default)]
    pub repo_selector: Option<String>,
    /// Text prepended to the prompt in plan mode (F-36). orca has no structured
    /// plan API, so plan intent is conveyed to the agent in-prompt.
    #[serde(default = "default_plan_prefix")]
    pub plan_prompt_prefix: String,
    /// Poll interval (ms) for the state loop when no terminal handle is
    /// available to block on. Also the `--timeout-ms` passed to
    /// `orca terminal wait` for pacing.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
}

impl OrcaConfig {
    /// The `--repo` selector for `worktree create`: the explicit config value,
    /// else the dispatch worktree path as a `path:` selector.
    pub fn repo_selector_for(&self, worktree_path: &str) -> String {
        self.repo_selector
            .clone()
            .unwrap_or_else(|| format!("path:{worktree_path}"))
    }

    /// Compose the launch prompt, prepending the plan directive in plan mode.
    pub fn compose_prompt(&self, base: &str, plan: bool) -> String {
        if plan {
            format!("{}{base}", self.plan_prompt_prefix)
        } else {
            base.to_string()
        }
    }
}

fn default_orca_bin() -> String {
    "orca".to_string()
}
fn default_agent() -> String {
    "claude".to_string()
}
fn default_setup() -> String {
    "inherit".to_string()
}
fn default_plan_prefix() -> String {
    DEFAULTS.prompts.plan_prefix.clone()
}
fn default_poll_interval() -> u64 {
    2000
}

/// Turn an arbitrary task id into an orca-safe worktree name (alphanumerics,
/// `-` and `_`; other runs collapse to a single `-`).
pub fn worktree_name(task_id: &str) -> String {
    let mut name = String::new();
    let mut last_dash = false;
    for ch in task_id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            name.push(ch);
            last_dash = false;
        } else if !last_dash {
            name.push('-');
            last_dash = true;
        }
    }
    let trimmed = name.trim_matches('-').to_string();
    let base = if trimmed.is_empty() {
        "task".to_string()
    } else {
        trimmed
    };
    format!("totsuka-{base}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: serde_json::Value) -> OrcaConfig {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(serde_json::json!({}));
        assert_eq!(cfg.orca_bin, "orca");
        assert_eq!(cfg.agent, "claude");
        assert_eq!(cfg.setup, "inherit");
        assert_eq!(cfg.poll_interval_ms, 2000);
        assert!(cfg.repo_selector.is_none());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err =
            serde_json::from_value::<OrcaConfig>(serde_json::json!({ "typo": 1 })).unwrap_err();
        assert!(err.to_string().contains("typo"), "got {err}");
    }

    #[test]
    fn repo_selector_falls_back_to_worktree_path() {
        let cfg = parse(serde_json::json!({}));
        assert_eq!(cfg.repo_selector_for("/wt/x"), "path:/wt/x");
        let explicit = parse(serde_json::json!({ "repo_selector": "id:abc" }));
        assert_eq!(explicit.repo_selector_for("/wt/x"), "id:abc");
    }

    #[test]
    fn embedded_defaults_parse() {
        // Force the LazyLock so a malformed `defaults.toml` fails here rather
        // than on the first dispatch in a real pane.
        assert!(!DEFAULTS.prompts.plan_prefix.is_empty());
    }

    #[test]
    fn default_plan_prefix_matches_todays_literal() {
        // Was pinned against the literal as it stood in Rust before #317
        // moved it into `defaults.toml`. That literal was Japanese and has
        // been rewritten into English, so there is no pre-#317 text left to
        // transcribe: the expectation below is now derived from `DEFAULTS`
        // and therefore only catches an *unintended* edit, not a wrong one.
        // The `ends_with("\n\n")` assertion under it is the one that still
        // holds on merit — that shape is what `compose_prompt` depends on.
        assert_eq!(
            default_plan_prefix(),
            "[Design only / plan mode] Present a design and a plan first. \
             Do not implement or modify code.\n\n"
        );
        // The trailing blank line separates the directive from the task prompt
        // that `compose_prompt` concatenates onto it.
        assert!(default_plan_prefix().ends_with("\n\n"));
    }

    #[test]
    fn plan_prompt_is_prefixed_only_in_plan_mode() {
        let cfg = parse(serde_json::json!({ "plan_prompt_prefix": "PLAN: " }));
        assert_eq!(cfg.compose_prompt("do it", false), "do it");
        assert_eq!(cfg.compose_prompt("do it", true), "PLAN: do it");
    }

    #[test]
    fn worktree_name_is_sanitized() {
        assert_eq!(worktree_name("T-123"), "totsuka-T-123");
        assert_eq!(worktree_name("owner/repo#45"), "totsuka-owner-repo-45");
        assert_eq!(worktree_name("!!!"), "totsuka-task");
    }
}
