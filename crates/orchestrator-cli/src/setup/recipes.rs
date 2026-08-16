//! The starting points `totsuka setup` offers (#348).
//!
//! Data only. These mirror the scenario recipes in
//! `ai-docs/development/config-examples.md` — the point of the wizard is that a
//! working `[[workflows]]` block is *chosen*, not composed from questions: the
//! trigger/mode/output/verification combination is what nobody can answer
//! without reading the docs first, which is exactly the reading the wizard is
//! supposed to replace.
//!
//! Each recipe declares which plugins it needs and which blanks the interview
//! must fill; everything else is fixed by the recipe.

use orchestrator_core::config::{OutputPolicy, Profile, VerificationMode, WorkflowMode};

/// A plugin a recipe requires, with the kind its `[plugins.<name>]` section
/// needs when the section is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredPlugin {
    /// `plugin.toml` name.
    pub name: &'static str,
    /// Config `kind` string.
    pub kind: &'static str,
}

/// A `[[workflows]]` entry a recipe contributes.
///
/// A recipe writes **either** a `profile` or the spelled-out
/// `mode`/`output`/`verification` — writing both is a `ProfileConflict` (#394).
/// `output` is the exception and may accompany a profile, which is what lets
/// the profile-based recipes below keep publishing back to their source.
#[derive(Debug, Clone, Copy)]
pub struct RecipeWorkflow {
    /// Workflow name.
    pub name: &'static str,
    /// Task source plugin.
    pub source: &'static str,
    /// Inline-table fragment, or `None` to match everything from the source.
    pub trigger: Option<&'static str>,
    /// One of the four archetypes; `None` spells the keys out instead.
    pub profile: Option<Profile>,
    /// Plan or implement. `None` when `profile` supplies it.
    pub mode: Option<WorkflowMode>,
    /// Agent IDE plugin.
    pub agent: &'static str,
    /// Result handling. `None` leaves it to the profile.
    pub output: Option<OutputPolicy>,
    /// `None` leaves the schema default (or the profile's).
    pub verification: Option<VerificationMode>,
    /// Inline-table fragment.
    pub on_success: Option<&'static str>,
}

/// A blank the interview has to fill for a recipe.
///
/// A blank exists when a plugin's own config has a **required** field that
/// nothing else can supply — not for every knob it exposes. `herdr` and `macos`
/// default every field, so they need no file and no questions; `github` and
/// `slack` do not, and a missing one is a plugin that refuses to initialise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blank {
    /// The Slack member ID to act for (`plugins/slack.toml`).
    SlackUserId,
    /// Which GitHub Project to poll, and as whom (`plugins/github.toml`).
    GitHub,
    /// `[llm]` base URL + model + key reference.
    Llm,
}

impl Blank {
    /// The field an answers file fills this blank with. Used to name the
    /// missing key when a hand-written file omits one.
    pub fn field(self) -> &'static str {
        match self {
            Blank::SlackUserId => "slack_user_id",
            Blank::GitHub => "github",
            Blank::Llm => "llm",
        }
    }
}

/// One selectable starting point.
#[derive(Debug, Clone, Copy)]
pub struct Recipe {
    /// Menu label.
    pub label: &'static str,
    /// One-line explanation shown under the label.
    pub blurb: &'static str,
    /// Plugins to install and enable.
    pub plugins: &'static [RequiredPlugin],
    /// Workflows to write.
    pub workflows: &'static [RecipeWorkflow],
    /// Extra questions this recipe needs answered.
    pub blanks: &'static [Blank],
}

const HERDR: RequiredPlugin = RequiredPlugin {
    name: "herdr",
    kind: "agent_ide",
};
const GITHUB: RequiredPlugin = RequiredPlugin {
    name: "github",
    kind: "task_source",
};
const SLACK: RequiredPlugin = RequiredPlugin {
    name: "slack",
    kind: "task_source",
};
const MACOS: RequiredPlugin = RequiredPlugin {
    name: "macos",
    kind: "notifier",
};

