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
//! from `initialize` with the keys it actually reads.

use plugin_protocol::methods::WorkflowInfo;

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workflow(name: &str, trigger: serde_json::Value) -> WorkflowInfo {
        WorkflowInfo {
            workflow: name.to_string(),
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
}
