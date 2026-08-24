//! Ownership of the plugin-defined keys written on `[[workflows]]` (#554).
//!
//! A workflow's keys are flat: `publish = "direct"` sits next to `profile` and
//! `agent`, and the Orchestrator cannot tell whose it is — the workflow names a
//! `source` **and** an `agent`, and either may define it. So it does not
//! decide. It hands the whole set to both plugins at `initialize` and asks each
//! which keys it recognises ([`WorkflowOption`]); this module turns those
//! answers into a verdict.
//!
//! The rule is **exactly one claimant**:
//!
//! - **zero** — nobody consumes the key, so writing it did nothing. That is
//!   what a typo looks like (`profil = "triage"`), and it is also what a key
//!   meant for a plugin the workflow does not name looks like. Both are worth
//!   stopping for; neither is worth guessing about.
//! - **two** — the key means one thing to the source and another to the agent.
//!   Picking one would route half the operator's intent somewhere they did not
//!   ask for and say nothing, so it is reported instead.
//!
//! # Why an unanswered plugin makes the workflow unjudgeable
//!
//! A plugin that failed to launch claims nothing, which is indistinguishable
//! on the wire from a plugin that claims nothing *on purpose*. Reading the
//! first as the second would turn every launch failure into a pile of
//! "unknown key" errors pointing at config that is perfectly fine. So a
//! workflow whose source or agent did not answer is **skipped**, and the
//! launch failure — which the caller already reports — stays the one thing
//! that needs fixing.

use std::collections::{BTreeMap, BTreeSet};

use plugin_protocol::methods::WorkflowOption;

use crate::config::RootConfig;

/// A workflow key whose ownership does not resolve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionIssue {
    /// The workflow the key was written on.
    pub workflow: String,
    /// The key, as spelled in `config.toml`.
    pub key: String,
    /// Why it does not resolve.
    pub kind: OptionIssueKind,
}

/// The two ways ownership fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionIssueKind {
    /// Neither of the workflow's plugins recognises the key.
    Unclaimed {
        /// The plugins that were asked (`source`, then `agent`).
        asked: Vec<String>,
    },
    /// Both of them do.
    Ambiguous {
        /// The plugins that claimed it.
        claimants: Vec<String>,
    },
}

impl std::fmt::Display for OptionIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            OptionIssueKind::Unclaimed { asked } => write!(
                f,
                "workflow `{}` sets `{}`, which no plugin consumes → {} \
                 {} asked and neither recognises it; check the spelling, or \
                 write it on a workflow whose plugins do",
                self.workflow,
                self.key,
                join(asked),
                if asked.len() == 1 { "was" } else { "were" },
            ),
            OptionIssueKind::Ambiguous { claimants } => write!(
                f,
                "workflow `{}` sets `{}`, which both {} claim → one key means \
                 one thing; rename it in one of the plugins or split the \
                 workflow so only one of them sees it",
                self.workflow,
                self.key,
                join(claimants),
            ),
        }
    }
}