/// The recipes, in menu order.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        label: "Minimal — GitHub Projects + herdr",
        blurb: "One workflow: cards in 実装待ち get implemented and written back.",
        plugins: &[GITHUB, HERDR],
        workflows: &[RecipeWorkflow {
            name: "implement",
            source: "github",
            trigger: Some(r#"{ project_status = "実装待ち" }"#),
            profile: Some(Profile::Implement),
            mode: None,
            agent: "herdr",
            // The profile alone would publish nothing; the card has to be
            // written back for the status transition to mean anything.
            output: Some(OutputPolicy::Source),
            verification: None,
            on_success: Some(r#"{ set_status = "レビュー待ち" }"#),
        }],
        blanks: &[Blank::GitHub],
    },
    Recipe {
        label: "Design → implement handoff",
        blurb: "Two stages with a human review in between (設計待ち, then 実装待ち).",
        plugins: &[GITHUB, HERDR],
        workflows: &[
            RecipeWorkflow {
                name: "design",
                source: "github",
                trigger: Some(r#"{ project_status = "設計待ち" }"#),
                profile: Some(Profile::Design),
                mode: None,
                agent: "herdr",
                output: Some(OutputPolicy::Source),
                verification: None,
                on_success: Some(r#"{ set_status = "設計レビュー待ち" }"#),
            },
            RecipeWorkflow {
                name: "implement",
                source: "github",
                trigger: Some(r#"{ project_status = "実装待ち" }"#),
                profile: Some(Profile::Implement),
                mode: None,
                agent: "herdr",
                output: Some(OutputPolicy::Source),
                verification: None,
                on_success: Some(r#"{ set_status = "レビュー待ち" }"#),
            },
        ],
        blanks: &[Blank::GitHub],
    },
    Recipe {
        label: "Slack — reply as yourself",
        blurb: "Mentions become tasks; the reply goes back to the thread under your name.",
        plugins: &[SLACK, HERDR, MACOS],
        workflows: &[RecipeWorkflow {
            name: "slack-reply",
            source: "slack",
            trigger: None,
            // Behaviour change, not just notation: this recipe used to write
            // `mode = "implement"`, and `answer` resolves to `plan`. It is WF 1
            // in #393 and answering a mention is not implementing — a Slack
            // question that turns out to need code is meant to become a
            // separate `impl:` task through a reaction (#393 D6 / #397) rather
            // than quietly getting a writable worktree.
            profile: Some(Profile::Answer),
            mode: None,
            agent: "herdr",
            output: Some(OutputPolicy::Source),
            verification: None,
            on_success: None,
        }],
        blanks: &[Blank::SlackUserId, Blank::Llm],
    },
    Recipe {
        label: "Human sign-off required",
        blurb: "High-impact work waits for `totsuka task verify`; pairs with the notifier.",
        plugins: &[GITHUB, HERDR, MACOS],
        // No profile: all four resolve `verification` to `llm`, and waiting for
        // a person is the entire point of this recipe. The spelled-out notation
        // stays supported for exactly this — a combination no archetype covers.
        workflows: &[RecipeWorkflow {
            name: "migration",
            source: "github",
            trigger: Some(r#"{ labels = ["migration", "high-risk"] }"#),
            profile: None,
            mode: Some(WorkflowMode::Implement),
            agent: "herdr",
            output: Some(OutputPolicy::Source),
            verification: Some(VerificationMode::Human),
            on_success: None,
        }],
        blanks: &[Blank::GitHub],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_recipe_names_plugins_it_actually_uses() {
        // A workflow referencing a plugin the recipe does not install would
        // produce a config `validate` rejects (`UnknownPluginRef`), and the
        // wizard would have written it without ever running.
        for recipe in RECIPES {
            let declared: Vec<&str> = recipe.plugins.iter().map(|p| p.name).collect();
            for workflow in recipe.workflows {
                assert!(
                    declared.contains(&workflow.source),
                    "{}: workflow `{}` sources `{}`, which the recipe does not install",
                    recipe.label,
                    workflow.name,
                    workflow.source
                );
                assert!(
                    declared.contains(&workflow.agent),
                    "{}: workflow `{}` uses agent `{}`, which the recipe does not install",
                    recipe.label,
                    workflow.name,
                    workflow.agent
                );
            }
        }
    }

    #[test]
    fn every_recipe_has_at_least_one_workflow() {
        // A recipe with no workflow produces a config that runs and does
        // nothing, which is worse than refusing to offer it.
        for recipe in RECIPES {
            assert!(
                !recipe.workflows.is_empty(),
                "{} has no workflow",
                recipe.label
            );
        }
    }

    #[test]
    fn workflow_names_are_unique_within_a_recipe() {
        // Duplicates would be `upsert`ed onto each other, silently dropping one.
        for recipe in RECIPES {
            let mut names: Vec<&str> = recipe.workflows.iter().map(|w| w.name).collect();
            names.sort_unstable();
            let before = names.len();
            names.dedup();
            assert_eq!(
                before,
                names.len(),
                "{}: duplicate workflow name",
                recipe.label
            );
        }
    }

    #[test]
    fn a_recipe_needing_the_llm_says_so() {
        // Repository selection falls back to a picker without `[llm]`, so any
        // recipe whose source cannot carry a repo hint must ask for it.
        let slack = RECIPES
            .iter()
            .find(|r| r.plugins.iter().any(|p| p.name == "slack"))
            .expect("a Slack recipe");
        assert!(slack.blanks.contains(&Blank::Llm));
        assert!(slack.blanks.contains(&Blank::SlackUserId));
    }
}
