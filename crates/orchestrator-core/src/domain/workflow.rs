//! Workflow definition and trigger matching (F-80–F-86).
//!
//! A workflow is a named `source × trigger × mode × agent × output` binding
//! ([`Workflow`]), interpreted from the parsed `[[workflows]]` config
//! ([`WorkflowConfig`]). It drives the
//! plan → human review → implement handoff via [`OutcomeAction`] status
//! transitions (F-84).
//!
//! Trigger *meaning* is owned by the task source plugin (it filters on the
//! trigger it receives at `initialize`), so the Orchestrator treats the
//! trigger as an opaque filter but additionally **re-checks** the pushed
//! task's `status`/`labels` defensively. Matching evaluates workflows in
//! definition order and takes the **first** match (F-81).

use plugin_protocol::Task;
use plugin_protocol::manifest::OutputCapability;

use crate::config::{
    CleanupPolicyConfig, OutputPolicy, Profile, PublishConfig, VerificationMode, WorkflowConfig,
    WorkflowMode,
};

/// How a reaction-derived task announces which emoji started it (#396).
///
/// A plugin that honours `trigger = { reaction = "..." }` stamps
/// `"reaction:<emoji>"` into [`Task::labels`], and [`Trigger::matches`] looks
/// for it there. `Task.labels` has existed since the first protocol version,
/// so carrying the emoji this way needs no wire change and no version bump.
pub const REACTION_LABEL_PREFIX: &str = "reaction:";

/// A trigger condition: an opaque key-value set the plugin filters on, plus the
/// status/label/reaction keys the Orchestrator re-checks defensively.
#[derive(Debug, Clone, PartialEq)]
pub struct Trigger(toml::Table);

impl Trigger {
    /// Wrap a raw trigger table.
    pub fn new(table: toml::Table) -> Self {
        Self(table)
    }

    /// The raw table, for passing to `initialize`'s `triggers` and for JSON
    /// conversion.
    pub fn as_table(&self) -> &toml::Table {
        &self.0
    }

