//! Workflow definition and trigger matching (F-80–F-86).
//!
//! A workflow is a named `source × trigger × mode × agent × output` binding
//! ([`Workflow`]), interpreted from the parsed `[[workflows]]` config
//! ([`WorkflowConfig`]). It drives the
//! plan → human review → implement handoff via [`OutcomeAction`] status
//! transitions (F-84).
//!
//! Trigger *meaning* is owned by the task source plugin end to end (0.6.0,
//! #554): it filters on the trigger it receives at `initialize`, runs
//! first-match (F-81) on its own side, and **names the resulting workflow on
//! `task/submit`**. The Orchestrator holds the trigger as an opaque table it
//! passes along and never reads.
//!
//! It used to re-check the pushed task's `status`/`labels` against the trigger
//! as well. That protected nothing — both fields are the plugin's own report
//! of the task, so the check and the thing checked came from one place — while
//! requiring every trigger key a source wanted (`reaction`, `project_status`)
//! to be a word in this module's vocabulary.

use std::collections::{BTreeMap, BTreeSet};

use plugin_protocol::manifest::OutputCapability;

use crate::config::{
    CleanupPolicyConfig, OutputPolicy, Profile, VerificationMode, WorkflowConfig, WorkflowMode,
};

/// A trigger condition: an opaque key-value set the plugin filters on.
///
/// Opaque all the way through since 0.6.0 (#554) — the Orchestrator carries it
/// to `initialize` and never interprets a key of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger(toml::Table);

impl Trigger {
    /// Wrap a raw trigger table.
    pub fn new(table: toml::Table) -> Self {
        Self(table)
    }

    /// The raw table, for passing to `initialize`'s `workflows` and for JSON
    /// conversion.
    pub fn as_table(&self) -> &toml::Table {
        &self.0
    }

    /// Convert to JSON for `initialize`'s `workflows` param. A `toml::Table`
    /// always serializes to a JSON object; the fallback is an empty object
    /// (never `null`) so the plugin always receives an object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.0)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
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
    /// Action when the task starts running (#556).
    pub on_start: Option<OutcomeAction>,
    /// Action on success.
    pub on_success: Option<OutcomeAction>,
    /// Action on failure.
    pub on_failure: Option<OutcomeAction>,
    /// How completion self-reports are verified (D-01).
    pub verification: VerificationMode,
    /// Silence limit in seconds since the last hook signal before escalation
    /// (D-03). `None` means the built-in default (30 minutes); `Some(0)` opts
    /// the workflow out of the sweep entirely (#439, attended panes).
    pub timeout_secs: Option<u64>,
    /// Criteria text for the llm-verification prompt hook.
    pub rubric: Option<String>,
    /// Explicit AI-tool pin (#196); `None` falls through to the repository /
    /// global defaults at dispatch.
    pub tool: Option<String>,
    /// The archetype this workflow was written as (#394), carried through
    /// rather than resolved away.
    ///
    /// Everything else here is a *resolved* value, deliberately — see
    /// [`from_config`](Self::from_config). This one is the exception because
    /// [#399](https://github.com/tomoya-k31/totsuka/issues/399) asks a question
    /// the resolved values cannot answer: which external tool the agent will
    /// need. `mode = "implement"` says the worktree is writable; it does not
    /// say a pull request is the deliverable.
    pub profile: Option<Profile>,
    /// Operator-written instructions prepended to the task body when a
    /// **new** conversation is started (#415).
    ///
    /// Normalised at interpretation: empty or whitespace-only in the config
    /// becomes `None`, so downstream only has to ask "is there one".
    pub initial_prompt: Option<String>,
    /// Worktree cleanup override (#548): `None` means the mode-selected
    /// `[worktree]` default.
    pub cleanup: Option<CleanupPolicyConfig>,
}

