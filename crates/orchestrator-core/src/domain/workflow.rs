//! Workflow definition and trigger matching (F-80–F-86).
//!
//! A workflow is a named `source × trigger × mode × agent × output` binding
//! ([`Workflow`]), interpreted from the parsed `[[workflows]]` config
//! ([`WorkflowConfig`](crate::config::WorkflowConfig)). It drives the
//! plan → human review → implement handoff via [`OutcomeAction`] status
//! transitions (F-84).
//!
//! Trigger *meaning* is owned by the task source plugin (it filters at
//! `tasks/fetch`), so the Orchestrator treats the trigger as an opaque filter
//! but additionally **re-checks** the returned task's `status`/`labels`
//! defensively. Matching evaluates workflows in definition order and takes the
//! **first** match (F-81).

use plugin_protocol::Task;
use plugin_protocol::manifest::OutputCapability;

use crate::config::{OutputPolicy, WorkflowConfig, WorkflowMode};

/// A trigger condition: an opaque key-value set the plugin filters on, plus the
/// status/label keys the Orchestrator re-checks defensively.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger(toml::Table);

impl Trigger {
    /// Wrap a raw trigger table.
    pub fn new(table: toml::Table) -> Self {
        Self(table)
    }

    /// The raw table, for passing to `tasks/fetch` and for JSON conversion.
    pub fn as_table(&self) -> &toml::Table {
        &self.0
    }

    /// Convert to JSON for the `tasks/fetch` RPC params.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.0).unwrap_or(serde_json::Value::Null)
    }

    /// Whether `task` satisfies the conditions the Orchestrator understands.
    ///
    /// `status`/`project_status` are checked against `task.status`; `label`
    /// (string or array) against `task.labels`. Keys the Orchestrator does not
    /// understand are trusted (the plugin already filtered on them).
    pub fn matches(&self, task: &Task) -> bool {
        for (key, value) in &self.0 {
            match key.as_str() {
                "status" | "project_status" => {
                    if let Some(want) = value.as_str()
                        && task.status.as_deref() != Some(want)
                    {
                        return false;
                    }
                }
                "label" | "labels" => {
                    if let Some(want) = value.as_str() {
                        if !task.labels.iter().any(|l| l == want) {
                            return false;
                        }
                    } else if let Some(array) = value.as_array() {
                        // Require every named label to be present.
                        for item in array {
                            if let Some(want) = item.as_str()
                                && !task.labels.iter().any(|l| l == want)
                            {
                                return false;
                            }
                        }
                    }
                }
                _ => {} // opaque; the plugin filtered on it at tasks/fetch
            }
        }
        true
    }
}

/// A source-side status transition applied when a task ends (F-84).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutcomeAction {
    /// Status to set on the source (`set_status`).
    pub set_status: Option<String>,
}

impl OutcomeAction {
    /// Interpret an `on_success`/`on_failure` table.
    fn from_table(table: &toml::Table) -> Self {
        Self {
            set_status: table
                .get("set_status")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    }
}

/// A named workflow binding (F-80).
#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    /// Workflow name.
    pub name: String,
    /// Task source instance name.
    pub source: String,
    /// Trigger condition.
    pub trigger: Trigger,
    /// Execution mode.
    pub mode: WorkflowMode,
    /// Agent plugin instance name.
    pub agent: String,
    /// Output policy.
    pub output: OutputPolicy,
    /// Action on success.
    pub on_success: Option<OutcomeAction>,
    /// Action on failure.
    pub on_failure: Option<OutcomeAction>,
}

impl Workflow {
    /// Interpret a parsed config workflow.
    pub fn from_config(config: &WorkflowConfig) -> Self {
        Self {
            name: config.name.clone(),
            source: config.source.clone(),
            trigger: Trigger::new(config.trigger.clone()),
            mode: config.mode,
            agent: config.agent.clone(),
            output: config.output,
            on_success: config.on_success.as_ref().map(OutcomeAction::from_table),
            on_failure: config.on_failure.as_ref().map(OutcomeAction::from_table),
        }
    }

    /// Interpret all workflows from a config.
    pub fn from_configs(configs: &[WorkflowConfig]) -> Vec<Self> {
        configs.iter().map(Self::from_config).collect()
    }
}

/// The first workflow (in definition order) that matches `task` for its source
/// (F-81). A task matches at most one workflow.
pub fn match_workflow<'a>(workflows: &'a [Workflow], task: &Task) -> Option<&'a Workflow> {
    workflows
        .iter()
        .find(|w| w.source == task.source && w.trigger.matches(task))
}

/// Severity of a workflow validation issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Blocks running.
    Error,
    /// Advisory only.
    Warning,
}

/// A workflow validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowIssue {
    /// Severity.
    pub severity: Severity,
    /// Human-readable message ("cause + next action").
    pub message: String,
}

