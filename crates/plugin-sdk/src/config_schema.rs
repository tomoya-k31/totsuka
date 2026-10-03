//! `config/schema` answers (protocol 0.7.7, ADR-0109): a plugin's config table
//! as the JSON Schema the menu bar app's settings window renders.
//!
//! Derive `schemars::JsonSchema` on the struct `initialize` deserializes, put
//! `x-title` / `x-help` (English strings) on every field with
//! `#[schemars(extend(...))]` (and `x-secret = true` on secret references),
//! then answer with [`of`]. Deriving from the struct serde reads is what keeps
//! the schema from describing a key the plugin does not accept.
//!
//! A task source also describes what it reads on `[[projects]]` and
//! `[[workflows]]` entries (`project` / `workflow` in the answer), so the
//! window shows those keys only for entries that use this source. A project's
//! options are a struct too ([`schema_for`]); a trigger is read key by key, so
//! its schema is written with [`workflow`] and the helpers below, and a test
//! next to the source's `TRIGGER_KEYS` keeps the two in step.

use plugin_protocol::methods::ConfigSchemaResult;
use serde_json::{Map, Value, json};

/// The `config/schema` answer for config type `T`, with no `project` /
/// `workflow` schema (set those with struct update syntax).
pub fn of<T: schemars::JsonSchema>() -> ConfigSchemaResult {
    ConfigSchemaResult {
        schema: schema_for::<T>(),
        project: None,
        workflow: None,
    }
}

/// The JSON Schema of `T`: every subschema inline (the protocol forbids
/// `$ref`), doc-comment `title` / `description` removed (they are written for
/// developers; the settings window reads `x-help`).
pub fn schema_for<T: schemars::JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = generator.into_root_schema_for::<T>().to_value();
    strip_docs(&mut schema, false);
    schema
}

/// A task source's `workflow` schema: its `trigger` table (the given
/// properties) plus any flat workflow option it claims.
pub fn workflow(trigger: Map<String, Value>, options: Map<String, Value>) -> Value {
    let mut properties = options;
    properties.insert(
        "trigger".into(),
        json!({
            "type": "object",
            "properties": trigger,
            "x-title": "Trigger",
            "x-help": "Which tasks this workflow picks up.",
        }),
    );
    json!({ "type": "object", "properties": properties })
}

/// The property names of the object schema at `pointer` in `schema`, sorted —
/// for a source's own test that its trigger schema names exactly the keys it
/// reads (`keys(&workflow, "/properties/trigger")` against `TRIGGER_KEYS`).
pub fn keys(schema: &Value, pointer: &str) -> Vec<String> {
    let mut keys: Vec<String> = schema
        .pointer(pointer)
        .and_then(|node| node["properties"].as_object())
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort_unstable();
    keys
}

/// An object schema with no keys — the `project` answer of a source that
/// reads nothing on its `[[projects]]` entries, so a key left there shows as
/// unused rather than as something the window cannot describe.
pub fn no_keys() -> Value {
    json!({ "type": "object", "properties": {} })
}

/// A property with a title and help on top of `schema`.
pub fn field(schema: Value, title: &str, help: &str) -> Value {
    let mut schema = schema;
    schema["x-title"] = json!(title);
    schema["x-help"] = json!(help);
    schema
}

/// A value written as one string or a list of them — what
/// [`one_or_many`](crate::trigger::one_or_many) reads.
pub fn one_or_many(title: &str, help: &str) -> Value {
    field(
        json!({ "anyOf": [
            { "type": "string" },
            { "type": "array", "items": { "type": "string" } },
        ] }),
        title,
        help,
    )
}

/// `trigger.assignee`, as [`AssigneeFilter`](crate::assignee::AssigneeFilter)
/// reads it.
pub fn assignee() -> Value {
    let mut schema = one_or_many(
        "Assignee",
        "Who may hold the task: logins, @me (you) and @none (nobody); @any alone admits anyone.",
    );
    schema["x-placeholder"] = json!("@me, @none");
    schema
}

/// `trigger.exclude` (ADR-0091): the given keys, any one of which drops a
/// task.
pub fn exclude(properties: Map<String, Value>) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "x-title": "Exclude",
        "x-help": "Skip a task that matches any one of these.",
    })
}

