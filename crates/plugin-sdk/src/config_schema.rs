//! `config/schema` answers (protocol 0.7.7, ADR-0109): a plugin's config table
//! as the JSON Schema the menu bar app's settings window renders.
//!
//! Derive `schemars::JsonSchema` on the struct `initialize` deserializes, put
//! `x-title` / `x-help` (`{en, ja}`) on every field with
//! `#[schemars(extend(...))]` (and `x-secret = true` on secret references),
//! then answer with [`of`]. Deriving from the struct serde reads is what keeps
//! the schema from describing a key the plugin does not accept.

use plugin_protocol::methods::ConfigSchemaResult;
use serde_json::Value;

/// The `config/schema` answer for config type `T`: every subschema inline (the
/// protocol forbids `$ref`), doc-comment `title` / `description` removed (they
/// are written for developers; the settings window reads `x-help`).
pub fn of<T: schemars::JsonSchema>() -> ConfigSchemaResult {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = generator.into_root_schema_for::<T>().to_value();
    strip_docs(&mut schema, false);
    ConfigSchemaResult { schema }
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

/// Every property, at any depth, that lacks an `x-title` or `x-help` in `en`
/// or `ja` — for a plugin's own test, so a key added without help text fails
/// there instead of showing up in the settings window as a bare name.
pub fn missing_help(schema: &Value) -> Vec<String> {
    let mut missing = Vec::new();
    walk(schema, "", &mut missing);
    missing
}

fn walk(value: &Value, path: &str, missing: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(props)) = map.get("properties") {
                for (key, prop) in props {
                    let at = format!("{path}/{key}");
                    for keyword in ["x-title", "x-help"] {
                        for lang in ["en", "ja"] {
                            if !prop[keyword][lang].as_str().is_some_and(|s| !s.is_empty()) {
                                missing.push(format!("{at}: {keyword}.{lang}"));
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
        Value::Array(items) => items.iter().for_each(|v| walk(v, path, missing)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Sample {
        /// Developer docs that must not reach the settings window.
        #[schemars(extend(
            "x-title" = {"en": "Token", "ja": "トークン"},
            "x-help" = {"en": "The token.", "ja": "トークン。"},
            "x-secret" = true
        ))]
        token: String,
        nested: Nested,
    }

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Nested {
        level: u32,
    }

    #[test]
    fn answers_inline_without_docs_and_finds_missing_help() {
        let schema = of::<Sample>().schema;
        let json = schema.to_string();
        assert!(!json.contains("$ref"), "{json}");
        assert!(!json.contains("Developer docs"), "{json}");
        assert_eq!(schema["properties"]["token"]["x-secret"], true);
        let missing = missing_help(&schema);
        assert!(
            missing.iter().any(|m| m.starts_with("/nested:")),
            "{missing:?}"
        );
        assert!(
            missing.iter().any(|m| m.starts_with("/nested/level:")),
            "{missing:?}"
        );
        assert!(
            !missing.iter().any(|m| m.starts_with("/token:")),
            "{missing:?}"
        );
    }
}
