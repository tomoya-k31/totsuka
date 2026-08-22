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
/// One Project status column a recipe's workflows name, and what to call it.
///
/// The recipes used to write these as literals, in Japanese, because that is
/// what the author's own board uses. On any other board the trigger matches an
/// option that does not exist, and the failure is the quiet kind: the config is
/// valid, `doctor` is green, and `run` simply never picks anything up.
///
/// [`key`](Self::key) is the placeholder name; the workflow fragments spell it
/// `{{key}}` and [`render_fragment`] substitutes it.
#[derive(Debug, Clone, Copy)]
pub struct StatusSlot {
    /// Placeholder name, as it appears inside `{{…}}` in a fragment.
    pub key: &'static str,
    /// What the interview asks.
    pub prompt: &'static str,
    /// The value an answers file written before these were asked replays with.
    ///
    /// **Deliberately the old literal.** A default that reproduces yesterday's
    /// behaviour is what lets [`ANSWERS_VERSION`](super::answers::ANSWERS_VERSION)
    /// stay where it is: an older file without `statuses` still means exactly
    /// what it meant.
    pub default: &'static str,
}

/// Substitute `{{key}}` placeholders in a recipe fragment.
///
/// Unknown placeholders are left alone rather than blanked — a literal
/// `{{foo}}` in someone's config is a visible bug, whereas an empty string
/// would be a trigger that silently matches nothing. The consistency test
/// below is what keeps one from ever being written.
pub fn render_fragment(
    fragment: &str,
    statuses: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = fragment.to_string();
    for (key, value) in statuses {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    out
}

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
    /// Stable identifier written into an answers file (#466).
    ///
    /// **Not the menu position and not the label.** An answers file is meant
    /// to be kept in a dotfiles repository and replayed on another machine at
    /// another version, so what it names a recipe by has to survive the menu
    /// being reordered and the label being reworded. A positional index
    /// survives neither: inserting one recipe silently makes every older file
    /// select its neighbour, with the range check still passing and the format
    /// version unmoved.
    ///
    /// Renaming a key is therefore a breaking change to the answers format
    /// (bump [`ANSWERS_VERSION`](super::answers::ANSWERS_VERSION)); reordering
    /// or inserting recipes is not.
    pub key: &'static str,
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
    /// Project status columns this recipe's fragments name, as `{{key}}`.
    pub statuses: &'static [StatusSlot],
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

/// The recipe carrying `key` in `recipes`, or `None` when nothing does.
///
/// Takes the slice rather than reaching for [`RECIPES`] because the answers
/// parser is handed one (tests inject a reordered or trimmed list to prove the
/// key survives it). [`by_key`] is the same lookup against the real set, so
/// there is exactly one implementation of "what does this key mean" — the
/// question a file replayed on another machine is asking.
pub fn by_key_in<'a>(recipes: &'a [Recipe], key: &str) -> Option<&'a Recipe> {
    recipes.iter().find(|r| r.key == key)
}

/// [`by_key_in`] against [`RECIPES`].
pub fn by_key(key: &str) -> Option<&'static Recipe> {
    by_key_in(RECIPES, key)
}

