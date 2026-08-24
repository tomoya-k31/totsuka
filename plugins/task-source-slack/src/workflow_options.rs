//! The `[[workflows]]` keys this plugin owns (#554).
//!
//! Written flat in `config.toml`, next to the Orchestrator's own keys:
//!
//! ```toml
//! [[workflows]]
//! name = "slack-books"
//! source = "slack"
//! agent = "herdr"
//! profile = "triage"
//! publish = "direct"
//! ```
//!
//! The Orchestrator cannot tell whose `publish` is — a workflow names a source
//! *and* an agent — so it hands every non-core key to both and asks. What this
//! module answers is [`claims`]: the `(workflow, key)` pairs this plugin
//! actually consumes. **Claiming a key it ignored would turn a typo into
//! silence**, which is the failure the handshake exists to remove, so the list
//! is derived from the same match that reads the values.
//!
//! Until 0.6.0 this key was read by the Orchestrator and sent as
//! `ResultPublishParams.delivery` ([ADR-0057], #548). Nothing about the policy
//! moved — which workflows may skip approval is still the operator's choice,
//! still written in `config.toml` — only who reads it.
//!
//! [ADR-0057]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0057-per-workflow-publish-and-cleanup.md

use std::collections::HashMap;

use plugin_protocol::methods::{WorkflowInfo, WorkflowOption};

/// The key this plugin claims on a workflow.
const PUBLISH: &str = "publish";

/// How a published result reaches the human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Delivery {
    /// Present it for approval before anything is visible (the default, and
    /// the only behaviour before #548).
    #[default]
    Draft,
    /// Post it immediately, no approval step.
    Direct,
}

impl Delivery {
    /// Parse a `publish` value. An unreadable one resolves to
    /// [`Draft`](Self::Draft) **and reports an error**: the two modes differ in
    /// whether a human gate is skipped, so a value this build cannot read must
    /// not be allowed to skip it — and it must not pass silently either.
    fn parse(value: &serde_json::Value) -> Result<Self, String> {
        match value.as_str() {
            Some("draft") => Ok(Self::Draft),
            Some("direct") => Ok(Self::Direct),
            _ => Err(format!(
                "`publish` must be \"draft\" or \"direct\", not {value} → \
                 leave it out for the approval flow, or write \"direct\" to post immediately"
            )),
        }
    }
}

/// The per-workflow settings resolved from `initialize`.
#[derive(Debug, Clone, Default)]
pub struct WorkflowOptions {
    /// workflow name → how its published result is delivered. A workflow
    /// absent here uses [`Delivery::Draft`].
    delivery: HashMap<String, Delivery>,
}

impl WorkflowOptions {
    /// Resolve every workflow's options, collecting the problems rather than
    /// stopping at the first: an operator fixing config wants the whole list.
    pub fn resolve(workflows: &[WorkflowInfo]) -> (Self, Vec<String>) {
        let mut delivery = HashMap::new();
        let mut errors = Vec::new();
        for wf in workflows {
            let Some(value) = wf.options.get(PUBLISH) else {
                continue;
            };
            match Delivery::parse(value) {
                Ok(d) => {
                    delivery.insert(wf.workflow.clone(), d);
                }
                Err(e) => errors.push(format!("workflow `{}`: {e}", wf.workflow)),
            }
        }
        (Self { delivery }, errors)
    }

    /// How `workflow`'s result is delivered. Unknown — including a workflow
    /// that never set the key — is the approval flow.
    pub fn delivery(&self, workflow: &str) -> Delivery {
        self.delivery.get(workflow).copied().unwrap_or_default()
    }
}

/// The `(workflow, key)` pairs this plugin claims, for `InitializeResult`.
///
/// Derived from the same lookup [`WorkflowOptions::resolve`] does, so a key
/// that stops being read stops being claimed — and a workflow that never wrote
/// `publish` yields nothing, because there is nothing there to own.
pub fn claims(workflows: &[WorkflowInfo]) -> Vec<WorkflowOption> {
    workflows
        .iter()
        .filter(|wf| wf.options.contains_key(PUBLISH))
        .map(|wf| WorkflowOption {
            workflow: wf.workflow.clone(),
            key: PUBLISH.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn workflow(name: &str, options: serde_json::Value) -> WorkflowInfo {
        WorkflowInfo {
            workflow: name.to_string(),
            trigger: json!({}),
            options: options.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn publish_resolves_and_defaults_to_draft() {
        let (options, errors) = WorkflowOptions::resolve(&[
            workflow("books", json!({ "publish": "direct" })),
            workflow("reply", json!({})),
        ]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(options.delivery("books"), Delivery::Direct);
        assert_eq!(options.delivery("reply"), Delivery::Draft);
        // A workflow this plugin was never told about is the same answer, not
        // a panic: `result/publish` can arrive for a task whose workflow was
        // renamed out of config between submit and completion.
        assert_eq!(options.delivery("gone"), Delivery::Draft);
    }

    /// An unreadable value must not pass silently. It resolves to the safe
    /// mode *and* is reported — the alternative is an operator who wrote
    /// `publish = "diretc"` believing they turned the gate off.
    #[test]
    fn an_unreadable_publish_value_is_reported() {
        let (options, errors) =
            WorkflowOptions::resolve(&[workflow("books", json!({ "publish": "diretc" }))]);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("books") && errors[0].contains("diretc"),
            "{errors:?}"
        );
        assert_eq!(options.delivery("books"), Delivery::Draft);
    }

    /// Claiming is scoped to the workflows that actually wrote the key. A
    /// blanket claim would make `publsh` — nobody's key — look owned, and the
    /// Orchestrator would stop reporting it.
    #[test]
    fn only_workflows_that_wrote_publish_are_claimed() {
        let claimed = claims(&[
            workflow("books", json!({ "publish": "direct" })),
            workflow("reply", json!({ "publsh": "direct" })),
            workflow("bare", json!({})),
        ]);
        assert_eq!(
            claimed,
            vec![WorkflowOption {
                workflow: "books".into(),
                key: "publish".into(),
            }]
        );
    }
}