    /// Convert to JSON for `initialize`'s `triggers` param. A `toml::Table`
    /// always serializes to a JSON object; the fallback is an empty object
    /// (never `null`) so the plugin always receives an object.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.0)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
    }

    /// Whether `task` satisfies the conditions the Orchestrator understands.
    ///
    /// **Reserved trigger keys** the Orchestrator re-checks against the
    /// normalized [`Task`] (a plugin contract — use these names consistently
    /// with the common schema, F-01):
    /// - `status` / `project_status` → compared to `task.status`
    /// - `label` (string) / `labels` (array) → looked up in `task.labels`
    /// - `reaction` → looked up in `task.labels` as [`REACTION_LABEL_PREFIX`]
    ///   + the emoji name (#396)
    ///
    /// All other keys are opaque: the plugin already filtered on them before
    /// pushing the task, so they are trusted. Non-string values on reserved keys
    /// are also treated as opaque (skipped). An empty trigger matches every
    /// task (a catch-all).
    ///
    /// **Skipping an unreadable reserved key means the trigger is weaker than
    /// it looks**, and for `reaction` that is the whole hazard back again — so
    /// [`validate_workflows`] rejects a non-string `reaction` outright rather
    /// than leaving it to be silently skipped here.
    ///
    /// `reaction` had to become reserved rather than stay opaque. Left opaque
    /// it is "satisfied" by every task, so a `reaction`-triggered workflow
    /// defined before the catch-all would swallow **mention-derived tasks
    /// too** — silently, since first-match produces a plausible run either
    /// way (#396).
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
                "reaction" => {
                    if let Some(want) = value.as_str()
                        && !task
                            .labels
                            .iter()
                            .any(|l| l == &format!("{REACTION_LABEL_PREFIX}{want}"))
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
                _ => {} // opaque; the plugin already filtered on it before pushing
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
    /// How the source delivers the published result (#548): `None` means the
    /// default (the plugin's approval flow).
    pub publish: Option<PublishConfig>,
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
            on_success: config.on_success.as_ref().map(OutcomeAction::from_table),
            on_failure: config.on_failure.as_ref().map(OutcomeAction::from_table),
            verification: config.resolved_verification(),
            timeout_secs: config.timeout_secs,
            rubric: config.rubric.clone(),
            tool: config.tool.clone(),
            profile: config.profile,
            // `""` and `"   "` mean the operator wrote the key and left it
            // blank. Rejecting that would be a validation error for something
            // with an obvious reading; normalising it here means no downstream
            // caller has to remember to trim.
            publish: config.publish,
            cleanup: config.cleanup,
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

/// Validate workflows (F-81, F-83).
///
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

        // A `reaction` that is not a string reverts to the pre-#396 hazard:
        // `matches` skips a reserved key it cannot read, so the workflow
        // matches **every** task from its source — and being defined above the
        // catch-all (which is where a reaction workflow belongs) it then
        // swallows the mentions. Meanwhile the plugin sees no emoji and never
        // fires the trigger. Both halves fail, in opposite directions, from
        // one mistyped value.
        if let Some(value) = wf.trigger.as_table().get("reaction")
            && value.as_str().is_none()
        {
            issues.push(WorkflowIssue {
                severity: Severity::Error,
                message: format!(
                    "workflow `{}` has a non-string `trigger.reaction` ({}) → write the emoji name as a string (`reaction = \"eyes\"`); a value that cannot be read is skipped during matching, so this workflow would match every task from `{}` while the plugin registers no emoji at all",
                    wf.name,
                    value.type_str(),
                    wf.source
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

    issues.extend(unreachable_after_catch_all(workflows));

    issues
}

/// Workflows that can never run because an earlier one in the same source
/// matches everything (#396).
///
/// A catch-all (`trigger = {}`) matches every task from its source, and
/// matching is first-match in definition order (F-81), so **everything after it
/// in that source is dead config**. This is the ordering mistake the reaction
/// notation invites: `trigger = { reaction = "hammer" }` reads as a filter that
/// stands on its own, and putting it below the mention catch-all silently turns
/// the emoji into a no-op.
///
/// Reported separately from the overlap warning above so the message can name
/// the fix (reorder) rather than just the ambiguity.
fn unreachable_after_catch_all(workflows: &[Workflow]) -> Vec<WorkflowIssue> {
    let mut issues = Vec::new();
    let mut catch_all: Vec<(&str, &str)> = Vec::new(); // (source, workflow name)

    for wf in workflows {
        if let Some((_, blocker)) = catch_all.iter().find(|(source, _)| *source == wf.source) {
            issues.push(WorkflowIssue {
                severity: Severity::Warning,
                message: format!(
                    "workflow `{}` (source `{}`) is unreachable: `{}` is defined earlier with an empty trigger, which matches every task from that source → move `{}` above `{}`",
                    wf.name, wf.source, blocker, wf.name, blocker
                ),
            });
        } else if wf.trigger.as_table().is_empty() {
            catch_all.push((&wf.source, &wf.name));
        }
    }

    issues
}

/// Whether some task could satisfy **both** triggers (F-81 ambiguity).
///
/// Two dimensions can make triggers mutually exclusive, and both work the same
/// way — a task has at most one value, so two triggers demanding *different*
/// values share no task:
///
/// - `status` / `project_status`. The **same** dimension under two spellings
///   (both compared to `task.status` in [`Trigger::matches`]), so they are
///   normalized together. `設計待ち` vs `実装待ち` → not ambiguous.
/// - `reaction` (#396). One task carries one reaction label, so `eyes` vs
///   `hammer` → not ambiguous.
///
/// Labels form a set (multiple required labels are jointly satisfiable) and
/// opaque keys cannot be proven contradictory, so neither forces non-overlap.
///
/// **A trigger requiring a reaction is treated as exclusive with one that does
/// not, which is not literally true** — a reaction-derived task carries the
/// label *and* satisfies a catch-all, so both match. It is deliberate: that
/// pair is the intended shape (`reaction = "hammer"` first, mention catch-all
/// last), and warning on the correct configuration is how a warning becomes
/// wallpaper. The genuinely broken order — catch-all first — is reported by
/// [`unreachable_after_catch_all`] instead, which can name the fix.
///
/// The imprecision has a cost worth stating: a reaction workflow placed after a
/// *narrower* non-catch-all trigger it overlaps with (say `label = "x"`) is
/// reported by neither check.
fn triggers_overlap(a: &Trigger, b: &Trigger) -> bool {
    // status: two *different* required values are exclusive; requiring one
    // against requiring none still overlaps (the unconstrained side matches
    // whatever the other demands). This is the pre-#396 rule, unchanged.
    let mut statuses: Vec<&str> = required_statuses(a.as_table());
    statuses.extend(required_statuses(b.as_table()));
    statuses.sort_unstable();
    statuses.dedup();
    if statuses.len() >= 2 {
        return false;
    }

    // reaction: any difference is exclusive, *including* "one requires an
    // emoji, the other requires none" — see the note above on why that
    // deliberate imprecision beats warning on the correct configuration.
    required_reaction(a.as_table()) == required_reaction(b.as_table())
}

/// The string status values a trigger requires (from `status`/`project_status`).
fn required_statuses(table: &toml::Table) -> Vec<&str> {
    ["status", "project_status"]
        .iter()
        .filter_map(|key| table.get(*key).and_then(|v| v.as_str()))
        .collect()
}

/// The emoji a trigger requires (from `reaction`), if any.
fn required_reaction(table: &toml::Table) -> Option<&str> {
    table.get("reaction").and_then(|v| v.as_str())
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
            message_key: None,
            instructions: None,
        }
    }

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
        assert_eq!(implement.output, OutputPolicy::Source);

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
    fn spec_example_does_not_warn_on_disjoint_status() {
        // design(設計待ち) and implement(実装待ち) contradict on project_status,
        // so no task matches both -> no overlap warning.
        let workflows = workflows_from_toml(SPEC_EXAMPLE);
        let issues = validate_workflows(&workflows, |_| Some(vec![OutputCapability::Source]));
        assert!(
            !issues.iter().any(|i| i.message.contains("overlapping")),
            "design/implement must not warn: {issues:?}"
        );
    }

    #[test]
    fn disjoint_key_triggers_overlap() {
        // Different dimensions (label vs status) can both match one task.
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "by_label"
source = "github"
trigger = { label = "bug" }
mode = "implement"
agent = "herdr"
output = "none"

[[workflows]]
name = "by_status"
source = "github"
trigger = { status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        // A task with both fits both workflows -> ambiguous.
        let t = task("github", Some("実装待ち"), &["bug"]);
        assert!(match_workflow(&workflows, &t).is_some());
        let issues = validate_workflows(&workflows, |_| None);
        assert!(
            issues.iter().any(|i| i.severity == Severity::Warning),
            "disjoint-key triggers must warn: {issues:?}"
        );
    }

    #[test]
    fn array_labels_require_all_and_parse_on_failure() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "multi"
source = "github"
trigger = { labels = ["ready", "backend"], status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
on_failure = { set_status = "失敗" }
"#,
        );
        // All required labels + status present.
        assert!(
            match_workflow(
                &workflows,
                &task("github", Some("実装待ち"), &["ready", "backend"])
            )
            .is_some()
        );
        // Missing one label -> no match.
        assert!(
            match_workflow(&workflows, &task("github", Some("実装待ち"), &["ready"])).is_none()
        );
        assert_eq!(
            workflows[0]
                .on_failure
                .as_ref()
                .unwrap()
                .set_status
                .as_deref(),
            Some("失敗")
        );
    }

    #[test]
    fn status_and_project_status_are_the_same_dimension_for_overlap() {
        // Different required statuses via different key spellings are still
        // mutually exclusive -> no overlap warning.
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "a"
source = "github"
trigger = { status = "A" }
mode = "implement"
agent = "herdr"
output = "none"

[[workflows]]
name = "b"
source = "github"
trigger = { project_status = "B" }
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        let issues = validate_workflows(&workflows, |_| None);
        assert!(
            !issues.iter().any(|i| i.message.contains("overlapping")),
            "different statuses (status vs project_status) must not warn: {issues:?}"
        );
    }

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

    /// The reaction notation in its intended shape: the emoji workflow first,
    /// the mention catch-all last.
    const REACTION_EXAMPLE: &str = r#"
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
"#;

    /// **The reason `reaction` had to stop being an opaque key.** Left opaque
    /// it is satisfied by every task, so `slack-implement` — defined first —
    /// would swallow mention-derived tasks and run them in implement mode.
    /// Nothing downstream would look wrong; the task simply took the other
    /// branch.
    #[test]
    fn a_reaction_workflow_defined_first_does_not_swallow_mentions() {
        let workflows = workflows_from_toml(REACTION_EXAMPLE);

        let reacted = task("slack", None, &["reaction:hammer"]);
        assert_eq!(
            match_workflow(&workflows, &reacted).unwrap().name,
            "slack-implement"
        );

        // A mention carries no reaction label, so it falls through to the
        // catch-all even though the emoji workflow is defined above it.
        let mention = task("slack", None, &[]);
        assert_eq!(
            match_workflow(&workflows, &mention).unwrap().name,
            "slack-reply"
        );

        // And a different emoji is not this workflow's.
        let other = task("slack", None, &["reaction:eyes"]);
        assert_eq!(
            match_workflow(&workflows, &other).unwrap().name,
            "slack-reply"
        );
    }

    #[test]
    fn the_intended_reaction_ordering_produces_no_warning() {
        // If the correct configuration warned, the warning would be wallpaper
        // and the real ordering mistake below would go unread.
        let issues = validate_workflows(&workflows_from_toml(REACTION_EXAMPLE), |_| {
            Some(vec![OutputCapability::Source])
        });
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn two_different_reactions_do_not_warn_but_the_same_one_twice_does() {
        let two = |a: &str, b: &str| {
            workflows_from_toml(&format!(
                r#"
[[workflows]]
name = "a"
source = "slack"
trigger = {{ reaction = "{a}" }}
profile = "answer"
agent = "herdr"

[[workflows]]
name = "b"
source = "slack"
trigger = {{ reaction = "{b}" }}
profile = "answer"
agent = "herdr"
"#
            ))
        };
        let distinct = validate_workflows(&two("eyes", "hammer"), |_| {
            Some(vec![OutputCapability::Source])
        });
        assert!(
            !distinct.iter().any(|i| i.message.contains("overlapping")),
            "one task carries one reaction, so these are exclusive: {distinct:?}"
        );

        let same = validate_workflows(&two("eyes", "eyes"), |_| {
            Some(vec![OutputCapability::Source])
        });
        assert!(
            same.iter().any(|i| i.message.contains("overlapping")),
            "two workflows on one emoji genuinely collide: {same:?}"
        );
    }

    /// The ordering mistake the reaction notation invites: `trigger = {…}`
    /// reads as a self-standing filter, so putting it below the catch-all
    /// looks fine and makes the emoji a no-op.
    #[test]
    fn a_workflow_after_a_catch_all_is_reported_unreachable() {
        let workflows = workflows_from_toml(
            r#"
[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"

[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }
profile = "implement"
agent = "herdr"
"#,
        );
        // The behaviour the warning is about: the emoji never runs.
        assert_eq!(
            match_workflow(&workflows, &task("slack", None, &["reaction:hammer"]))
                .unwrap()
                .name,
            "slack-reply"
        );

        let issues = validate_workflows(&workflows, |_| Some(vec![OutputCapability::Source]));
        let issue = issues
            .iter()
            .find(|i| i.message.contains("unreachable"))
            .unwrap_or_else(|| panic!("expected an unreachable warning: {issues:?}"));
        assert_eq!(issue.severity, Severity::Warning);
        // Naming both sides is the whole value — "unreachable" alone leaves
        // the operator hunting for which line to move.
        assert!(issue.message.contains("slack-implement"), "{issue:?}");
        assert!(issue.message.contains("slack-reply"), "{issue:?}");
    }

    /// One typo puts the pre-#396 hazard straight back: `matches` skips a
    /// reserved key it cannot read, so a non-string `reaction` matches
    /// **everything** — and this workflow belongs above the catch-all, so it
    /// swallows the mentions. The plugin meanwhile registers no emoji. Both
    /// halves fail, in opposite directions, and neither reports anything.
    #[test]
    fn a_non_string_reaction_is_rejected_rather_than_silently_skipped() {
        for value in ["123", "true", r#"["eyes"]"#] {
            let workflows = workflows_from_toml(&format!(
                r#"
[[workflows]]
name = "typo"
source = "slack"
trigger = {{ reaction = {value} }}
profile = "implement"
agent = "herdr"
"#
            ));
            // The behaviour being guarded against: it currently matches a task
            // that carries no reaction at all.
            assert!(
                match_workflow(&workflows, &task("slack", None, &[])).is_some(),
                "{value}: an unreadable reserved key is skipped, which is why this must not ship"
            );

            let issues = validate_workflows(&workflows, |_| Some(vec![OutputCapability::Source]));
            let issue = issues
                .iter()
                .find(|i| i.message.contains("non-string"))
                .unwrap_or_else(|| panic!("{value}: expected a rejection: {issues:?}"));
            assert_eq!(issue.severity, Severity::Error, "{issue:?}");
            // The message has to explain *both* failures, or the operator fixes
            // the emoji and never learns what the run was doing meanwhile.
            assert!(issue.message.contains("every task"), "{issue:?}");
        }
    }

    #[test]
    fn a_catch_all_does_not_shadow_another_source() {
        // Matching is per-source, so a Slack catch-all says nothing about a
        // GitHub workflow defined after it.
        let issues = validate_workflows(
            &workflows_from_toml(
                r#"
[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"

[[workflows]]
name = "gh-design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"
agent = "herdr"
"#,
            ),
            |_| Some(vec![OutputCapability::Source]),
        );
        assert!(
            !issues.iter().any(|i| i.message.contains("unreachable")),
            "{issues:?}"
        );
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
