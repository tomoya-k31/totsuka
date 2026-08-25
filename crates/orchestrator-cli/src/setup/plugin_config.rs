//! Generating each plugin's `[<name>]` settings table from the interview's
//! answers (#349, moved into `config.toml` by #554).
//!
//! `[plugins.<name>]` says *which* plugins run; `[<name>]` says how to reach
//! the service one talks to. A recipe that installs `github` but leaves
//! `[github]` unwritten produces a setup that looks finished and fails at the
//! first poll, so the wizard writes both.
//!
//! # What gets written
//!
//! Two kinds of key, and no third: what the plugin **requires**, plus what the
//! chosen **recipe's behaviour depends on**. Nothing that is merely available.
//!
//! `herdr` and `macos` default every field of their config, so they get no
//! table at all — an empty one would be indistinguishable from a table a human
//! wrote and then emptied, and [`crate::setup`] would stop offering to fill it
//! in.
//! `github` and `slack` do have required fields, and those are exactly the
//! questions [`super::recipes::Blank`] declares.
//!
//! `slack`'s `bot_token` is the one key of the second kind. The plugin treats it
//! as opt-in (absent = no nudge), but the reply-as-yourself recipe writes it,
//! because a reply posted under your own name raises **no Slack notification at
//! all** — the bot nudge is what makes the whole flow noticeable (ADR-0021).
//! The cost is that skipping its registration now fails the plugin's launch
//! rather than quietly disabling one feature, which is why
//! [`super::required_secrets`] lists it and the checklist says what it buys.
//!
//! # No secret values
//!
//! Tokens appear only as *references* (`keychain:totsuka/github-token`),
//! resolved at dispatch by the orchestrator (F-65). The same rule as
//! `config.toml`, for the same reason — see [ADR-0028].
//!
//! [ADR-0028]: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0028-setup-wizard.md

use serde::Serialize;

use super::answers::Answers;
use super::recipes::Recipe;

/// A `[<name>]` settings table the wizard would add to `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginConfigDraft {
    /// Plugin name, which is also the table name.
    pub name: &'static str,
    /// The complete section, header line included, ready to append.
    pub body: String,
}

/// The required half of `[github]`.
///
/// A struct rather than a hand-built table so the field list is the thing the
/// compiler checks, and so serialisation cannot produce mis-quoted TOML. The
/// names mirror `task_source_github::config::GithubConfig`; the test at the
/// bottom of this file is what keeps them in step.
#[derive(Debug, Serialize)]
struct GithubFile<'a> {
    token: String,
    github_login: &'a str,
}

/// The required half of `[slack]`.
///
/// Three separate tokens, not one: the app token opens Socket Mode, the user
/// token is what makes the reply come from the human rather than a bot
/// (ADR-0016), and the bot token is what the nudge posts as (#305).
#[derive(Debug, Serialize)]
struct SlackFile<'a> {
    app_token: String,
    user_token: String,
    bot_token: String,
    target_user_id: &'a str,
}

/// The plugin settings tables `recipe` needs, given `answers`.
///
/// A plugin whose blanks were not answered is skipped rather than written with
/// a placeholder: [`Answers::from_toml_str`](super::answers::Answers::from_toml_str)
/// already refuses answers that leave a recipe's blanks unfilled, so reaching
/// here without them means the recipe does not need them.
pub fn drafts_for(answers: &Answers, recipe: &Recipe) -> Vec<PluginConfigDraft> {
    let mut drafts = Vec::new();
    for plugin in recipe.plugins {
        let reference = |account: &str| answers.secret_backend.reference(account);
        let body = match plugin.name {
            "github" => answers.github.as_ref().map(|gh| {
                render(
                    "github",
                    &GithubFile {
                        token: reference("github-token"),
                        github_login: &gh.github_login,
                    },
                )
            }),
            "slack" => answers.slack_user_id.as_deref().map(|target_user_id| {
                render(
                    "slack",
                    &SlackFile {
                        app_token: reference("slack-app"),
                        user_token: reference("slack-user"),
                        bot_token: reference("slack-bot"),
                        target_user_id,
                    },
                )
            }),
            // Everything else defaults every field; see the module docs.
            _ => None,
        };
        if let Some(body) = body {
            drafts.push(PluginConfigDraft {
                name: plugin.name,
                body,
            });
        }
    }
    drafts
}

