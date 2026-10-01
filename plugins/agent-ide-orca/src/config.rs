//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `[orca]` table of `config.toml` as JSON (F-65, #554).
//!
//! The launch itself is not configured here: the Orchestrator resolves the full
//! argv/env (`tool_launch`, #196) and this plugin types exactly that into an
//! orca terminal. What is left is how to reach orca and how the terminal is
//! presented.

use serde::Deserialize;

/// orca agent_ide settings.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(extend("x-category" = {"en": "orca", "ja": "orca"}))]
#[serde(deny_unknown_fields)]
pub struct OrcaConfig {
    /// The `orca` executable (name on PATH or absolute path).
    #[serde(default = "default_orca_bin")]
    #[schemars(extend(
        "x-title" = {"en": "orca command", "ja": "orca のコマンド"},
        "x-help" = {"en": "The orca executable: a name on PATH or an absolute path. Default \"orca\".", "ja": "orca の実行ファイル。PATH 上の名前か絶対パス。既定は \"orca\"。"}
    ))]
    pub orca_bin: String,
    /// The longest a single `orca` invocation may run before it is killed. A
    /// `terminal wait` is given its own `--timeout-ms` on top of this.
    #[serde(default = "default_request_timeout")]
    #[schemars(extend(
        "x-title" = {"en": "Request timeout (seconds)", "ja": "リクエストのタイムアウト（秒）"},
        "x-help" = {"en": "How long one orca command may take. Default 30.", "ja": "orca のコマンド 1 回にかけてよい秒数。既定は 30。"}
    ))]
    pub request_timeout_secs: u64,
    /// How the agent's terminal tab is arranged.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = {"en": "Layout", "ja": "レイアウト"},
        "x-help" = {"en": "How the agent's terminal tab is arranged.", "ja": "エージェントのターミナルタブの並べ方。"}
    ))]
    pub layout: LayoutConfig,
    /// Whether the dispatch names the worktree after the task in orca's
    /// sidebar.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = {"en": "Worktree names", "ja": "worktree の名前"},
        "x-help" = {"en": "Whether the worktree is named after the task in orca's sidebar.", "ja": "orca のサイドバーで worktree にタスクの名前を付けるか。"}
    ))]
    pub identity: IdentityConfig,
}

/// `[orca.layout]`: the agent's tab.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    /// Split a companion shell off the agent's terminal (herdr's
    /// `[herdr.layout] shell`).
    ///
    /// **Off by default, unlike herdr.** herdr gives each task a private
    /// workspace, so without the split there is no shell there at all; orca
    /// already shows the task's worktree in its sidebar, where a terminal is
    /// one click away. The split also takes orca's focus into the new pane.
    #[serde(default)]
    #[schemars(extend(
        "x-title" = {"en": "Shell split", "ja": "シェルの分割"},
        "x-help" = {"en": "Split a shell off the agent's terminal. Default off.", "ja": "エージェントのターミナルからシェルを分割する。既定はオフ。"}
    ))]
    pub shell: bool,
    /// `terminal split --direction`. Unset leaves orca's default. A closed
    /// set, so a typo fails `initialize` instead of silently leaving the tab
    /// unsplit (the split itself is best-effort).
    #[serde(default)]
    #[schemars(extend(
        "x-title" = {"en": "Split direction", "ja": "分割の向き"},
        "x-help" = {"en": "The split's direction. Empty leaves orca's default.", "ja": "分割の向き。空なら orca の既定。"}
    ))]
    pub direction: Option<SplitDirection>,
}

/// orca's `terminal split --direction` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// `horizontal`.
    Horizontal,
    /// `vertical`.
    Vertical,
}

impl SplitDirection {
    /// The value `--direction` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            SplitDirection::Horizontal => "horizontal",
            SplitDirection::Vertical => "vertical",
        }
    }
}

/// `[orca.identity]`: what the dispatch tells orca about the task.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IdentityConfig {
    /// Set the worktree's orca display name to `{repo}: {title}` (herdr's
    /// `[herdr.identity]`, #417). Best-effort: a refusal never fails the
    /// dispatch.
    #[serde(default = "default_true")]
    #[schemars(extend(
        "x-title" = {"en": "Enabled", "ja": "有効"},
        "x-help" = {"en": "Show the worktree as \"repo: title\". Default on.", "ja": "worktree を「リポジトリ: タイトル」と表示する。既定はオン。"}
    ))]
    pub enabled: bool,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Keys the pre-`tool_launch` plugin accepted, with what replaced them.
