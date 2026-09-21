//! `[[workflows]].trigger` key validation for task_source plugins (#574).
//!
//! The Orchestrator carries the trigger table verbatim (#554), so a key only
//! ever means something to the plugin that reads it. Every reader does that
//! with `.get("…")`, which means a key nobody asks for is **dropped without a
//! word** — and the failure that produces is silent in the worst direction: a
//! mistyped condition does not narrow anything, so the workflow fires on tasks
//! the operator meant to exclude.
//!
//! "Verbatim" is not quite "untouched": the Orchestrator does read
//! `status` out of the table, but only to build the column graph its
//! cycle check walks — lexically, comparing two operator-written strings
//! without acting on either. Nothing on the core side reacts to a trigger key's
//! *value*, which is why this check has to live in the plugin.
//!
//! [`unknown_trigger_keys`] turns that into a startup error. A source calls it
//! from `initialize` with the keys it actually reads, and
//! [`unknown_exclude_keys`] does the same for the `exclude` table inside it.
//!
//! `exclude` is the trigger's negation (ADR-0091): a table in the trigger's own
//! vocabulary, and a task matching **any one** of its conditions is dropped.
//! The keys outside it are ANDed and an array value is an OR, so `exclude`
//! reads as `NOT (a OR b)`. Each source decides which of its keys it can
//! negate; [`one_or_many`] reads the string-or-array values they share.

use plugin_protocol::methods::WorkflowInfo;
use serde_json::Value;

/// One message per `trigger` key that is not in `valid`, plus one per trigger
/// that is not a table at all.
///
/// `valid` is the list of keys this source reads — write it out at the call
/// site rather than deriving it, so adding a key to the parser and forgetting
/// it here fails the new key's own test rather than passing silently.
///
/// An empty `Vec` means every trigger is understood. `trigger = {}` is the
/// catch-all (#396) and is always accepted: it has no keys to be wrong about.
///
/// The message names the valid keys, which is also how an operator migrating
/// from a renamed key learns what to write instead.
pub fn unknown_trigger_keys(workflows: &[WorkflowInfo], valid: &[&str]) -> Vec<String> {
    let known = valid
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut errors = Vec::new();
    for wf in workflows {
        // `WorkflowConfig::trigger` is a `toml::Table`, so the Orchestrator
        // only ever sends an object. Saying so out loud still beats skipping:
        // a non-table here means the wire shape changed, and reporting that is
        // cheaper than the silent no-condition trigger it would otherwise be.
        let Some(table) = wf.trigger.as_object() else {
            errors.push(format!(
                "workflow `{}` has a `trigger` that is not a table ({}) → write it as \
                 `trigger = {{ … }}`",
                wf.workflow, wf.trigger
            ));
            continue;
        };
        for key in table.keys() {
            if !valid.contains(&key.as_str()) {
                errors.push(format!(
                    "workflow `{}` has an unknown `trigger` key `{key}` → this source reads \
                     {known}. An unread key is dropped, which would leave the trigger with \
                     fewer conditions than written",
                    wf.workflow
                ));
            }
        }
    }
    errors
}

/// [`unknown_trigger_keys`] for the `trigger.exclude` table (ADR-0091).
///
/// `valid` is the keys this source can negate — not always its whole trigger
/// vocabulary: notion's raw `filter` goes to the server verbatim and has no
/// negation to wrap it in. `exclude` is not in it either, so a nested
/// `exclude` is reported like any other unread key.
///
/// Only the keys are checked. What a key's value means is not: an `exclude`
/// that drops every task is the operator's to write.
pub fn unknown_exclude_keys(workflows: &[WorkflowInfo], valid: &[&str]) -> Vec<String> {
    let known = valid
        .iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut errors = Vec::new();
    for wf in workflows {
        let Some(exclude) = wf.trigger.get("exclude") else {
            continue;
        };
        let Some(table) = exclude.as_object() else {
            errors.push(format!(
                "workflow `{}` has a `trigger.exclude` that is not a table ({exclude}) → write it \
                 as a table, `exclude = {{ <key> = … }}`, keyed by {known}",
                wf.workflow
            ));
            continue;
        };
        for key in table.keys() {
            if !valid.contains(&key.as_str()) {
                errors.push(format!(
                    "workflow `{}` has an unknown `trigger.exclude` key `{key}` → this source can \
                     exclude on {known}. An unread key is dropped, which would leave the trigger \
                     excluding less than written",
                    wf.workflow
                ));
            }
        }
    }
    errors
}