/// Validate workflows (F-81, F-82, F-83).
///
/// - `mode = plan` with `output = pull_request` is an error (plan must not
///   push/PR, F-82).
/// - `output = source` requires the source plugin to declare the `source`
///   output capability (F-83). `source_outputs` returns a plugin's declared
///   outputs, or `None` when unknown (then the check is skipped — an unknown
///   plugin is already flagged by config validation).
/// - Two workflows in the same source whose triggers can both match a task are
///   a warning (F-81).
pub fn validate_workflows<F>(workflows: &[Workflow], source_outputs: F) -> Vec<WorkflowIssue>
where
    F: Fn(&str) -> Option<Vec<OutputCapability>>,
{
    let mut issues = Vec::new();

    for wf in workflows {
        // F-82: plan mode cannot push/PR.
        if wf.mode == WorkflowMode::Plan && wf.output == OutputPolicy::PullRequest {
            issues.push(WorkflowIssue {
                severity: Severity::Error,
                message: format!(
                    "workflow `{}` uses mode = plan with output = pull_request → plan must not push/PR; use output = source or none",
                    wf.name
                ),
            });
        }

        // F-83: output = source needs the plugin to declare it.
        if wf.output == OutputPolicy::Source
            && let Some(outputs) = source_outputs(&wf.source)
            && !outputs.contains(&OutputCapability::Source)
        {
            issues.push(WorkflowIssue {
                severity: Severity::Error,
                message: format!(
                    "workflow `{}` output = source but plugin `{}` does not declare the `source` output capability → use a capable plugin or change output",
                    wf.name, wf.source
                ),
            });
        }
    }

    // F-81: within a source, triggers that can both match a task are ambiguous.
    for (i, a) in workflows.iter().enumerate() {
        for b in &workflows[i + 1..] {
            if a.source == b.source && triggers_overlap(&a.trigger, &b.trigger) {
                issues.push(WorkflowIssue {
                    severity: Severity::Warning,
                    message: format!(
                        "workflows `{}` and `{}` (source `{}`) have overlapping triggers → a task could match both; the earlier definition wins",
                        a.name, b.name, a.source
                    ),
                });
            }
        }
    }

    issues
}

/// Whether one trigger's conditions are a subset of the other's, meaning a task
/// matching the stricter one also matches the looser one.
fn triggers_overlap(a: &Trigger, b: &Trigger) -> bool {
    is_subset(a.as_table(), b.as_table()) || is_subset(b.as_table(), a.as_table())
}

/// Whether every key/value in `sub` appears identically in `sup`.
fn is_subset(sub: &toml::Table, sup: &toml::Table) -> bool {
    sub.iter()
        .all(|(k, v)| sup.get(k).is_some_and(|sv| sv == v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(source: &str, status: Option<&str>, labels: &[&str]) -> Task {
        Task {
            id: "1".into(),
            source: source.into(),
            title: "t".into(),
            body: None,
            repo_hint: None,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            priority: 0,
            status: status.map(str::to_string),
            url: None,
            assignee: None,
        }
    }

    fn workflows_from_toml(toml: &str) -> Vec<Workflow> {
        let cfg = crate::config::RootConfig::from_toml_str(toml).unwrap();
        Workflow::from_configs(&cfg.workflows)
    }

    /// The §4.9 example: design (plan/source) + implement (implement/pull_request).
    const SPEC_EXAMPLE: &str = r#"
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { set_status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = { set_status = "レビュー待ち" }
"#;

    #[test]
    fn spec_example_parses_and_matches() {
        let workflows = workflows_from_toml(SPEC_EXAMPLE);
        assert_eq!(workflows.len(), 2);

        let design = match_workflow(&workflows, &task("github", Some("設計待ち"), &[])).unwrap();
        assert_eq!(design.name, "design");
        assert_eq!(design.mode, WorkflowMode::Plan);
        assert_eq!(
            design.on_success.as_ref().unwrap().set_status.as_deref(),
            Some("設計レビュー待ち")
        );

        let implement = match_workflow(&workflows, &task("github", Some("実装待ち"), &[])).unwrap();
        assert_eq!(implement.name, "implement");
        assert_eq!(implement.output, OutputPolicy::PullRequest);

        // No matching status -> no workflow.
        assert!(match_workflow(&workflows, &task("github", Some("完了"), &[])).is_none());
        // Right status but wrong source -> no match.
        assert!(match_workflow(&workflows, &task("notion", Some("設計待ち"), &[])).is_none());
    }

    #[test]
    fn label_triggers_match() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "bugs"
source = "github"
trigger = { label = "bug" }
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        assert!(match_workflow(&workflows, &task("github", None, &["bug"])).is_some());
        assert!(match_workflow(&workflows, &task("github", None, &["feature"])).is_none());
    }

    #[test]
    fn first_match_wins_and_overlap_warns() {
        // Two workflows whose triggers overlap (one subsumes the other).
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "wide"
source = "github"
trigger = { label = "ready" }
mode = "implement"
agent = "herdr"
output = "none"

[[workflows]]
name = "narrow"
source = "github"
trigger = { label = "ready", project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        // A task matching both takes the first-defined (`wide`).
        let t = task("github", Some("実装待ち"), &["ready"]);
        assert_eq!(match_workflow(&workflows, &t).unwrap().name, "wide");

        let issues = validate_workflows(&workflows, |_| None);
        assert!(
            issues
                .iter()
                .any(|i| i.severity == Severity::Warning && i.message.contains("overlapping")),
            "expected overlap warning: {issues:?}"
        );
    }

    #[test]
    fn plan_with_pull_request_is_an_error() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "bad"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "pull_request"
"#,
        );
        let issues = validate_workflows(&workflows, |_| None);
        assert!(issues.iter().any(|i| i.severity == Severity::Error
            && i.message.contains("plan with output = pull_request")));
    }

    #[test]
    fn output_source_requires_declared_capability() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
"#,
        );
        // Plugin does not declare `source` output -> error.
        let issues = validate_workflows(&workflows, |_| Some(vec![]));
        assert!(issues.iter().any(|i| i.severity == Severity::Error
            && i.message.contains("does not declare the `source` output")));

        // Plugin declares it -> no capability error.
        let issues = validate_workflows(&workflows, |_| Some(vec![OutputCapability::Source]));
        assert!(
            !issues
                .iter()
                .any(|i| i.message.contains("output capability"))
        );
    }
}
