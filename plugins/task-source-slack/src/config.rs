//! Plugin settings, deserialized from `InitializeParams.config` — the resolved
//! `plugins/slack.toml` as JSON with secrets already expanded (F-64/F-65).
//!
//! Two Slack tokens are involved: the App-Level Token (`xapp-`) opens the
//! Socket Mode WebSocket, and the *user* token (`xoxp-`) calls the Web API so
//! replies are posted under the operator's own name — there is no bot user.

use serde::Deserialize;

/// The OpenAI-compatible LLM used for repository classification when channel
/// rules leave more than one candidate. This is the plugin's own LLM call
/// (repo resolution happens entirely inside this plugin; tasks are submitted
/// with a resolved `repo_hint`). An explicit `[llm]` here always wins; when
/// omitted, the orchestrator's `[llm]` (supplied at `initialize` since
/// protocol 0.1.2, #119) is adopted as the default — see `server`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    /// OpenAI-compatible base URL (e.g. `https://openrouter.ai/api/v1`).
    pub base_url: String,
    /// Model identifier.
    pub model: String,
    /// API key (resolved by the orchestrator, F-65).
    pub api_key: String,
    /// Minimum classification confidence; below it the plugin falls back to
    /// asking in-thread via an ephemeral message.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
}

/// A channel-name prefix rule narrowing repository candidates (first match in
/// declaration order wins).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelGroup {
    /// Channel-name prefix (e.g. `dev-frontend-`).
    pub prefix: String,
    /// Candidate repository names; each must exist in [`SlackConfig::repos`].
    pub repos: Vec<String>,
}

/// A candidate repository the plugin may resolve a mention to. `name` must
/// match a `[[repositories]].name` in the orchestrator's `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoInfo {
    /// Repository name (as known to the orchestrator).
    pub name: String,
    /// One-line description fed to the LLM classifier.
    #[serde(default)]
    pub summary: Option<String>,
    /// Local checkout path; when set, the README head is added as classifier
    /// material.
    #[serde(default)]
    pub path: Option<String>,
}

/// Slack task-source settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackConfig {
    /// App-Level Token (`xapp-`) for Socket Mode.
    pub app_token: String,
    /// User OAuth Token (`xoxp-`); replies are posted as the operator.
    pub user_token: String,
    /// The operator's own Slack user id (`U…`). Mentions of this user become
    /// tasks, and the TokenGuard refuses a token belonging to anyone else.
    pub target_user_id: String,
    /// How many recent thread messages to include as context.
    #[serde(default = "default_thread_context_limit")]
    pub thread_context_limit: u32,
    /// Optional tone/style instruction injected into the task body.
    #[serde(default)]
    pub reply_style: Option<String>,
    /// The plugin instance name stamped onto each `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// Repository-selection LLM. Required when more than one repository
    /// candidate ends up declared (with a single candidate there is nothing
    /// to classify) — but optional since #119: when omitted, the
    /// orchestrator's `[llm]` (supplied at `initialize`) fills in.
    #[serde(default)]
    pub llm: Option<LlmConfig>,
    /// Channel-prefix rules, checked before the LLM (first match wins).
    #[serde(default)]
    pub channel_groups: Vec<ChannelGroup>,
    /// Candidate repositories. Optional since #109: when omitted, the
    /// orchestrator's `[[repositories]]` (supplied at `initialize`) become
    /// the candidates; an explicit list here always wins.
    #[serde(default)]
    pub repos: Vec<RepoInfo>,
    /// Slack Web API base URL (overridable for tests).
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// State-directory root for the persisted draft store (#122), replacing
    /// `${XDG_STATE_HOME:-~/.local/state}/totsuka` (overridable for tests).
    #[serde(default)]
    pub state_dir: Option<std::path::PathBuf>,
    /// Max retry attempts for retryable API failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl SlackConfig {
    /// The declared repository names.
    pub fn repo_names(&self) -> Vec<&str> {
        self.repos.iter().map(|r| r.name.as_str()).collect()
    }
}

