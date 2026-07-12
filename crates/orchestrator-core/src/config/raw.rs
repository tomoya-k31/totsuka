//! Uninterpreted plugin-specific configuration (`plugins/{name}.toml`, F-64).
//!
//! The Orchestrator does not interpret these settings; it holds them as a raw
//! TOML table and converts them to JSON to pass as JSON-RPC `initialize`
//! params (wired up in #51). Secret references inside are resolved by the
//! Orchestrator before dispatch (F-65), using
//! [`resolve`](crate::config::resolve).

use super::schema::ConfigError;

/// A plugin's own `plugins/{name}.toml`, held verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginRawConfig(toml::Table);

impl PluginRawConfig {
    /// Parse a `plugins/{name}.toml` document without interpreting it.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Ok(Self(toml::from_str(s)?))
    }

    /// Borrow the underlying TOML table.
    pub fn as_table(&self) -> &toml::Table {
        &self.0
    }

    /// Convert to a JSON object for `initialize` params (F-64).
    pub fn to_json(&self) -> Result<serde_json::Value, ConfigError> {
        Ok(serde_json::to_value(&self.0)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_and_converts_plugin_config() {
        // The §4.6 plugins/herdr.toml example.
        let raw = PluginRawConfig::from_toml_str(
            r#"
socket_path = "${XDG_RUNTIME_DIR}/herdr.sock"
design_preview = "side_pane"
"#,
        )
        .unwrap();

        // Held uninterpreted (placeholder not yet expanded).
        assert_eq!(
            raw.as_table().get("socket_path").unwrap().as_str(),
            Some("${XDG_RUNTIME_DIR}/herdr.sock")
        );

        let json = raw.to_json().unwrap();
        assert_eq!(json["design_preview"], serde_json::json!("side_pane"));
    }
}