impl Workflow {
    /// Interpret a parsed config workflow.
    ///
    /// This is the **single** place a `profile` is resolved into concrete
    /// mode/output/verification values (#394): everything downstream reads this
    /// struct, whose fields are already concrete, so no other code has to know
    /// profiles exist.
    pub fn from_config(config: &WorkflowConfig) -> Self {
        Self {
            name: config.name.clone(),
            source: config.source.clone(),
            trigger: Trigger::new(config.trigger.clone()),
            mode: config.resolved_mode(),
            agent: config.agent.clone(),
            output: config.resolved_output(),
            on_start: config.on_start.as_ref().map(OutcomeAction::from_table),
            on_success: config.on_success.as_ref().map(OutcomeAction::from_table),
            on_failure: config.on_failure.as_ref().map(OutcomeAction::from_table),
            verification: config.resolved_verification(),
            timeout_secs: config.timeout_secs,
            rubric: config.rubric.clone(),
            tool: config.tool.clone(),
            profile: config.profile,
            cleanup: config.cleanup,
            // `""` and `"   "` mean the operator wrote the key and left it
            // blank. Rejecting that would be a validation error for something
            // with an obvious reading; normalising it here means no downstream
            // caller has to remember to trim.
            initial_prompt: config
                .initial_prompt
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        }
    }

