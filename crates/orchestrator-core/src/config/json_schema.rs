//! The JSON Schema of `config.toml`'s core keys, for the menu bar app's
//! settings window (ADR-0109).
//!
//! Derived with schemars from the same structs serde reads, so it cannot
//! describe a key the loader does not accept. Each property carries the
//! settings window's extension keywords (`x-title` / `x-help` / `x-category`,
//! each `{en, ja}`, and `x-secret`); the doc comments' `description`s are
//! stripped, being written for developers.
//!
//! Plugin tables are not here: the plugins describe them through
//! `config/schema`, and `totsuka config schema` merges the two.

use serde_json::Value;

use super::schema::RootConfig;

/// The core part of the config schema: every subschema inlined (no `$ref`),
/// `title` / `description` removed.
pub fn core_schema() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = generator.into_root_schema_for::<RootConfig>().to_value();
    strip_docs(&mut schema, false);
    schema
}

/// Remove `title` / `description` from every schema object. `schemas` is
/// whether `value` is a map *of* schemas (`properties`), whose keys are field
/// names and must survive even when a field is called `title`.
fn strip_docs(value: &mut Value, schemas: bool) {
    match value {
        Value::Object(map) => {
            if !schemas {
                map.remove("title");
                map.remove("description");
            }
            for (key, child) in map.iter_mut() {
                let child_is_map = !schemas && matches!(key.as_str(), "properties" | "$defs");
                strip_docs(child, child_is_map);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| strip_docs(v, false)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every property, at any depth, has `x-title` and `x-help` in both
    /// languages — so a key added to the config without help text fails here
    /// instead of showing up in the settings window as a bare key name.
    #[test]
    fn every_property_has_bilingual_title_and_help() {
        fn walk(value: &Value, path: &str, missing: &mut Vec<String>) {
            let Value::Object(map) = value else {
                if let Value::Array(items) = value {
                    items.iter().for_each(|v| walk(v, path, missing));
                }
                return;
            };
            if let Some(Value::Object(props)) = map.get("properties") {
                for (key, prop) in props {
                    let at = format!("{path}.{key}");
                    for kw in ["x-title", "x-help"] {
                        for lang in ["en", "ja"] {
                            if !prop[kw][lang].as_str().is_some_and(|s| !s.is_empty()) {
                                missing.push(format!("{at}: {kw}.{lang}"));
                            }
                        }
                    }
                    walk(prop, &at, missing);
                }
            }
            for (key, child) in map {
                if key != "properties" {
                    walk(child, path, missing);
                }
            }
        }
        let schema = core_schema();
        let mut missing = Vec::new();
        walk(&schema, "", &mut missing);
        assert!(missing.is_empty(), "{}", missing.join("\n"));
        assert!(schema["properties"]["repositories"].is_object());
    }

    #[test]
    fn top_level_keys_have_a_category_and_secrets_are_marked() {
        let schema = core_schema();
        for (key, prop) in schema["properties"].as_object().unwrap() {
            assert!(prop["x-category"]["ja"].is_string(), "{key}");
        }
        let json = schema.to_string();
        assert!(!json.contains("\"$ref\""), "subschemas are inlined");
        assert!(
            !json.contains("\"description\""),
            "doc comments are stripped"
        );
        assert!(
            json.contains("\"x-secret\":true"),
            "llm.api_key_ref is a secret"
        );
        // Removed or plugin-owned keys are not offered.
        for gone in ["prompts", "plugin_settings"] {
            assert!(schema["properties"].get(gone).is_none(), "{gone}");
        }
    }
}