/// The recipes, in menu order.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        key: "minimal-github-herdr",
        label: "Minimal — GitHub Projects + herdr",
        blurb: "One workflow: cards in your implement column get implemented and written back.",
        plugins: &[GITHUB, HERDR],
        workflows: &[RecipeWorkflow {
            name: "implement",
            source: "github",
            trigger: Some(r#"{ project_status = "{{implement_status}}" }"#),
            profile: Some(Profile::Implement),
            mode: None,
            agent: "herdr",
            // The profile alone would publish nothing; the card has to be
            // written back for the status transition to mean anything.
            output: Some(OutputPolicy::Source),
            verification: None,
            on_success: Some(r#"{ set_status = "{{implement_done_status}}" }"#),
        }],
        blanks: &[Blank::GitHub],
        statuses: &[
            StatusSlot {
                key: "implement_status",
                prompt: "Status column tasks wait in before being implemented",
                default: "実装待ち",
            },
            StatusSlot {
                key: "implement_done_status",
                prompt: "Status column they move to once implemented",
                default: "レビュー待ち",
            },
        ],
    },
    Recipe {
        key: "design-implement-handoff",
        label: "Design → implement handoff",
        blurb: "Two stages — design, then implement — with a human review in between.",
        plugins: &[GITHUB, HERDR],
        workflows: &[
            RecipeWorkflow {
                name: "design",
                source: "github",
                trigger: Some(r#"{ project_status = "{{design_status}}" }"#),
                profile: Some(Profile::Design),
                mode: None,
                agent: "herdr",
                output: Some(OutputPolicy::Source),
                verification: None,
                on_success: Some(r#"{ set_status = "{{design_done_status}}" }"#),
            },
            RecipeWorkflow {
                name: "implement",
                source: "github",
                trigger: Some(r#"{ project_status = "{{implement_status}}" }"#),
                profile: Some(Profile::Implement),
                mode: None,
                agent: "herdr",
                output: Some(OutputPolicy::Source),
                verification: None,
                on_success: Some(r#"{ set_status = "{{implement_done_status}}" }"#),
            },
        ],
        blanks: &[Blank::GitHub],
        statuses: &[
            StatusSlot {
                key: "design_status",
                prompt: "Status column tasks wait in before being designed",
                default: "設計待ち",
            },
            StatusSlot {
                key: "design_done_status",
                prompt: "Status column they move to once designed",
                default: "設計レビュー待ち",
            },
            StatusSlot {
                key: "implement_status",
                prompt: "Status column tasks wait in before being implemented",
                default: "実装待ち",
            },
            StatusSlot {
                key: "implement_done_status",
                prompt: "Status column they move to once implemented",
                default: "レビュー待ち",
            },
        ],
    },
    Recipe {
        key: "slack-reply-as-yourself",
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
        statuses: &[],
    },
    Recipe {
        key: "human-sign-off",
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
        // Triggers on labels, not on a status column.
        statuses: &[],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Placeholders and slots must agree **in both directions**.
    ///
    /// One direction: a `{{…}}` with no slot is never asked, never
    /// substituted, and lands in the operator's `config.toml` as a literal
    /// `{{foo}}` — a trigger that matches nothing.
    ///
    /// The other: a slot no fragment uses asks a question whose answer is
    /// silently discarded, which is worse than not asking, because the
    /// operator believes they configured something.
    #[test]
    fn declared_status_slots_and_placeholders_agree() {
        fn placeholders(fragment: &str) -> Vec<String> {
            let mut found = Vec::new();
            let mut rest = fragment;
            while let Some(start) = rest.find("{{") {
                let after = &rest[start + 2..];
                let Some(end) = after.find("}}") else { break };
                found.push(after[..end].to_string());
                rest = &after[end + 2..];
            }
            found
        }

        for recipe in RECIPES {
            let used: Vec<String> = recipe
                .workflows
                .iter()
                .flat_map(|w| {
                    w.trigger
                        .into_iter()
                        .chain(w.on_success)
                        .flat_map(placeholders)
                })
                .collect();
            let declared: Vec<&str> = recipe.statuses.iter().map(|s| s.key).collect();

            for key in &used {
                assert!(
                    declared.contains(&key.as_str()),
                    "recipe `{}` writes `{{{{{key}}}}}` but declares no slot for it — it would \
                     reach the config as a literal",
                    recipe.key
                );
            }
            for slot in recipe.statuses {
                assert!(
                    used.iter().any(|k| k == slot.key),
                    "recipe `{}` asks for `{}` but no fragment uses it — the answer would be \
                     discarded",
                    recipe.key,
                    slot.key
                );
            }
        }
    }

    /// The defaults must reproduce what the recipes used to hard-code, or an
    /// answers file written before the interview asked would start meaning
    /// something else — the thing `ANSWERS_VERSION` exists to prevent.
    #[test]
    fn substituting_the_defaults_reproduces_the_original_fragments() {
        let recipe = by_key("design-implement-handoff").expect("recipe");
        let filled: std::collections::HashMap<String, String> = recipe
            .statuses
            .iter()
            .map(|s| (s.key.to_string(), s.default.to_string()))
            .collect();
        let rendered: Vec<String> = recipe
            .workflows
            .iter()
            .flat_map(|w| w.trigger.into_iter().chain(w.on_success))
            .map(|f| render_fragment(f, &filled))
            .collect();
        assert_eq!(
            rendered,
            vec![
                r#"{ project_status = "設計待ち" }"#,
                r#"{ set_status = "設計レビュー待ち" }"#,
                r#"{ project_status = "実装待ち" }"#,
                r#"{ set_status = "レビュー待ち" }"#,
            ]
        );
    }

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
