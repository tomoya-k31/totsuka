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
    /// The boards to poll (#542). The wizard asks about one, so this is always
    /// a single entry — the plugin's schema is a list because several boards
    /// are configurable by hand.
    ///
    /// **Last field on purpose**: TOML puts every scalar written after an
    /// array-of-tables *inside* that table, and the `toml` serializer emits
    /// fields in declaration order. A scalar declared below this one would be
    /// written into the `[[projects]]` block and rejected by the plugin as an
    /// unknown key.
    projects: Vec<GithubProjectFile<'a>>,
}

/// One `[[github.projects]]` entry in the generated `[github]` table.
#[derive(Debug, Serialize)]
struct GithubProjectFile<'a> {
    owner: &'a str,
    owner_type: &'static str,
    project_number: i64,
    /// Which repositories this board tracks — required and non-empty since
    /// #542, because it is also the repository → board mapping.
    ///
    /// The wizard fills it with **every** repository the operator configured.
    /// That matches what a single-board setup meant before #542 (an omitted
    /// `repos` accepted any repository on the board) and it is the only answer
    /// derivable without asking a question the wizard does not ask.
    repos: Vec<&'a str>,
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
                        projects: vec![GithubProjectFile {
                            owner: &gh.owner,
                            owner_type: gh.owner_type.as_str(),
                            project_number: gh.project_number,
                            repos: answers
                                .repositories
                                .iter()
                                .map(|r| r.name.as_str())
                                .collect(),
                        }],
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
                        let config = serde_json::from_value::<
                            task_source_github::config::GithubConfig,
                        >(json)
                        .unwrap_or_else(|e| {
                            panic!("{}: github rejects it: {e}\n{}", recipe.label, draft.body)
                        });
                        // Deserializing is not the whole contract. `projects`
                        // and `repos` are lists, so an *empty* one parses
                        // cleanly and is then rejected by `config validate` —
                        // exactly the "setup reported success, the plugin
                        // refuses later" failure this test exists to stop.
                        let errors = task_source_github::client::static_config_errors(&config);
                        assert!(
                            errors.is_empty(),
                            "{}: github accepts the file but validation rejects it: {errors:?}\n{}",
                            recipe.label,
                            draft.body
                        );
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

    /// The generated board claims the repositories the operator configured.
    /// An empty `repos` would parse and then fail validation, and a board that
    /// claims nothing routes nothing (#542).
    #[test]
    fn the_generated_board_tracks_every_configured_repository() {
        let recipe = RECIPES
            .iter()
            .find(|r| r.plugins.iter().any(|p| p.name == "github"))
            .expect("a recipe with the github plugin");
        let mut answers = answers_for(recipe.key);
        answers.repositories.push(RepositoryAnswer {
            name: "dotfiles".to_string(),
            path: "/tmp/dotfiles".to_string(),
            summary: None,
        });
        let draft = drafts_for(&answers, recipe)
            .into_iter()
            .find(|d| d.name == "github")
            .expect("a github draft");
        // Asserted through the plugin's own deserializer rather than on the
        // rendered text: `toml` breaks a two-element array across lines, so a
        // string match would be testing the formatter, not the contents.
        let config: task_source_github::config::GithubConfig =
            serde_json::from_value(draft_json(&draft).unwrap()).unwrap();
        assert_eq!(config.projects.len(), 1);
        assert_eq!(config.projects[0].repos, ["totsuka", "dotfiles"]);
    }

    /// TOML puts a scalar written after an array-of-tables *inside* it, so the
    /// field order in `GithubFile` is load-bearing: `github_login` must land in
    /// `[github]`, not inside `[[github.projects]]`.
    ///
    /// Checked through the parser rather than by comparing byte offsets: since
    /// #554 the section is nested, and a text search for `[[projects]]` would
    /// now miss the very hazard it was written for.
    #[test]
    fn top_level_keys_are_written_before_the_projects_block() {
        let recipe = RECIPES
            .iter()
            .find(|r| r.plugins.iter().any(|p| p.name == "github"))
            .expect("a recipe with the github plugin");
        let draft = drafts_for(&answers_for(recipe.key), recipe)
            .into_iter()
            .find(|d| d.name == "github")
            .expect("a github draft");
        let document: toml::Table = draft.body.parse().expect("the section parses");
        let github = document["github"].as_table().expect("[github] is a table");
        assert!(
            github.contains_key("github_login"),
            "github_login was swallowed by the projects block:\n{}",
            draft.body
        );
        assert!(
            github["projects"][0].get("github_login").is_none(),
            "github_login landed inside [[github.projects]]:\n{}",
            draft.body
        );
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

    #[test]
    fn the_owner_type_the_user_picked_is_what_gets_written() {
        // Defaulting this wrong is not a formatting nit: `user(login:)` and
        // `organization(login:)` are different GraphQL roots, so an org board
        // polled as a user returns nothing, with no error to explain it.
        let mut answers = answers_for(RECIPES[0].key);
        let github = answers.github.as_mut().unwrap();
        github.owner_type = GitHubOwnerType::Organization;
        let drafts = drafts_for(&answers, &RECIPES[0]);
        assert!(drafts[0].body.contains(r#"owner_type = "organization""#));

        answers.github.as_mut().unwrap().owner_type = GitHubOwnerType::User;
        let drafts = drafts_for(&answers, &RECIPES[0]);
        assert!(drafts[0].body.contains(r#"owner_type = "user""#));
    }
}
