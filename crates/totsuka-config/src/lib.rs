#![forbid(unsafe_code)]
pub mod env_override;
pub mod expand;
pub mod schema;
pub mod validate;
pub use env_override::apply_env_overrides;
pub use expand::{expand_toml_value, expand_vars, ExpandError};
pub use schema::Config;
pub use validate::ValidationError;

use std::collections::HashMap;
use std::path::Path;

/// Errors from `Config::load` — covers every stage of the pipeline.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("variable expansion: {0}")]
    Expand(#[from] ExpandError),
    #[error("validation: {0:?}")]
    Validation(Vec<ValidationError>),
}

impl Config {
    /// Single entry point that every bin should call:
    ///   read file → parse TOML → apply `TOTSUKA__*` env override
    ///   → expand `${var}` / `${env:NAME}` in string leaves → re-parse → validate
    ///
    /// Variable definitions live in a top-level `[vars]` table (read before
    /// the overlay step so env-driven values can still reference them); the
    /// `[vars]` table is stripped from the final `Config`.
    pub fn load(path: impl AsRef<Path>) -> Result<Config, LoadError> {
        let raw = std::fs::read_to_string(path.as_ref())?;
        let parsed: toml::Value = toml::from_str(&raw)?;

        // 1. Apply env overrides (TOTSUKA__SECTION__KEY=value).
        let overlaid = apply_env_overrides(parsed, std::env::vars());

        // 2. Collect [vars] block as the expansion lookup map. Strip from the
        //    final tree so it never reaches Config deserialization.
        let (mut tree, vars) = take_vars_table(overlaid);

        // 3. Expand every string leaf in place.
        expand_toml_value(&mut tree, &vars, &|name| std::env::var(name).ok())?;

        // 4. Deserialize into the typed Config.
        let cfg: Config = tree.try_into()?;

        // 5. Validate.
        cfg.validate().map_err(LoadError::Validation)?;
        Ok(cfg)
    }

    /// In-process variant of `Config::load`: same pipeline (env override →
    /// [vars] take → expand → deserialize → validate) but reading a string
    /// instead of a file. Primarily for tests that need to construct a
    /// Config from a TOML literal.
    pub fn from_toml_str(raw: &str) -> Result<Config, LoadError> {
        let parsed: toml::Value = toml::from_str(raw)?;

        // 1. Apply env overrides (TOTSUKA__SECTION__KEY=value).
        let overlaid = apply_env_overrides(parsed, std::env::vars());

        // 2. Collect [vars] block as the expansion lookup map. Strip from the
        //    final tree so it never reaches Config deserialization.
        let (mut tree, vars) = take_vars_table(overlaid);

        // 3. Expand every string leaf in place.
        expand_toml_value(&mut tree, &vars, &|name| std::env::var(name).ok())?;

        // 4. Deserialize into the typed Config.
        let cfg: Config = tree.try_into()?;

        // 5. Validate.
        cfg.validate().map_err(LoadError::Validation)?;
        Ok(cfg)
    }
}

/// Split off a top-level `[vars]` table. Returns `(rest, vars_map)`.
/// Non-string leaves under `[vars]` are stringified via `to_string()`.
fn take_vars_table(mut root: toml::Value) -> (toml::Value, HashMap<String, String>) {
    let mut vars = HashMap::new();
    if let Some(t) = root.as_table_mut() {
        if let Some(toml::Value::Table(vt)) = t.remove("vars") {
            for (k, v) in vt {
                let s = match v {
                    toml::Value::String(s) => s,
                    other => other.to_string(),
                };
                vars.insert(k, s);
            }
        }
    }
    (root, vars)
}