fn default_thread_context_limit() -> u32 {
    6
}
fn default_source_name() -> String {
    "slack".to_string()
}
fn default_api_url() -> String {
    "https://slack.com/api".to_string()
}
fn default_max_retries() -> u32 {
    3
}
pub(crate) fn default_confidence_threshold() -> f64 {
    0.6
}

/// Static (offline) config problems for `config/validate` (F-63), each in the
/// "cause → next action" form (§7). Live token verification is *not* done
/// here — that is the TokenGuard's job at `initialize` — so `config validate`
/// and `doctor` probes stay deterministic and network-free.
pub fn static_config_errors(config: &SlackConfig) -> Vec<String> {
    let mut errors = Vec::new();

    if !config.app_token.starts_with("xapp-") {
        errors.push(
            "`app_token` is not an App-Level Token (must start with `xapp-`) → generate one \
             under the Slack app's Basic Information > App-Level Tokens (scope \
             `connections:write`) and update plugins/slack.toml"
                .into(),
        );
    }
    if !config.user_token.starts_with("xoxp-") {
        errors.push(
            "`user_token` is not a User OAuth Token (must start with `xoxp-`; a bot token \
             `xoxb-` cannot post as you) → copy the User OAuth Token from the Slack app's \
             OAuth & Permissions page and update plugins/slack.toml"
                .into(),
        );
    }
    if config.target_user_id.is_empty() {
        errors.push(
            "`target_user_id` is empty → set your Slack user id (profile > … > \
             Copy member ID)"
                .into(),
        );
    }
    if config.thread_context_limit == 0 {
        errors.push("`thread_context_limit` is 0 → set it to 1 or more".into());
    }

    // An empty `[[repos]]` is legal here: the orchestrator supplies its
    // `[[repositories]]` at `initialize` (#109), where the merged candidate
    // list is validated (see `server`). Offline validation can only check
    // an explicit list.
    let names = config.repo_names();
    for (i, name) in names.iter().enumerate() {
        if names[..i].contains(name) {
            errors.push(format!(
                "`[[repos]]` declares `{name}` more than once → remove the duplicate entry"
            ));
        }
    }
    // More than one candidate needs an `[llm]` to classify — but offline
    // validation cannot know whether `initialize` will supply the
    // orchestrator's `[llm]` as the default (#119), so that check runs at
    // `initialize` (see `server`), like the empty-`[[repos]]` case above.
    if let Some(llm) = &config.llm {
        if llm.base_url.is_empty() {
            errors.push("`llm.base_url` is empty → set the OpenAI-compatible base URL".into());
        }
        if llm.model.is_empty() {
            errors.push("`llm.model` is empty → set the model identifier".into());
        }
        if llm.api_key.is_empty() {
            errors
                .push("`llm.api_key` is empty → set it (or its ${ENV}/keychain: reference)".into());
        }
        if !(0.0..=1.0).contains(&llm.confidence_threshold) {
            errors.push(format!(
                "`llm.confidence_threshold` is {} → use a value between 0.0 and 1.0",
                llm.confidence_threshold
            ));
        }
    }

    for group in &config.channel_groups {
        if group.prefix.is_empty() {
            errors.push(
                "a `[[channel_groups]]` entry has an empty `prefix` → set the channel-name \
                 prefix it should match"
                    .into(),
            );
        }
        if group.repos.is_empty() {
            errors.push(format!(
                "`[[channel_groups]]` (prefix `{}`) has an empty `repos` list → list the \
                 candidate repositories that prefix should narrow to",
                group.prefix
            ));
        }
        // With no explicit `[[repos]]` the candidates are not known until
        // `initialize` supplies them — the reference check runs there.
        if names.is_empty() {
            continue;
        }
        for repo in &group.repos {
            if !names.contains(&repo.as_str()) {
                errors.push(format!(
                    "`[[channel_groups]]` (prefix `{}`) references repo `{repo}` which is not \
                     declared in `[[repos]]` → add it to `[[repos]]` or fix the name",
                    group.prefix
                ));
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(json: serde_json::Value) -> SlackConfig {
        serde_json::from_value(json).unwrap()
    }

    fn minimal() -> serde_json::Value {
        json!({
            "app_token": "xapp-1-A1-token",
            "user_token": "xoxp-user-token",
            "target_user_id": "U012345",
            "repos": [{ "name": "web-app" }]
        })
    }

    #[test]
    fn minimal_config_applies_defaults() {
        let cfg = parse(minimal());
        assert_eq!(cfg.thread_context_limit, 6);
        assert_eq!(cfg.source_name, "slack");
        assert_eq!(cfg.api_url, "https://slack.com/api");
        assert_eq!(cfg.max_retries, 3);
        assert!(cfg.reply_style.is_none());
        assert!(cfg.llm.is_none());
        assert!(cfg.channel_groups.is_empty());
        assert!(static_config_errors(&cfg).is_empty());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let mut value = minimal();
        value["typo_field"] = json!(true);
        let err = serde_json::from_value::<SlackConfig>(value).unwrap_err();
        assert!(err.to_string().contains("typo_field"), "got {err}");
    }

    #[test]
    fn llm_confidence_threshold_defaults() {
        let mut value = minimal();
        value["llm"] = json!({ "base_url": "https://llm", "model": "m", "api_key": "k" });
        let cfg = parse(value);
        assert!((cfg.llm.unwrap().confidence_threshold - 0.6).abs() < f64::EPSILON);
    }

    #[test]
    fn token_prefixes_are_checked() {
        let mut value = minimal();
        value["app_token"] = json!("xoxb-wrong");
        value["user_token"] = json!("xoxb-bot-token");
        let errors = static_config_errors(&parse(value));
        assert!(errors.iter().any(|e| e.contains("xapp-")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("xoxp-")), "{errors:?}");
    }

    #[test]
    fn multiple_repos_without_llm_defer_to_initialize() {
        // Offline validation cannot know whether initialize will supply the
        // orchestrator's `[llm]` as the default (#119) — the "more than one
        // candidate needs an `[llm]`" check fires at initialize instead.
        let mut value = minimal();
        value["repos"] = json!([{ "name": "a" }, { "name": "b" }]);
        assert!(static_config_errors(&parse(value)).is_empty());
    }

    #[test]
    fn single_repo_needs_no_llm() {
        let cfg = parse(minimal());
        assert!(static_config_errors(&cfg).is_empty());
    }

    #[test]
    fn empty_repos_defer_to_initialize_but_duplicates_are_flagged() {
        // Empty is legal offline: the orchestrator supplies its
        // repositories at initialize (#109), where the merged list is
        // checked — including channel_groups references.
        let mut value = minimal();
        value["repos"] = json!([]);
        value["channel_groups"] = json!([{ "prefix": "dev-", "repos": ["ghost"] }]);
        assert!(static_config_errors(&parse(value)).is_empty());

        let mut value = minimal();
        value["repos"] = json!([{ "name": "web-app" }, { "name": "web-app" }]);
        let errors = static_config_errors(&parse(value));
        assert!(
            errors.iter().any(|e| e.contains("more than once")),
            "{errors:?}"
        );
    }

    #[test]
    fn channel_group_referencing_unknown_repo_is_flagged() {
        let mut value = minimal();
        value["channel_groups"] = json!([{ "prefix": "dev-", "repos": ["web-app", "ghost"] }]);
        let errors = static_config_errors(&parse(value));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("ghost"), "{errors:?}");
    }

    #[test]
    fn out_of_range_confidence_is_flagged() {
        let mut value = minimal();
        value["llm"] = json!({
            "base_url": "https://llm", "model": "m", "api_key": "k",
            "confidence_threshold": 1.5
        });
        let errors = static_config_errors(&parse(value));
        assert!(
            errors.iter().any(|e| e.contains("confidence_threshold")),
            "{errors:?}"
        );
    }
}
