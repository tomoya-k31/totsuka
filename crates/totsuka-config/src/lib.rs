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

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("Expand error: {0}")]
    Expand(#[from] ExpandError),
    #[error("Validation errors: {0:?}")]
    Validation(Vec<ValidationError>),
}

impl Config {
    /// Load and fully process a TOML config file:
    ///
    /// 1. Read the file from `path`.
    /// 2. Parse into a `toml::Value` tree.
    /// 3. Apply `TOTSUKA__*` environment variable overrides.
    /// 4. Extract the optional top-level `[vars]` section and use it to expand
    ///    `${var}` / `${env:VAR}` references throughout all string leaves (lenient:
    ///    undefined refs are left as-is; only cycles are errors).
    /// 5. Deserialize into `Config`.
    /// 6. Validate structural constraints.
    pub fn load(path: impl AsRef<Path>) -> Result<Config, LoadError> {
        let content = std::fs::read_to_string(path.as_ref())?;

        // Parse into generic Value so we can manipulate before deserializing
        let mut val: toml::Value = toml::from_str(&content)?;

        // Apply TOTSUKA__SECTION__KEY env overrides
        val = apply_env_overrides(val, std::env::vars());

        // Extract optional top-level [vars] table before expansion
        let vars: HashMap<String, String> = val
            .as_table_mut()
            .and_then(|t| t.remove("vars"))
            .and_then(|v| v.try_into().ok())
            .unwrap_or_default();

        // Expand ${var} / ${env:VAR} in all string leaves (lenient)
        expand_toml_value(&mut val, &vars)?;

        // Re-serialize to TOML string so we can use toml::from_str for final deserialization
        // (avoids any lifetime/trait-object issues with Value as Deserializer)
        let expanded_toml =
            toml::to_string(&val).expect("a valid toml::Value always re-serializes");
        let cfg: Config = toml::from_str(&expanded_toml)?;

        // Structural validation
        cfg.validate().map_err(LoadError::Validation)?;

        Ok(cfg)
    }
}