    /// Interpret all workflows from a config.
    pub fn from_configs(configs: &[WorkflowConfig]) -> Vec<Self> {
        configs.iter().map(Self::from_config).collect()
    }
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

/// Validate workflows (F-81, F-83).
///
/// - `output = source` requires the source plugin to declare the `source`
///   output capability (F-83). `source_outputs` returns a plugin's declared
///   outputs, or `None` when unknown (then the check is skipped — an unknown
///   plugin is already flagged by config validation).
///
/// # What is no longer checked here
///
/// Trigger overlap, unreachable-after-catch-all and the `reaction` value type
/// were checked here until 0.6.0 (#396, #554). All three needed the
/// Orchestrator to *interpret* triggers, which it no longer does — and for
/// Slack, the hazard they guarded is gone by construction rather than moved: a
/// mention and a reaction take different event paths in the plugin, so a
/// reaction workflow written after the mention one is not shadowed by it. What
/// remains genuinely ambiguous (two workflows claiming the same emoji, two
/// claiming plain mentions) is refused by the plugin at `initialize`, where the
/// semantics live.
pub fn validate_workflows<F>(workflows: &[Workflow], source_outputs: F) -> Vec<WorkflowIssue>
where
    F: Fn(&str) -> Option<Vec<OutputCapability>>,
{
    let mut issues = Vec::new();

    for wf in workflows {
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
    issues.extend(column_cycles(workflows));

    issues
}

/// One hop of a column cycle: the workflow that ran, and the column its
/// write-back sent the card to.
struct Hop<'a> {
    workflow: &'a str,
    key: &'a str,
    to_column: &'a str,
}

/// Status write-backs that route a card back into a column some workflow
/// triggers on, in a loop (#565).
///
/// Since a lane re-entry is a *request* (#556) and a re-entry under another
/// workflow hands the conversation over to it (#565), a cycle in the column
/// graph runs **forever with no human in it** — every lap dispatches an agent
/// and spends real tokens. The single-workflow case (a write-back into the
/// workflow's own trigger column) is this same check with a cycle of length 1.
///
/// **Lexical only.** The Orchestrator does not interpret triggers (#554), and
/// this reads two operator-written strings without acting on either: a
/// plugin-side `status_map` that aliases two names onto one column is out of
/// its sight, and so is a column shared by name across two different trackers
/// (which is not a cycle at all — different boards, and `source` separates
/// them here).
fn column_cycles(workflows: &[Workflow]) -> Vec<WorkflowIssue> {
    let mut issues = Vec::new();
    // Per source: a column reached by a write-back only re-triggers a workflow
    // watching the *same* tracker.
    let sources: BTreeSet<&str> = workflows.iter().map(|w| w.source.as_str()).collect();
    for source in sources {
        let of_source: Vec<&Workflow> = workflows
            .iter()
            .filter(|w| w.source == source)
            .filter(|w| trigger_column(w).is_some())
            .collect();
        // column → the hops leaving it (one per write-back key that names a
        // column, from every workflow triggering on it).
        let mut edges: BTreeMap<&str, Vec<Hop>> = BTreeMap::new();
        for wf in &of_source {
            let from = trigger_column(wf).expect("filtered above");
            for (key, action) in [
                ("on_start", &wf.on_start),
                ("on_success", &wf.on_success),
                ("on_failure", &wf.on_failure),
            ] {
                if let Some(to) = action.as_ref().and_then(|a| a.set_status.as_deref()) {
                    edges.entry(from).or_default().push(Hop {
                        workflow: wf.name.as_str(),
                        key,
                        to_column: to,
                    });
                }
            }
        }
        // DFS with an explicit path, so the message can name the actual loop
        // rather than just asserting one exists.
        let mut settled: BTreeSet<&str> = BTreeSet::new();
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for start in edges.keys().copied().collect::<Vec<_>>() {
            let mut path: Vec<(&str, &Hop)> = Vec::new();
            walk(
                start,
                &edges,
                &mut path,
                &mut settled,
                &mut reported,
                &mut issues,
            );
        }
    }
    issues
}

/// Depth-first walk over the column graph, reporting each cycle once.
fn walk<'a>(
    column: &'a str,
    edges: &'a BTreeMap<&'a str, Vec<Hop<'a>>>,
    path: &mut Vec<(&'a str, &'a Hop<'a>)>,
    settled: &mut BTreeSet<&'a str>,
    reported: &mut BTreeSet<String>,
    issues: &mut Vec<WorkflowIssue>,
) {
    if settled.contains(column) {
        return;
    }
    // The column is already on the path: everything from its first appearance
    // onwards is the cycle.
    if let Some(at) = path.iter().position(|(c, _)| *c == column) {
        let cycle = &path[at..];
        let mut route = String::new();
        for (from, hop) in cycle {
            route.push_str(&format!(
                "column `{from}` → workflow `{}` ({}) → ",
                hop.workflow, hop.key
            ));
        }
        route.push_str(&format!("column `{column}`"));
        // Normalise so the same loop found from a different entry point is
        // reported once: rotate the workflow names to their smallest order.
        let mut names: Vec<&str> = cycle.iter().map(|(_, h)| h.workflow).collect();
        names.sort_unstable();
        names.dedup();
        if reported.insert(names.join("\u{1f}")) {
            issues.push(WorkflowIssue {
                severity: Severity::Error,
                message: format!(
                    "status write-backs form a loop with no human in it: {route} → each lap \
                     dispatches an agent again, forever → route one hop through a column no \
                     workflow triggers on (a review column a person moves the card out of)"
                ),
            });
        }
        return;
    }
    for hop in edges.get(column).into_iter().flatten() {
        path.push((column, hop));
        walk(hop.to_column, edges, path, settled, reported, issues);
        path.pop();
    }
    settled.insert(column);
}

/// The column a workflow triggers on, when it triggers on one at all.
fn trigger_column(wf: &Workflow) -> Option<&str> {
    wf.trigger
        .as_table()
        .get("project_status")
        .and_then(|v| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflows_from_toml(toml: &str) -> Vec<Workflow> {
        let cfg = crate::config::RootConfig::from_toml_str(toml).unwrap();
        Workflow::from_configs(&cfg.workflows)
    }

    /// The §4.9 example: design (plan/source) + implement (implement/source).
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
output = "source"
on_success = { set_status = "レビュー待ち" }
"#;

    #[test]
    fn verification_fields_are_wired_from_config() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "verified"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "human"
timeout_secs = 600
rubric = "実調査に基づくこと"

[[workflows]]
name = "defaulted"
source = "slack"
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        assert_eq!(workflows[0].verification, VerificationMode::Human);
        assert_eq!(workflows[0].timeout_secs, Some(600));
        assert_eq!(workflows[0].rubric.as_deref(), Some("実調査に基づくこと"));
        // Omitted -> D-01 default llm, no overrides.
        assert_eq!(workflows[1].verification, VerificationMode::Llm);
        assert!(workflows[1].timeout_secs.is_none());
        assert!(workflows[1].rubric.is_none());
    }

    /// Two workflows handing the card back and forth (#565): with handoff,
    /// a lane re-entry under another workflow re-runs the conversation there,
    /// so this ping-pong never stops and never asks a person. The message has
    /// to name the actual route — "there is a cycle" is not actionable.
    #[test]
    fn a_two_workflow_column_ping_pong_is_a_loop_and_errors() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "Design" }
mode = "plan"
agent = "herdr"
output = "none"
on_success = { set_status = "Todo" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "Todo" }
mode = "implement"
agent = "herdr"
output = "none"
on_success = { set_status = "Design" }
"#,
        );
        let issues = validate_workflows(&workflows, |_| None);
        assert_eq!(issues.len(), 1, "one loop, reported once: {issues:?}");
        let m = &issues[0].message;
        assert_eq!(issues[0].severity, Severity::Error);
        for needle in ["design", "implement", "Design", "Todo"] {
            assert!(m.contains(needle), "route must name `{needle}`: {m}");
        }
    }

    /// The pipeline the spec's §4.9 example describes: design hands off to
    /// implement, and implement parks the card in a column nobody triggers on.
    /// A person moves it from there — that is the hop that breaks the cycle.
    #[test]
    fn a_pipeline_that_ends_in_a_human_column_is_fine() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "Design" }
