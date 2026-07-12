//! Declarative config editing helpers (F-57).
//!
//! `plugin enable/disable` only flips `[plugins.{name}] enabled`. Using
//! `toml_edit` preserves comments, ordering, and whitespace, so the result is
//! identical to a hand-edit — `config.toml` stays the single source of truth
//! (F-56).

use toml_edit::{DocumentMut, Item, Table, value};

/// Errors from editing `config.toml`.
#[derive(Debug, thiserror::Error)]
pub enum EditError {
    /// The document could not be parsed.
    #[error("failed to parse config.toml for editing: {0}")]
    Parse(#[from] toml_edit::TomlError),
    /// A path expected to be a table was something else.
    #[error("`{0}` is not a table in config.toml → fix it by hand")]
    NotATable(String),
}

/// Set `[plugins.{name}] enabled = <enabled>`, preserving all other formatting.
///
/// Creates the `[plugins.{name}]` table if absent. When the section is being
/// created (it has no `kind` yet) and `kind_if_new` is provided, `kind` is also
/// written — otherwise a brand-new section would be missing the required `kind`
/// field and make `config.toml` fail to load. Returns the edited document.
pub fn set_plugin_enabled(
    config_toml: &str,
    name: &str,
    enabled: bool,
    kind_if_new: Option<&str>,
) -> Result<String, EditError> {
    let mut doc: DocumentMut = config_toml.parse()?;

    let plugins = doc
        .as_table_mut()
        .entry("plugins")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::NotATable("plugins".to_string()))?;
    // Keep `[plugins.foo]` idiomatic (no bare `[plugins]` header when it only
    // holds subtables).
    plugins.set_implicit(true);

    let section = plugins
        .entry(name)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| EditError::NotATable(format!("plugins.{name}")))?;

    // Fill in `kind` only when the section lacks it, so a newly-created section
    // is a valid `PluginConfig` and existing sections are left untouched.
    if !section.contains_key("kind")
        && let Some(kind) = kind_if_new
    {
        section["kind"] = value(kind);
    }
    section["enabled"] = value(enabled);

    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flips_enabled_preserving_comments_and_layout() {
        let original = r#"# totsuka config
version = 1

[plugins.herdr]  # our agent
enabled = true
kind = "agent_ide"   # keep this comment
max_concurrency = 3
"#;
        let disabled = set_plugin_enabled(original, "herdr", false, None).unwrap();
        // enabled flipped...
        assert!(disabled.contains("enabled = false"));
        // ...and everything else preserved verbatim.
        assert!(disabled.contains("# totsuka config"));
        assert!(disabled.contains("[plugins.herdr]  # our agent"));
        assert!(disabled.contains("kind = \"agent_ide\"   # keep this comment"));
        assert!(disabled.contains("max_concurrency = 3"));

        // Round-trips back to enabled.
        let reenabled = set_plugin_enabled(&disabled, "herdr", true, None).unwrap();
        assert!(reenabled.contains("enabled = true"));
        assert!(reenabled.contains("# keep this comment"));
    }

    #[test]
    fn creates_section_with_kind_and_stays_schema_valid() {
        let out = set_plugin_enabled("version = 1\n", "notion", true, Some("task_source")).unwrap();
        assert!(out.contains("[plugins.notion]"));
        assert!(out.contains("enabled = true"));
        assert!(out.contains("kind = \"task_source\""));
        // No stray bare `[plugins]` header.
        assert!(!out.contains("[plugins]\n"));
        assert!(out.contains("version = 1"));
        // The result must load through the real schema (kind is required).
        crate::config::RootConfig::from_toml_str(&out).expect("new section must be schema-valid");
    }

    #[test]
    fn existing_section_kind_is_untouched() {
        let out = set_plugin_enabled(
            "[plugins.x]\nenabled = true\nkind = \"notifier\"\n",
            "x",
            false,
            Some("task_source"), // ignored: section already has a kind
        )
        .unwrap();
        crate::config::RootConfig::from_toml_str(&out).unwrap();
        assert!(out.contains("enabled = false"));
        assert!(
            out.contains("kind = \"notifier\""),
            "existing kind must not change"
        );
    }
}
