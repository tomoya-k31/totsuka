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
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(extend("x-category" = {"en": "Discord", "ja": "Discord"}))]
#[serde(deny_unknown_fields)]
pub struct DiscordConfig {
    /// Bot token (`Bot <token>` is added by the transport). Required: Discord
    /// has no other supported identity for an app — automating a human
    /// account is forbidden by its Terms of Service, so there is deliberately
    /// no user-token option here.
    #[schemars(extend(
        "x-title" = {"en": "Bot token", "ja": "Bot トークン"},
        "x-help" = {"en": "The Discord bot's token.", "ja": "Discord の Bot のトークン。"},
        "x-secret" = true
    ))]
    pub bot_token: String,
    /// The operator's own Discord user id (a snowflake). The author gate
    /// compares posts against this, and it is what makes "only my own posts
    /// trigger" the default.
    #[schemars(extend(
        "x-title" = {"en": "Your user ID", "ja": "あなたのユーザー ID"},
        "x-help" = {"en": "Your own Discord user ID. Only your posts become tasks.", "ja": "あなた自身の Discord のユーザー ID。あなたの投稿だけがタスクになる。"}
    ))]
    pub operator_user_id: String,
    /// REST base URL, overridable for tests.
    #[serde(default = "default_api_url")]
    #[schemars(extend(
        "x-title" = {"en": "API URL", "ja": "API の URL"},
        "x-help" = {"en": "The API's base URL.", "ja": "API のベース URL。"}
    ))]
    pub api_url: String,
    /// This source instance's name, as used in `Task.source`.
    #[serde(default = "default_source_name")]
    #[schemars(extend(
        "x-title" = {"en": "Source name", "ja": "ソース名"},
        "x-help" = {"en": "This source's name on tasks. Change it only to run two of this plugin.", "ja": "タスクに付くこのソースの名前。このプラグインを 2 つ動かすときだけ変える。"}
    ))]
    pub source_name: String,
    /// Max retry attempts for retryable REST failures.
    #[serde(default = "default_max_retries")]
    #[schemars(extend(
        "x-title" = {"en": "Retries", "ja": "再試行回数"},
        "x-help" = {"en": "How many times a failed API call that can be retried is retried.", "ja": "再試行できる API の失敗を何回まで再試行するか。"}
    ))]
    pub max_retries: u32,
    /// Most messages the startup backfill recovers per watched channel.
    /// Omitted means [`plugin_sdk::watch::DEFAULT_BACKFILL_COUNT`].
    #[serde(default)]
    #[schemars(extend(
        "x-title" = {"en": "Backfill limit", "ja": "取りこぼしの回収件数"},
        "x-placeholder" = "100",
        "x-help" = {"en": "How many missed posts per watched channel are recovered on start.", "ja": "起動時に、見張っているチャンネルごとに取りこぼした投稿を何件まで回収するか。"}
    ))]
    pub watch_backfill_limit: Option<u32>,
    /// How old a missed post may be and still be recovered, in hours.
    /// Omitted means [`plugin_sdk::watch::DEFAULT_BACKFILL_MAX_AGE_HOURS`].
    #[serde(default)]
    #[schemars(extend(
        "x-title" = {"en": "Backfill age (hours)", "ja": "取りこぼしの回収期間（時間）"},
        "x-placeholder" = "24",
        "x-help" = {"en": "How old a missed post may be and still be recovered.", "ja": "取りこぼした投稿を何時間前のものまで回収するか。"}
    ))]
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

#[cfg(test)]
mod schema_tests {
    /// Every key of `[discord]`, at any depth, carries an `x-title` and `x-help`
    /// in English and Japanese for the settings window (ADR-0109).
    #[test]
    fn every_key_has_bilingual_help() {
        let schema = plugin_sdk::config_schema::of::<super::DiscordConfig>().schema;
        let missing = plugin_sdk::config_schema::missing_help(&schema);
        assert!(missing.is_empty(), "{}", missing.join("\n"));
        let stated = plugin_sdk::config_schema::help_stating_defaults(&schema);
        assert!(stated.is_empty(), "help states a default: {stated:?}");
        assert!(schema["x-category"]["ja"].is_string(), "{schema}");
    }
}