mode = "plan"
agent = "herdr"
output = "none"
on_success = { set_status = "Todo" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "Todo" }
mode = "implement"
agent = "herdr"
output = "none"
on_start = { set_status = "In Progress" }
on_success = { set_status = "Done" }
on_failure = { set_status = "Failed" }
"#,
        );
        assert!(
            validate_workflows(&workflows, |_| None).is_empty(),
            "a chain is not a cycle"
        );
    }

    /// Two trackers can name a column the same way without it being one
    /// column: the graph is built per `source`.
    #[test]
    fn identically_named_columns_on_different_sources_are_not_a_cycle() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "gh"
source = "github"
trigger = { project_status = "Todo" }
mode = "implement"
agent = "herdr"
output = "none"
on_success = { set_status = "Review" }

[[workflows]]
name = "nt"
source = "notion"
trigger = { project_status = "Review" }
mode = "implement"
agent = "herdr"
output = "none"
on_success = { set_status = "Todo" }
"#,
        );
        assert!(
            validate_workflows(&workflows, |_| None).is_empty(),
            "different trackers, different columns"
        );
    }

    #[test]
    fn a_write_back_into_the_own_trigger_column_is_a_loop_and_errors() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "looping"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
on_failure = { set_status = "実装待ち" }

[[workflows]]
name = "fine"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
on_success = { set_status = "レビュー待ち" }

[[workflows]]
name = "label-only"
source = "github"
trigger = { label = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
on_success = { set_status = "実装待ち" }
"#,
        );
        let issues = validate_workflows(&workflows, |_| None);
        // Only the first workflow loops: `fine` writes elsewhere, and
        // `label-only` has no lane for the write-back to re-enter (the check
        // is lexical, over `project_status` only). A self-loop is a cycle of
        // length 1 — the same check as the multi-workflow ping-pong (#565).
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(
            issues[0].message.contains("looping") && issues[0].message.contains("on_failure"),
            "{}",
            issues[0].message
        );
    }

    #[test]
    fn on_start_is_wired_from_config_and_absent_by_default() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "with-start"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
on_start = { set_status = "実装中" }
on_success = { set_status = "レビュー待ち" }

