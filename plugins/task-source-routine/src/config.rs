//! The `[routine]` table of `config.toml`, as it arrives at `initialize`
//! (F-65, #554). The routines themselves are files, not config (ADR-0117).

use serde::Deserialize;
use time::Duration;

use crate::routine::parse_duration;

/// How far back a missed slot is run when nothing says otherwise.
pub const DEFAULT_CATCHUP_WINDOW: &str = "24h";

fn default_catchup_window() -> String {
    DEFAULT_CATCHUP_WINDOW.to_string()
}

fn default_source_name() -> String {
    "routine".to_string()
}

/// This plugin's settings.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[schemars(extend("x-category" = "Routines"))]
#[serde(deny_unknown_fields)]
pub struct RoutineConfig {
    /// How far back a slot missed while the machine or `run` was down is
    /// still run, once, when it comes back. A routine's own `catchup_window`
    /// wins.
    #[serde(default = "default_catchup_window")]
    #[schemars(extend(
        "x-title" = "Catch-up window",
        "x-placeholder" = "24h",
        "x-help" = "How far back a run missed while the machine was off is still made up, once. 0 runs only on time; also 90m, 6h, 7d."
    ))]
    pub catchup_window: String,
    /// This source instance's name, as used in `Task.source`.
    #[serde(default = "default_source_name")]
    #[schemars(extend(
        "x-title" = "Source name",
        "x-help" = "This source's name on tasks. Change it only to run two of this plugin."
    ))]
    pub source_name: String,
}

impl RoutineConfig {
    /// [`catchup_window`](Self::catchup_window), parsed.
    pub fn window(&self) -> Result<Duration, String> {
        parse_duration(&self.catchup_window).map_err(|e| format!("`catchup_window`: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_and_typos() {
        let config: RoutineConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(config.window(), Ok(Duration::hours(24)));
        assert_eq!(config.source_name, "routine");
        let bad: RoutineConfig =
            serde_json::from_value(json!({ "catchup_window": "a day" })).unwrap();
        assert!(bad.window().unwrap_err().contains("catchup_window"));
        assert!(serde_json::from_value::<RoutineConfig>(json!({ "window": "1h" })).is_err());
    }
}
