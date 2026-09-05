//! The `[discord]` table of `config.toml`, as it arrives at `initialize`
//! with secret references already resolved (F-65, #554).

use serde::Deserialize;

/// Default Discord REST base.
fn default_api_url() -> String {
    "https://discord.com/api/v10".to_string()
}

/// Default source instance name.
fn default_source_name() -> String {
    "discord".to_string()
}

/// Default retry attempts for retryable REST failures.
fn default_max_retries() -> u32 {
    3
}

/// This plugin's settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    /// Bot token (`Bot <token>` is added by the transport). Required: Discord
    /// has no other supported identity for an app — automating a human
    /// account is forbidden by its Terms of Service, so there is deliberately
    /// no user-token option here.
    pub bot_token: String,
    /// The operator's own Discord user id (a snowflake). The author gate
    /// compares posts against this, and it is what makes "only my own posts
    /// trigger" the default.
    pub operator_user_id: String,
    /// REST base URL, overridable for tests.
    #[serde(default = "default_api_url")]
    pub api_url: String,
    /// This source instance's name, as used in `Task.source`.
    #[serde(default = "default_source_name")]
    pub source_name: String,
    /// Max retry attempts for retryable REST failures.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// Most messages the startup backfill recovers per watched channel.
    /// Omitted means [`plugin_sdk::watch::DEFAULT_BACKFILL_COUNT`].
    #[serde(default)]
    pub watch_backfill_limit: Option<u32>,
    /// How old a missed post may be and still be recovered, in hours.
    /// Omitted means [`plugin_sdk::watch::DEFAULT_BACKFILL_MAX_AGE_HOURS`].
    #[serde(default)]
    pub watch_backfill_max_age_hours: Option<u64>,
}

/// Offline consistency checks, shared by `config/validate` and `initialize`.
pub fn static_config_errors(config: &DiscordConfig) -> Vec<String> {
    let mut errors = Vec::new();
    if config.bot_token.trim().is_empty() {
        errors.push(
            "`bot_token` is empty → set it to the app's Bot Token (Developer Portal → Bot), \
             ideally through a secret reference"
                .into(),
        );
    }
    if config.operator_user_id.trim().is_empty() {
        errors.push(
            "`operator_user_id` is empty → set it to your own Discord user id, which is what \
             decides whose posts may start a task. Enable Developer Mode in Discord and use \
             \"Copy User ID\" on your own name"
                .into(),
        );
    } else if !config.operator_user_id.chars().all(|c| c.is_ascii_digit()) {
        // Copying the *username* instead of the id is the mistake this
        // catches: it would simply never match, and a watch that matches
        // nobody looks identical to a watch nobody used.
        errors.push(format!(
            "`operator_user_id` is `{}`, which is not a Discord user id → ids are all digits \
             (a snowflake). Enable Developer Mode and use \"Copy User ID\", not the username",
            config.operator_user_id
        ));
    }
    if config.source_name.trim().is_empty() {
        errors.push("`source_name` is empty → leave it out for the default `discord`".into());
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> Result<DiscordConfig, serde_json::Error> {
        serde_json::from_value(value)
    }

    fn minimal() -> serde_json::Value {
        json!({ "bot_token": "tok", "operator_user_id": "123456789012345678" })
    }

    #[test]
    fn the_minimal_config_parses_and_fills_defaults() {
        let config = parse(minimal()).unwrap();
        assert_eq!(config.api_url, "https://discord.com/api/v10");
        assert_eq!(config.source_name, "discord");
        assert_eq!(config.max_retries, 3);
        assert!(config.watch_backfill_limit.is_none());
        assert!(static_config_errors(&config).is_empty());
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_ignored() {
        let mut value = minimal();
        value["bot_tokn"] = json!("typo");
        assert!(parse(value).is_err());
    }

    /// A username where an id belongs matches nobody, and a watch that
    /// matches nobody is indistinguishable from one nobody used.
    #[test]
    fn a_username_in_place_of_the_operator_id_is_refused() {
        let mut value = minimal();
        value["operator_user_id"] = json!("tomoya");
        let errors = static_config_errors(&parse(value).unwrap());
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("Copy User ID"), "{}", errors[0]);
    }

    #[test]
    fn empty_required_values_are_each_reported() {
        let value = json!({ "bot_token": "  ", "operator_user_id": "" });
        let errors = static_config_errors(&parse(value).unwrap());
        assert_eq!(errors.len(), 2, "got {errors:?}");
    }
}