[[workflows]]
name = "without-start"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        assert_eq!(
            workflows[0]
                .on_start
                .as_ref()
                .and_then(|a| a.set_status.as_deref()),
            Some("実装中"),
        );
        // Omitted means "write nothing at start" — the pre-#556 behaviour,
        // which every existing config must keep byte-for-byte.
        assert!(workflows[1].on_start.is_none());
    }

    #[test]
    fn each_profile_resolves_the_documented_bundle() {
        // The #393 D5 table, pinned. These four rows decide what a workflow may
        // do, so a silent edit to `Profile::mode` is the kind of change that
        // hands `implement` powers to an `answer` task.
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "answer"
source = "slack"
trigger = { label = "a" }
profile = "answer"
agent = "herdr"

[[workflows]]
name = "triage"
source = "slack"
trigger = { label = "t" }
profile = "triage"
agent = "herdr"

[[workflows]]
name = "design"
source = "slack"
trigger = { label = "d" }
profile = "design"
agent = "herdr"

[[workflows]]
name = "implement"
source = "slack"
trigger = { label = "i" }
profile = "implement"
agent = "herdr"
"#,
        );
        let expected = [
            ("answer", WorkflowMode::Plan, OutputPolicy::Source),
            ("triage", WorkflowMode::Plan, OutputPolicy::Source),
            ("design", WorkflowMode::Plan, OutputPolicy::None),
            ("implement", WorkflowMode::Implement, OutputPolicy::None),
        ];
        for (wf, (name, mode, output)) in workflows.iter().zip(expected) {
            assert_eq!(wf.name, name);
            assert_eq!(wf.mode, mode, "{name} mode");
            assert_eq!(wf.output, output, "{name} output");
            // All four judge with the llm verifier; #398 varies the rubric, not
            // the mode.
            assert_eq!(wf.verification, VerificationMode::Llm, "{name}");
        }
    }

    #[test]
    fn an_explicit_output_overrides_the_profile_but_mode_still_comes_from_it() {
        // The one documented override: a Slack-sourced `implement` needs
        // `output = "source"` to get its PR URL back into the thread, and that
        // choice of destination is not a permission.
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "slack-implement"
source = "slack"
profile = "implement"
output = "source"
agent = "herdr"
"#,
        );
        assert_eq!(workflows[0].output, OutputPolicy::Source);
        assert_eq!(workflows[0].mode, WorkflowMode::Implement);
    }

    #[test]
    fn a_config_without_profiles_resolves_exactly_as_before() {
        // The compatibility half of making `mode`/`output` optional: every
        // pre-#394 config has to mean what it meant.
        let workflows = workflows_from_toml(SPEC_EXAMPLE);
        assert_eq!(workflows[0].mode, WorkflowMode::Plan);
        assert_eq!(workflows[0].output, OutputPolicy::Source);
        assert_eq!(workflows[0].verification, VerificationMode::Llm);
        assert_eq!(workflows[1].mode, WorkflowMode::Implement);
    }

    #[test]
    fn initial_prompt_is_carried_through_and_blank_means_unset() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "Design" }
profile = "design"
agent = "herdr"
initial_prompt = "  /grill-me で {設計観点} を詰めてください  "

[[workflows]]
name = "blank"
source = "github"
trigger = {}
profile = "design"
agent = "herdr"
initial_prompt = "   "

[[workflows]]
name = "absent"
source = "github"
trigger = {}
profile = "design"
agent = "herdr"
"#,
        );
        assert_eq!(
            workflows[0].initial_prompt.as_deref(),
            Some("/grill-me で {設計観点} を詰めてください"),
            "trimmed, but otherwise literal — nothing runs `template::render` \
             over it, so a brace survives interpretation"
        );
        // Written-but-blank reads as unset rather than as an empty preamble
        // followed by two newlines.
        assert_eq!(workflows[1].initial_prompt, None);
        assert_eq!(workflows[2].initial_prompt, None);
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