///
/// [`OrcaConfig`] is `deny_unknown_fields`, so without these a config that
/// worked yesterday would fail with serde's `unknown field 'agent'`, which does
/// not say the key was *removed* or what to do instead (same device as the
/// herdr plugin's #411 list).
const REMOVED_KEYS: &[(&str, &str)] = &[
    (
        "agent",
        "the agent is the Orchestrator's resolved `tool_launch` now; choose it with \
         `[tools]` / `default_tool` in the orchestrator config",
    ),
    (
        "setup",
        "the plugin no longer creates worktrees — it opens a terminal in the one \
         totsuka already prepared, so orca's setup script never runs",
    ),
    (
        "repo_selector",
        "the plugin no longer creates worktrees; the terminal is opened in the dispatch \
         worktree by path, and orca finds it once the repository is registered \
         (`orca repo add --path <repository>`)",
    ),
    (
        "plan_prompt_prefix",
        "plan mode is part of the resolved `tool_launch` (`[tools.<name>].plan_args`), \
         not text typed ahead of the prompt",
    ),
    (
        "poll_interval_ms",
        "completion is reported by the agent's hooks; the state stream only blocks on \
         `orca terminal wait --for exit` and no longer polls",
    ),
];

/// The removed keys present in a raw plugin-config object, rendered as
/// operator-facing lines. Empty when the config is clean — including when it is
/// not an object at all, which is a different error and reported by serde.
pub fn removed_keys_in(config: &serde_json::Value) -> Vec<String> {
    let Some(map) = config.as_object() else {
        return Vec::new();
    };
    REMOVED_KEYS
        .iter()
        .filter(|(key, _)| map.contains_key(*key))
        .map(|(key, advice)| {
            format!("`{key}` was removed from `[orca]`: {advice}. Delete the key.")
        })
        .collect()
}

fn default_orca_bin() -> String {
    "orca".to_string()
}
fn default_request_timeout() -> u64 {
    30
}
fn default_true() -> bool {
    true
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
        assert_eq!(cfg.request_timeout_secs, 30);
        assert!(!cfg.layout.shell);
        assert!(cfg.layout.direction.is_none());
        assert!(cfg.identity.enabled);
    }

    #[test]
    fn nested_tables_parse() {
        let cfg = parse(serde_json::json!({
            "layout": { "shell": true, "direction": "vertical" },
            "identity": { "enabled": false },
        }));
        assert!(cfg.layout.shell);
        assert_eq!(cfg.layout.direction, Some(SplitDirection::Vertical));
        assert!(!cfg.identity.enabled);
    }

    #[test]
    fn a_misspelt_direction_is_rejected() {
        let err = serde_json::from_value::<OrcaConfig>(
            serde_json::json!({ "layout": { "direction": "vertial" } }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("vertial"), "got {err}");
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err =
            serde_json::from_value::<OrcaConfig>(serde_json::json!({ "typo": 1 })).unwrap_err();
        assert!(err.to_string().contains("typo"), "got {err}");
    }

    #[test]
    fn removed_keys_are_named_with_their_replacement() {
        for (key, _) in REMOVED_KEYS {
            let found = removed_keys_in(&serde_json::json!({ *key: "whatever" }));
            assert_eq!(found.len(), 1, "{key}");
            assert!(found[0].contains(key), "{}", found[0]);
            assert!(found[0].contains("Delete the key"), "{}", found[0]);
        }
        assert!(removed_keys_in(&serde_json::json!({ "orca_bin": "orca" })).is_empty());
        assert!(removed_keys_in(&serde_json::json!("not an object")).is_empty());
    }
}

#[cfg(test)]
mod schema_tests {
    /// Every key of `[orca]`, at any depth, carries an `x-title` and `x-help`
    /// in English and Japanese for the settings window (ADR-0109).
    #[test]
    fn every_key_has_bilingual_help() {
        let schema = plugin_sdk::config_schema::of::<super::OrcaConfig>().schema;
        let missing = plugin_sdk::config_schema::missing_help(&schema);
        assert!(missing.is_empty(), "{}", missing.join("\n"));
        assert!(schema["x-category"]["ja"].is_string(), "{schema}");
    }
}
