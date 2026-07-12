//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/orca.toml` as JSON (F-64/F-65).

use serde::Deserialize;

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
    "【設計のみ / plan mode】まず設計・計画を提示し、コードの実装や変更はしないでください。\n\n"
        .to_string()
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