/// Serialise a settings struct **nested under its plugin name**, with a header
/// saying where it came from.
///
/// The nesting is not cosmetic. `GithubFile::projects` serialises as an
/// array-of-tables, and at the top level that is `[[projects]]` — a *different*
/// table from `[github.projects]`, which is what the plugin is handed. Wrapping
/// the struct in a one-key map before serialising is what makes the header
/// lines come out as `[github]` and `[[github.projects]]`; writing `[github]`
/// by hand above a top-level serialisation would not.
///
/// Every field of these structs is a plain scalar, so serialisation is
/// infallible in practice; a panic here would mean the struct grew a shape TOML
/// cannot represent, which is a bug to fix, not a runtime condition to report.
fn render<T: Serialize>(name: &str, file: &T) -> String {
    let mut wrapper = std::collections::BTreeMap::new();
    wrapper.insert(name, file);
    let body = toml::to_string_pretty(&wrapper).expect("plugin config structs are plain scalars");
    format!(
        "# Written by `totsuka setup`. Secret values live in your secret store;\n\
         # only references appear here. Add optional settings below.\n\
         {body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::answers::{
        GitHubAnswer, GitHubOwnerType, LlmAnswer, RepositoryAnswer, SecretBackend,
    };
    use crate::setup::recipes::{Blank, RECIPES};

    fn answers_for(recipe: &str) -> Answers {
        Answers {
            version: super::super::answers::ANSWERS_VERSION,
            recipe: recipe.to_string(),
            repositories: vec![RepositoryAnswer {
                name: "totsuka".to_string(),
                path: "/tmp".to_string(),
                summary: None,
            }],
            secret_backend: SecretBackend::Keychain,
            llm: crate::setup::recipes::by_key(recipe)
                .unwrap()
                .blanks
                .contains(&Blank::Llm)
                .then(|| LlmAnswer {
                    base_url: "https://openrouter.ai/api/v1".to_string(),
                    model: "anthropic/claude-haiku-4-5".to_string(),
                }),
            slack_user_id: crate::setup::recipes::by_key(recipe)
                .unwrap()
                .blanks
                .contains(&Blank::SlackUserId)
                .then(|| "U123456".to_string()),
            github: crate::setup::recipes::by_key(recipe)
                .unwrap()
                .blanks
                .contains(&Blank::GitHub)
                .then(|| GitHubAnswer {
                    owner: "acme".to_string(),
                    owner_type: GitHubOwnerType::Organization,
                    project_number: 7,
                    github_login: "tomoya-k31".to_string(),
                }),
            statuses: Default::default(),
        }
    }

    /// The draft's `[<name>]` table, as the Orchestrator would hand it to the
    /// plugin at `initialize` — TOML parsed, that one table taken, converted
    /// to JSON, and **not** interpreted.
    ///
    /// Secret resolution is deliberately not exercised: the drafts carry
    /// `keychain:` references, and resolving one would reach for the real
    /// Keychain. What this checks is the step before that, which is the whole
    /// of what `setup` is responsible for producing.
    ///
    /// Taking the table *by name* is also the assertion that the section
    /// header is right. A body that serialised at the top level (the
    /// `[[projects]]` vs `[[github.projects]]` hazard `render` guards) has no
    /// `[github]` key at all and fails here.
    fn draft_json(draft: &PluginConfigDraft) -> Result<serde_json::Value, String> {
        let document: toml::Table = draft.body.parse().map_err(|e| format!("{e}"))?;
        let table = document
            .get(draft.name)
            .ok_or_else(|| format!("no `[{}]` table in the rendered section", draft.name))?;
        serde_json::to_value(table).map_err(|e| format!("{e}"))
    }

    #[test]
    fn every_generated_file_is_accepted_by_the_plugin_that_reads_it() {
        // The contract, checked against the plugins' **real** deserializers
        // rather than "it parses as TOML". A missing required key or a renamed
        // one produces a file the orchestrator hands over at `initialize` and
        // the plugin rejects — long after `setup` reported success.
        for recipe in RECIPES {
            let answers = answers_for(recipe.key);
            for draft in drafts_for(&answers, recipe) {
                // Held uninterpreted by the orchestrator first (#554) …
                let json = draft_json(&draft).unwrap_or_else(|e| {
                    panic!(
                        "{}: {} does not parse: {e}\n{}",
                        recipe.label, draft.name, draft.body
                    )
                });

                // … then deserialized by the plugin itself.
                match draft.name {
                    "github" => {
                        serde_json::from_value::<task_source_github::config::GithubConfig>(json)
                            .unwrap_or_else(|e| {
                                panic!("{}: github rejects it: {e}\n{}", recipe.label, draft.body)
                            });
                        // The other half of the contract — that the boards are
                        // configured, not merely parseable — is asserted where
                        // the boards are written since #554: on `build_config`
                        // output, in
                        // `setup::tests::the_board_is_written_and_every_repository_binds_to_it`.
                        // Running `static_config_errors` here would report an
                        // empty board list for every correct setup, because
                        // this table no longer holds one.
                    }
                    "slack" => {
                        serde_json::from_value::<task_source_slack::config::SlackConfig>(json)
                            .unwrap_or_else(|e| {
                                panic!("{}: slack rejects it: {e}\n{}", recipe.label, draft.body)
                            });
                    }
                    other => panic!("no deserializer asserted for `{other}` — add one"),
                }
            }
        }
    }

    #[test]
    fn a_recipe_gets_a_file_for_every_plugin_that_needs_one() {
        // The mapping the other direction: a plugin with a required field but
        // no draft is the failure this module exists to prevent, and it would
        // otherwise be invisible until run time.
        const NEEDS_A_TABLE: &[&str] = &["github", "slack"];
        for recipe in RECIPES {
            let drafts = drafts_for(&answers_for(recipe.key), recipe);
            let written: Vec<&str> = drafts.iter().map(|d| d.name).collect();
            for plugin in recipe.plugins {
                if NEEDS_A_TABLE.contains(&plugin.name) {
                    assert!(
                        written.contains(&plugin.name),
                        "{}: no [{}] table generated",
                        recipe.label,
                        plugin.name
                    );
                } else {
                    assert!(
                        !written.contains(&plugin.name),
                        "{}: {} defaults everything and should get no file",
                        recipe.label,
                        plugin.name
                    );
                }
            }
        }
    }

    #[test]
    fn tokens_appear_only_as_references() {
        for recipe in RECIPES {
            for draft in drafts_for(&answers_for(recipe.key), recipe) {
                for line in draft.body.lines().filter(|l| l.contains("token")) {
                    assert!(
                        line.contains("keychain:totsuka/"),
                        "{}: a token key holds something other than a reference: {line}",
                        draft.name
                    );
                }
            }
        }
    }
}
