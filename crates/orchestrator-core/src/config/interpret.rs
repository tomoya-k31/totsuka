//! Interpretation of the parsed config into [`domain`](crate::domain) values
//! (#762).
//!
//! `domain` does not know `config.toml` exists: it receives resolved
//! [`Workflow`]s and [`CleanupPolicy`]s. Everything that knows how the file
//! spells them — which `on_*` key names a column, how a `profile` resolves,
//! what the `keep_*` presets mean — lives here, on the config side of the
//! boundary, so a schema change stops at this module.

use crate::domain::{CleanupPolicy, OutcomeAction, Trigger, Workflow};

use super::schema::{CleanupPolicyConfig, CleanupPolicyName, RootConfig, WorkflowConfig};

/// The `on_start` / `on_success` / `on_failure` keys the Orchestrator reads
/// (#574).
///
/// Kept beside [`outcome_action`] because that is what makes them
/// true. `config validate` — which `run` shares — rejects every other key, so
/// a typo cannot silently drop a status write-back; add a key here in the same
/// edit that teaches `outcome_action` to read it.
///
/// The key is spelled the same as the `trigger` one it pairs with (#575): both
/// name the source's status column, and the surrounding table says which
/// direction it is read in.
pub const OUTCOME_ACTION_KEYS: &[&str] = &["status"];

/// Interpret an `on_start`/`on_success`/`on_failure` table.
///
/// `pub(crate)` so the one place that reads the `status` key stays the one
/// place: `plugins::spec` derives a workflow's write-back columns for
/// `WorkflowInfo.status_writebacks` through this, rather than reaching
/// into the table itself (#626). [`OUTCOME_ACTION_KEYS`] beside it is what
/// makes the vocabulary true, and a second reader would be able to drift
/// from it silently.
pub(crate) fn outcome_action(table: &toml::Table) -> OutcomeAction {
    OutcomeAction {
        status: table
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

/// The `keep_*` presets (#210) desugar to `RetentionDays` here —
/// [`CleanupPolicy`] never learns about them.
impl From<CleanupPolicyConfig> for CleanupPolicy {
    fn from(config: CleanupPolicyConfig) -> Self {
        match config {
            CleanupPolicyConfig::Named(CleanupPolicyName::Immediate) => CleanupPolicy::Immediate,
            CleanupPolicyConfig::Named(CleanupPolicyName::Manual) => CleanupPolicy::Manual,
            CleanupPolicyConfig::Named(CleanupPolicyName::Keep7d) => {
                CleanupPolicy::RetentionDays(7)
            }
            CleanupPolicyConfig::Named(CleanupPolicyName::Keep28d) => {
                CleanupPolicy::RetentionDays(28)
            }
            CleanupPolicyConfig::Retention { retention_days } => {
                CleanupPolicy::RetentionDays(retention_days)
            }
        }
    }
}

impl RootConfig {
    /// Interpret every `[[workflows]]` entry.
    ///
    /// Takes the whole config rather than `(workflows, projects)` because every
    /// caller had both from the same `RootConfig`: one argument means the
    /// projects cannot come from a different config than the workflows.
    pub fn domain_workflows(&self) -> Vec<Workflow> {
        self.workflows
            .iter()
            .map(|w| interpret_workflow(w, self))
            .collect()
    }
}

/// Interpret a parsed config workflow.
///
/// This is the **single** place a `profile` is resolved into concrete
/// mode/output/verification values (#394): everything downstream reads
/// [`Workflow`], whose fields are already concrete, so no other code has to
/// know profiles exist.
///
/// Since #626 the same holds for the task source: it is derived here from
/// the workflow's `projects` against `[[projects]]`, so downstream code
/// keeps reading a plain [`Workflow::source`] and does not have to
/// know the config no longer spells one out.
fn interpret_workflow(config: &WorkflowConfig, root: &RootConfig) -> Workflow {
    Workflow {
        name: config.name.clone(),
        projects: config.projects.clone(),
        source: config
            .projects
            .first()
            .and_then(|first| root.projects.iter().find(|p| &p.name == first))
            .map(|p| p.source.clone())
            .unwrap_or_default(),
        trigger: Trigger::new(config.trigger.clone()),
        mode: config.resolved_mode(),
        agent: config.agent.clone(),
        output: config.resolved_output(),
        on_start: config.on_start.as_ref().map(outcome_action),
        on_success: config.on_success.as_ref().map(outcome_action),
        on_failure: config.on_failure.as_ref().map(outcome_action),
        verification: config.resolved_verification(),
        timeout_secs: config.timeout_secs,
        rubric: config.rubric.clone(),
        tool: config.tool.clone(),
        profile: config.profile,
        cleanup: config.cleanup.map(CleanupPolicy::from),
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

#[cfg(test)]
mod tests {
    use crate::domain::{OutputPolicy, VerificationMode, Workflow, WorkflowMode};

    use super::*;

    fn workflows_from_toml(toml: &str) -> Vec<Workflow> {
        RootConfig::from_toml_str(toml).unwrap().domain_workflows()
    }

    /// The §4.9 example: design (plan/source) + implement (implement/source).
    const SPEC_EXAMPLE: &str = r#"
[[projects]]
name = "github"
source = "github"

[[workflows]]
name = "design"
projects = ["github"]
trigger = { status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
projects = ["github"]
trigger = { status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "source"
on_success = { status = "レビュー待ち" }
"#;

    #[test]
    fn verification_fields_are_wired_from_config() {
        let workflows = workflows_from_toml(
            r#"
[[projects]]
name = "slack"
source = "slack"

[[workflows]]
name = "verified"
projects = ["slack"]
mode = "implement"
agent = "herdr"
output = "source"
verification = "human"
timeout_secs = 600
rubric = "実調査に基づくこと"

[[workflows]]
name = "defaulted"
projects = ["slack"]
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

    #[test]
    fn on_start_is_wired_from_config_and_absent_by_default() {
        let workflows = workflows_from_toml(
            r#"
[[projects]]
name = "github"
source = "github"

[[workflows]]
name = "with-start"
projects = ["github"]
trigger = { status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
on_start = { status = "実装中" }
on_success = { status = "レビュー待ち" }

[[workflows]]
name = "without-start"
projects = ["github"]
trigger = { status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "none"
"#,
        );
        assert_eq!(
            workflows[0]
                .on_start
                .as_ref()
                .and_then(|a| a.status.as_deref()),
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
[[projects]]
name = "slack"
source = "slack"

[[workflows]]
name = "answer"
projects = ["slack"]
trigger = { label = "a" }
profile = "answer"
agent = "herdr"

[[workflows]]
name = "triage"
projects = ["slack"]
trigger = { label = "t" }
profile = "triage"
agent = "herdr"

[[workflows]]
name = "design"
projects = ["slack"]
trigger = { label = "d" }
profile = "design"
agent = "herdr"

[[workflows]]
name = "implement"
projects = ["slack"]
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
[[projects]]
name = "slack"
source = "slack"

[[workflows]]
name = "slack-implement"
projects = ["slack"]
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
[[projects]]
name = "github"
source = "github"

[[workflows]]
name = "design"
projects = ["github"]
trigger = { status = "Design" }
profile = "design"
agent = "herdr"
initial_prompt = "  /grill-me で {設計観点} を詰めてください  "

[[workflows]]
name = "blank"
projects = ["github"]
trigger = {}
profile = "design"
agent = "herdr"
initial_prompt = "   "

[[workflows]]
name = "absent"
projects = ["github"]
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
}