/// A trigger value that is one string or an array of them, as its
/// alternatives (an array is an OR). `None` when the key is absent; anything
/// that is not a string is skipped, as the scalar keys have always done.
pub fn one_or_many(value: Option<&Value>) -> Option<Vec<String>> {
    match value? {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workflow(name: &str, trigger: serde_json::Value) -> WorkflowInfo {
        WorkflowInfo {
            workflow: name.to_string(),
            projects: vec![],
            status_writebacks: vec![],
            trigger,
            instructions_kind: None,
            task_id_prefix: None,
            options: serde_json::Map::new(),
        }
    }

    #[test]
    fn known_keys_and_the_catch_all_pass() {
        let workflows = [
            workflow("a", json!({ "status": "Todo", "label": "bug" })),
            workflow("catch-all", json!({})),
        ];
        assert!(
            unknown_trigger_keys(&workflows, &["status", "label"]).is_empty(),
            "every key is read by this source"
        );
    }

    #[test]
    fn a_typo_is_reported_and_names_the_valid_keys() {
        let workflows = [workflow("a", json!({ "project_stat": "Todo" }))];
        let errors = unknown_trigger_keys(&workflows, &["status", "label"]);
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("`project_stat`"), "got {errors:?}");
        assert!(errors[0].contains("`status`"), "got {errors:?}");
        assert!(errors[0].contains("`label`"), "got {errors:?}");
        // The reason the check exists: without it this trigger matches
        // everything rather than nothing.
        assert!(errors[0].contains("dropped"), "got {errors:?}");
    }

    #[test]
    fn every_unknown_key_is_reported_not_just_the_first() {
        let workflows = [
            workflow("a", json!({ "x": 1, "y": 2 })),
            workflow("b", json!({ "z": 3 })),
        ];
        let errors = unknown_trigger_keys(&workflows, &["status"]);
        assert_eq!(errors.len(), 3, "got {errors:?}");
    }

    #[test]
    fn a_non_table_trigger_is_reported_rather_than_skipped() {
        let workflows = [workflow("a", json!("Todo"))];
        let errors = unknown_trigger_keys(&workflows, &["status"]);
        assert_eq!(errors.len(), 1, "got {errors:?}");
        assert!(errors[0].contains("not a table"), "got {errors:?}");
    }

    #[test]
    fn exclude_keys_are_checked_like_the_trigger_itself() {
        let workflows = [
            workflow(
                "ok",
                json!({ "status": "Todo", "exclude": { "label": "waiting" } }),
            ),
            workflow("typo", json!({ "exclude": { "lable": "waiting" } })),
            workflow(
                "nested",
                json!({ "exclude": { "exclude": { "label": "x" } } }),
            ),
            workflow("scalar", json!({ "exclude": "waiting" })),
            workflow("none", json!({ "status": "Todo" })),
        ];
        let errors = unknown_exclude_keys(&workflows, &["label", "status"]);
        assert_eq!(errors.len(), 3, "got {errors:?}");
        assert!(errors[0].contains("`typo`") && errors[0].contains("`lable`"));
        assert!(errors[1].contains("`nested`") && errors[1].contains("`exclude`"));
        assert!(errors[2].contains("`scalar`") && errors[2].contains("not a table"));
        // The fix names this source's keys, not a key another source reads.
        assert!(errors[2].contains("`label`, `status`"), "got {errors:?}");
    }

    #[test]
    fn one_or_many_reads_a_string_or_an_array() {
        assert_eq!(one_or_many(None), None);
        assert_eq!(one_or_many(Some(&json!("a"))), Some(vec!["a".into()]));
        assert_eq!(
            one_or_many(Some(&json!(["a", 1, "b"]))),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(one_or_many(Some(&json!(1))), None);
    }
}