/// The keys of a channel watch, as [`WatchTrigger`](crate::watch::WatchTrigger)
/// reads them.
pub fn watch_trigger() -> Map<String, Value> {
    let string = || json!({ "type": "string" });
    let list = || json!({ "type": "array", "items": { "type": "string" } });
    let mut keys = Map::new();
    keys.insert(
        "channel".into(),
        field(
            string(),
            "Channel ID",
            "The channel to watch, by ID; every post there starts this workflow.",
        ),
    );
    keys.insert(
        "channel_name".into(),
        field(
            string(),
            "Channel name",
            "The channel's name, checked against the ID at start so a rename is reported.",
        ),
    );
    keys.insert(
        "repo".into(),
        field(
            string(),
            "Repository",
            "The repository this channel's tasks belong to.",
        ),
    );
    keys.insert(
        "from".into(),
        field(
            list(),
            "Also from",
            "User IDs besides you whose posts start it.",
        ),
    );
    keys
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

/// Every property, at any depth, that lacks an `x-title` or `x-help` — for a plugin's own test, so a key added without help text fails
/// there instead of showing up in the settings window as a bare name.
pub fn missing_help(schema: &Value) -> Vec<String> {
    let mut missing = Vec::new();
    walk(schema, "", &mut missing);
    missing
}

/// Every `x-help` text that states a default value ("Default 3", "既定は 3")
/// — for a plugin's own test. The default belongs in the schema's `default`
/// (serde's, derived) or `x-placeholder`, where the settings window shows it
/// in the empty field; a sentence in the help repeats it and goes stale.
pub fn help_stating_defaults(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    collect_help(schema, &mut found);
    found
        .into_iter()
        .filter(|text| {
            [
                "Default ",
                "既定は",
                "(default)",
                "（既定）",
                ", default)",
                "、既定）",
            ]
            .iter()
            .any(|stated| text.contains(stated))
        })
        .collect()
}

fn collect_help(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "x-help" {
                    out.extend(child.as_str().map(str::to_string));
                } else {
                    collect_help(child, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_help(v, out)),
        _ => {}
    }
}

fn walk(value: &Value, path: &str, missing: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(props)) = map.get("properties") {
                for (key, prop) in props {
                    let at = format!("{path}/{key}");
                    for keyword in ["x-title", "x-help"] {
                        if !prop[keyword].as_str().is_some_and(|s| !s.is_empty()) {
                            missing.push(format!("{at}: {keyword}"));
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

    /// The shared trigger pieces carry help everywhere, state no default, and
    /// `workflow` puts the trigger under its own key beside the options.
    #[test]
    fn trigger_helpers_have_help_and_nest_under_trigger() {
        let mut trigger = watch_trigger();
        trigger.insert("assignee".into(), assignee());
        trigger.insert("label".into(), one_or_many("Label", "A label."));
        let mut excluded = Map::new();
        excluded.insert("assignee".into(), assignee());
        trigger.insert("exclude".into(), exclude(excluded));
        let mut options = Map::new();
        options.insert(
            "publish".into(),
            field(json!({ "type": "string" }), "Publish", "How."),
        );
        let schema = workflow(trigger, options);
        assert_eq!(missing_help(&schema), Vec::<String>::new());
        assert_eq!(help_stating_defaults(&schema), Vec::<String>::new());
        assert!(schema["properties"]["publish"].is_object());
        let keys = schema["properties"]["trigger"]["properties"]
            .as_object()
            .unwrap();
        for key in [
            "channel",
            "channel_name",
            "repo",
            "from",
            "assignee",
            "label",
            "exclude",
        ] {
            assert!(keys.contains_key(key), "{key}");
        }
    }

    #[derive(schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Sample {
        /// Developer docs that must not reach the settings window.
        #[schemars(extend(
            "x-title" = "Token",
            "x-help" = "The token.",
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
        assert!(help_stating_defaults(&schema).is_empty());
        let stated = serde_json::json!({"properties": {"a": {"x-help": "Default 3."}}});
        assert_eq!(
            help_stating_defaults(&stated),
            vec!["Default 3.".to_string()]
        );
    }
}
