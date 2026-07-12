//! Configuration precedence resolution (F-66).
//!
//! Effective values are resolved across four layers, highest priority first:
//!
//! 1. CLI flags
//! 2. environment variables (`TOTSUKA_*`)
//! 3. `plugins/{name}.toml` (plugin-specific file)
//! 4. `config.toml` defaults
//!
//! The precedence is fixed here (and pinned by unit tests) so later tasks
//! wiring CLI/env into it cannot accidentally reorder the layers.

use std::collections::HashMap;

/// Prefix for environment variable overrides.
pub const ENV_PREFIX: &str = "TOTSUKA_";

/// One resolvable key/value scope, layered by precedence.
///
/// Each layer maps a key to a string value; [`ConfigResolver::get`] returns the
/// first layer (in priority order) that defines the key.
#[derive(Debug, Default, Clone)]
pub struct ConfigResolver {
    cli: HashMap<String, String>,
    env: HashMap<String, String>,
    plugin_file: HashMap<String, String>,
    config_default: HashMap<String, String>,
}

impl ConfigResolver {
    /// Build a resolver from the four layers.
    pub fn new(
        cli: HashMap<String, String>,
        env: HashMap<String, String>,
        plugin_file: HashMap<String, String>,
        config_default: HashMap<String, String>,
    ) -> Self {
        Self {
            cli,
            env,
            plugin_file,
            config_default,
        }
    }

    /// Build the environment layer from a `TOTSUKA_*` snapshot.
    ///
    /// `TOTSUKA_MAX_CONCURRENCY=5` becomes the key `max_concurrency`. Keys are
    /// lowercased and the prefix stripped so they line up with config keys.
    pub fn env_layer_from<I>(vars: I) -> HashMap<String, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        vars.into_iter()
            .filter_map(|(k, v)| {
                k.strip_prefix(ENV_PREFIX)
                    .map(|key| (key.to_ascii_lowercase(), v))
            })
            .collect()
    }

    /// Resolve `key` across the layers, highest precedence first.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.cli
            .get(key)
            .or_else(|| self.env.get(key))
            .or_else(|| self.plugin_file.get(key))
            .or_else(|| self.config_default.get(key))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn cli_beats_all_lower_layers() {
        let r = ConfigResolver::new(
            map(&[("log_level", "trace")]),
            map(&[("log_level", "debug")]),
            map(&[("log_level", "info")]),
            map(&[("log_level", "warn")]),
        );
        assert_eq!(r.get("log_level"), Some("trace"));
    }

    #[test]
    fn falls_through_layers_in_order() {
        // env > plugin_file > config_default when CLI is absent.
        let r = ConfigResolver::new(
            HashMap::new(),
            map(&[("a", "env")]),
            map(&[("a", "plugin"), ("b", "plugin")]),
            map(&[("a", "cfg"), ("b", "cfg"), ("c", "cfg")]),
        );
        assert_eq!(r.get("a"), Some("env"));
        assert_eq!(r.get("b"), Some("plugin"));
        assert_eq!(r.get("c"), Some("cfg"));
        assert_eq!(r.get("missing"), None);
    }

    #[test]
    fn env_layer_strips_prefix_and_lowercases() {
        let layer = ConfigResolver::env_layer_from([
            ("TOTSUKA_MAX_CONCURRENCY".to_string(), "5".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ]);
        assert_eq!(layer.get("max_concurrency"), Some(&"5".to_string()));
        assert!(!layer.contains_key("path"));
    }
}