fn join(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Check every workflow's plugin-defined keys against what the plugins
/// claimed.
///
/// `claims` maps plugin instance name → the options that plugin claimed at
/// `initialize`. **Only plugins that answered belong in it**: a name absent
/// from the map means "did not answer", and every workflow naming it is
/// skipped (see the module docs).
pub fn check_workflow_options(
    cfg: &RootConfig,
    claims: &BTreeMap<String, Vec<WorkflowOption>>,
) -> Vec<OptionIssue> {
    let mut issues = Vec::new();
    for wf in &cfg.workflows {
        if wf.options.is_empty() {
            continue;
        }
        let (Some(source_claims), Some(agent_claims)) =
            (claims.get(&wf.source), claims.get(&wf.agent))
        else {
            continue;
        };
        // The same plugin can be both the source and the agent of a workflow.
        // Deduplicate, or it would claim every key twice and every key would
        // read as ambiguous with itself.
        let asked: Vec<String> = BTreeSet::from([wf.source.clone(), wf.agent.clone()])
            .into_iter()
            .collect();
        for key in wf.options.keys() {
            let mut claimants: Vec<String> = Vec::new();
            for (plugin, claimed) in [(&wf.source, source_claims), (&wf.agent, agent_claims)] {
                let claims_it = claimed
                    .iter()
                    .any(|c| c.workflow == wf.name && &c.key == key);
                if claims_it && !claimants.contains(plugin) {
                    claimants.push(plugin.clone());
                }
            }
            let kind = match claimants.len() {
                1 => continue,
                0 => OptionIssueKind::Unclaimed {
                    asked: asked.clone(),
                },
                _ => OptionIssueKind::Ambiguous { claimants },
            };
            issues.push(OptionIssue {
                workflow: wf.name.clone(),
                key: key.clone(),
                kind,
            });
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: &str) -> RootConfig {
        RootConfig::from_toml_str(&format!(
            r#"
[plugins.slack]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"

[[workflows]]
name = "reply"
source = "slack"
agent = "herdr"
profile = "answer"
{extra}
"#
        ))
        .unwrap()
    }

    fn claims(pairs: &[(&str, &[(&str, &str)])]) -> BTreeMap<String, Vec<WorkflowOption>> {
        pairs
            .iter()
            .map(|(plugin, options)| {
                (
                    (*plugin).to_string(),
                    options
                        .iter()
                        .map(|(workflow, key)| WorkflowOption {
                            workflow: (*workflow).to_string(),
                            key: (*key).to_string(),
                        })
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn one_claimant_resolves() {
        let issues = check_workflow_options(
            &config(r#"thread_scope = "parent""#),
            &claims(&[("slack", &[("reply", "thread_scope")]), ("herdr", &[])]),
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    /// The typo case — the whole reason `deny_unknown_fields` could be dropped
    /// from `WorkflowConfig` without losing the check it was doing.
    #[test]
    fn a_key_nobody_claims_is_reported_with_who_was_asked() {
        let issues = check_workflow_options(
            &config(r#"profil = "triage""#),
            &claims(&[("slack", &[]), ("herdr", &[])]),
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].key, "profil");
        let text = issues[0].to_string();
        assert!(
            text.contains("`slack`") && text.contains("`herdr`"),
            "{text}"
        );
    }

    #[test]
    fn a_key_both_claim_is_ambiguous() {
        let issues = check_workflow_options(
            &config(r#"timeout = 5"#),
            &claims(&[
                ("slack", &[("reply", "timeout")]),
                ("herdr", &[("reply", "timeout")]),
            ]),
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(matches!(issues[0].kind, OptionIssueKind::Ambiguous { .. }));
    }

    /// A claim naming a *different* workflow must not satisfy this one, or a
    /// plugin that owns `publish` anywhere would legitimise it everywhere.
    #[test]
    fn a_claim_is_scoped_to_the_workflow_it_names() {
        let issues = check_workflow_options(
            &config(r#"thread_scope = "parent""#),
            &claims(&[
                ("slack", &[("some-other-workflow", "thread_scope")]),
                ("herdr", &[]),
            ]),
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(matches!(issues[0].kind, OptionIssueKind::Unclaimed { .. }));
    }

    /// A plugin that did not answer leaves the workflow unjudgeable. Reading
    /// its silence as "claims nothing" would bury a launch failure under
    /// unknown-key errors about config that is fine.
    #[test]
    fn a_workflow_whose_plugin_did_not_answer_is_skipped() {
        let issues = check_workflow_options(
            &config(r#"thread_scope = "parent""#),
            &claims(&[("slack", &[])]),
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn a_workflow_with_no_plugin_keys_needs_no_claims() {
        let issues = check_workflow_options(&config(""), &BTreeMap::new());
        assert!(issues.is_empty(), "{issues:?}");
    }
}
