//! What the interview collected, and the non-interactive form of it (#348).
//!
//! The wizard is interactive by design, but the collected answers are a plain
//! serialisable value, and `--answers <file>` replays one. **That is a product
//! feature** (#466): it is the pair to `--save-answers`, and the setup playbook
//! documents it as the way to bring a machine up from a file kept in a dotfiles
//! repository. It also happens to be the only way to test the wizard
//! end-to-end — a CLI E2E spawns `totsuka` as a child process, which has no
//! terminal — but that is a consequence of the feature, not its purpose.
//!
//! It was hidden from `--help` until #466 while the playbook recommended it in
//! the same breath as apologising for the flag not appearing there. The
//! apology was the tell.
//!
//! **`setup` never writes a secret value into one.** It asks which backend to
//! use, writes the *references* into the config
//! (`keychain:totsuka/slack-user`), and prints the commands to register the
//! values — which never pass through this process at all. That is what makes a
//! generated file safe to commit.
//!
//! It is a property of what the wizard writes, **not** of what the format can
//! hold: `path`, `summary`, `base_url` and friends are free-form strings, and a
//! hand-edited file can contain anything a human puts there. Saying the format
//! "cannot hold a secret" would be a stronger claim than the types support, and
//! the wrong one to reassure someone with before they `git add` the file.
//!
//! # Format stability
//!
//! A replayed file is read by a *different build* than wrote it, so the format
//! is a contract:
//!
//! - [`ANSWERS_VERSION`] is bumped by any change that would make an older file
//!   mean something different, and a mismatch is **refused, never guessed**.
//! - The version is read before anything else, so the refusal survives a field
//!   changing shape (see [`Answers::from_toml_str`]).
//! - Fields name what they select rather than counting to it — see
//!   [`Answers::recipe`], which is why reordering the recipe menu does not
//!   silently repoint existing files.

use serde::{Deserialize, Serialize};

use super::recipes::{Blank, Recipe};

/// Where secret references point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretBackend {
    /// macOS Keychain (`keychain:totsuka/<account>`).
    Keychain,
    /// 1Password (`op://<vault>/<item>/<field>`).
    OnePassword,
    /// Environment variables (`${TOTSUKA_...}`).
    Env,
}

impl SecretBackend {
    /// The reference string this backend uses for `account`.
    ///
    /// Conventional rather than asked: the names only have to be consistent
    /// between `config.toml` and the register-these commands printed at the
    /// end, and inventing a naming scheme is not a decision worth a prompt.
    pub fn reference(self, account: &str) -> String {
        match self {
            SecretBackend::Keychain => format!("keychain:totsuka/{account}"),
            SecretBackend::OnePassword => format!("op://Dev/totsuka/{account}"),
            SecretBackend::Env => {
                format!("${{TOTSUKA_{}}}", account.to_uppercase().replace('-', "_"))
            }
        }
    }

    /// A copy-pasteable command that registers `account`, when the backend has
    /// one. `Env` has no register step — the user exports it.
    pub fn register_command(self, account: &str) -> Option<String> {
        match self {
            SecretBackend::Keychain => Some(format!(
                "security add-generic-password -U -s totsuka -a {account} -w '<paste the value>'"
            )),
            SecretBackend::OnePassword => Some(format!(
                "op item edit totsuka {account}='<paste the value>'   # or create the item first"
            )),
            SecretBackend::Env => None,
        }
    }
}

/// A repository to register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryAnswer {
    /// Name used in `config.toml` and by workflows.
    pub name: String,
    /// Path as typed (`~` is expanded at load time, not here).
    pub path: String,
    /// Optional one-liner for LLM repository selection.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Which GitHub account owns the Project board, since the two GraphQL roots
/// differ (`user(login:)` vs `organization(login:)`) and guessing wrong makes
/// every poll fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitHubOwnerType {
    /// A personal account.
    User,
    /// An organization.
    Organization,
}

impl GitHubOwnerType {
    /// The value the `[github]` table uses.
    pub fn as_str(self) -> &'static str {
        match self {
            GitHubOwnerType::User => "user",
            GitHubOwnerType::Organization => "organization",
        }
    }
}

/// What the `[github]` table needs beyond the token.
///
/// The GitHub plugin requires all of these — none has a default — so a recipe
/// that installs it cannot produce a working setup without asking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitHubAnswer {
    /// Account or organization that owns the Project board.
    pub owner: String,
    /// Which of the two GraphQL roots `owner` lives under.
    pub owner_type: GitHubOwnerType,
    /// Project number, as it appears in the board's URL.
    pub project_number: i64,
    /// The login whose assigned cards are picked up.
    pub github_login: String,
}

/// The `[llm]` block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmAnswer {
    /// OpenAI-compatible base URL.
    pub base_url: String,
    /// Model id.
    pub model: String,
}

/// Everything the interview collects.
///
/// `deny_unknown_fields` matches the config layer's own strictness: a typo in a
/// hand-written answers file should fail loudly, not be ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answers {
    /// Format version, so a future change can reject an old file rather than
    /// misread it.
    ///
    /// It only guards changes that *move* the version. A field whose meaning
    /// drifts underneath a stable version is invisible to it — which is why
    /// [`recipe`](Self::recipe) names its recipe rather than counting to it.
    pub version: u32,
    /// Which recipe to start from, by its stable
    /// [`key`](super::recipes::Recipe::key) — **not** its menu position (#466).
    pub recipe: String,
    /// Repositories to register (at least one).
    pub repositories: Vec<RepositoryAnswer>,
    /// Where secret references point.
    pub secret_backend: SecretBackend,
    /// Filled when the recipe asks for it.
    #[serde(default)]
    pub llm: Option<LlmAnswer>,
    /// Slack member id, when the recipe asks for it.
    #[serde(default)]
    pub slack_user_id: Option<String>,
    /// GitHub Project coordinates, when the recipe asks for them.
    #[serde(default)]
    pub github: Option<GitHubAnswer>,
    /// Project status column names, keyed by the recipe's
    /// [`StatusSlot::key`](super::recipes::StatusSlot::key).
    ///
    /// **`ANSWERS_VERSION` deliberately does not move for this.** A file
    /// written before the interview asked has no `statuses`, and rather than
    /// being filled in from a default it is **refused by name**, listing the
    /// keys to add. The version guards changes that make an old file mean
    /// something *different*; refusing to read one is stricter, not different.
    /// Do not "fix" the version here.
    ///
    /// The alternative — falling back to a declared default — was rejected on
    /// purpose: it would write column names the operator never chose, and a
    /// column name that does not exist on the board fails silently (valid
    /// config, green `doctor`, a `run` that picks nothing up). That is the
    /// failure this whole change removes; leaving it on the replay path would
    /// have kept it exactly where the playbook sends a second machine.
    ///
    /// A `BTreeMap`, not a `HashMap`: `--save-answers` writes this file into a
    /// dotfiles repository, and a hash order would churn the diff on every
    /// regeneration.
    ///
    /// A map rather than named fields because the slots belong to the recipe,
    /// not to any one plugin: they end up in `config.toml`'s workflows, while
    /// [`github`](Self::github) describes the `[github]` table.
    #[serde(default)]
    pub statuses: std::collections::BTreeMap<String, String>,
}

/// The version this build writes and accepts.
///
/// `2` since #466 made `recipe` a stable key instead of a menu index. Version
/// `1` files are rejected rather than translated: the flag was hidden until
/// #466 and no `1` file was ever written by a documented path, so a
/// compatibility shim would carry the index semantics — the very thing being
/// removed — forward forever for no reader.
pub const ANSWERS_VERSION: u32 = 2;

/// Failure to read an answers file.
#[derive(Debug, thiserror::Error)]
pub enum AnswersError {
    /// The file could not be read.
    #[error("cannot read answers file {path}: {source}")]
    Read {
        /// Path that was tried.
        path: String,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The file is not valid TOML, or has unknown/missing fields.
    #[error("{path} is not a valid answers file: {source}")]
    Parse {
        /// Path that was tried.
        path: String,
        /// Underlying error.
        source: toml::de::Error,
    },
    /// Written by a different version of the format.
    #[error(
        "{path} is version {found}, but this totsuka writes version {expected} → regenerate it \
         by running `totsuka setup` interactively"
    )]
    Version {
        /// Path that was tried.
        path: String,
        /// Version in the file.
        found: u32,
        /// Version this build understands.
        expected: u32,
    },
    /// No recipe carries the key the file names.
    #[error("{path} selects recipe `{found}`, which does not exist → known keys: {known}")]
    UnknownRecipe {
        /// Path that was tried.
        path: String,
        /// Key in the file.
        found: String,
        /// The keys that do exist, comma-separated — a typo'd key is otherwise
        /// unanswerable without reading the source.
        known: String,
    },
    /// No repository was given.
    #[error("{path} lists no repositories → at least one is required")]
    NoRepositories {
        /// Path that was tried.
        path: String,
    },
    /// A `[statuses]` key the chosen recipe does not declare.
    ///
    /// `deny_unknown_fields` only guards struct fields; `statuses` is a map,
    /// and substitution reads from the **recipe** side, so an unrecognised key
    /// would be dropped without a word and the declared default written in its
    /// place — `setup --answers` succeeds, `validate` passes, `doctor` is
    /// green, and `run` picks nothing up. That is the exact failure this whole
    /// change exists to remove, so it cannot be left open on the path the
    /// playbook recommends for a second machine.
    #[error(
        "{path} sets status `{found}`, which `{recipe}` does not use → this recipe names: {known}"
    )]
    UnknownStatus {
        /// Path that was tried.
        path: String,
        /// Label of the selected recipe.
        recipe: &'static str,
        /// The key in the file.
        found: String,
        /// The keys the recipe declares, comma-separated.
        known: String,
    },
    /// A `[statuses]` value that cannot name a column.
    ///
    /// `contains_key` is not enough: `""` passes every presence check and
    /// renders `{ project_status = "" }`, which no task can match — a valid
    /// config, a green `doctor`, and a `run` that picks nothing up. Surrounding
    /// whitespace does the same thing while *looking* right. Neither is
    /// silently trimmed: altering what the operator wrote is how a file stops
    /// meaning what it says.
    #[error("{path} sets status `{key}` to a value that {reason} → give the column's exact name")]
    InvalidStatus {
        /// Path that was tried.
        path: String,
        /// The offending key.
        key: &'static str,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// A `[statuses]` key the chosen recipe declares but the file omits.
    #[error(
        "{path} selects `{recipe}`, which needs status `{missing}` → add it under `[statuses]` \
         (this recipe names: {known})"
    )]
    MissingStatus {
        /// Path that was tried.
        path: String,
        /// Label of the selected recipe.
        recipe: &'static str,
        /// The first key that is absent.
        missing: &'static str,
        /// Every key the recipe declares, comma-separated.
        known: String,
    },
    /// The chosen recipe needs a field the file does not set.
    #[error("{path} selects `{recipe}`, which needs `{field}` → add it to the answers file")]
    MissingBlank {
        /// Path that was tried.
        path: String,
        /// Label of the recipe that requires it.
        recipe: &'static str,
        /// Key that is missing.
        field: &'static str,
    },
}

impl Answers {
    /// Parse an answers file, rejecting anything this build cannot act on.
    ///
    /// The checks here are the ones whose failure would otherwise surface much
    /// later as a confusing config error.
    ///
    /// The interview cannot leave a blank unfilled — it asks for exactly the
    /// ones its recipe declares — but a hand-written file can, so the recipe's
    /// blanks are checked here too. Skipping that check produces a config that
    /// loads with, say, `verification = "llm"` and no `[llm]` block, or a Slack
    /// setup with nobody to act for: a wizard that reported success and left
    /// the failure for run time.
    pub fn from_toml_str(path: &str, text: &str, recipes: &[Recipe]) -> Result<Self, AnswersError> {
        // The version is read **before** the rest, against a struct that
        // ignores every other key. Deserializing the whole file first would
        // report a wrong-version file as a type error on whichever field
        // changed shape — a v1 file says `recipe = 0`, and "invalid type:
        // integer" is not an answer anyone can act on. Reading the version
        // first is what makes the field do the job it exists for.
        #[derive(Deserialize)]
        struct VersionOnly {
            version: u32,
        }
        let probe: VersionOnly = toml::from_str(text).map_err(|source| AnswersError::Parse {
            path: path.to_string(),
            source,
        })?;
        if probe.version != ANSWERS_VERSION {
            return Err(AnswersError::Version {
                path: path.to_string(),
                found: probe.version,
                expected: ANSWERS_VERSION,
            });
        }
        let answers: Answers = toml::from_str(text).map_err(|source| AnswersError::Parse {
            path: path.to_string(),
            source,
        })?;
        let Some(recipe) = super::recipes::by_key_in(recipes, &answers.recipe) else {
            return Err(AnswersError::UnknownRecipe {
                path: path.to_string(),
                found: answers.recipe.clone(),
                known: recipes.iter().map(|r| r.key).collect::<Vec<_>>().join(", "),
            });
        };
        if answers.repositories.is_empty() {
            return Err(AnswersError::NoRepositories {
                path: path.to_string(),
            });
        }
        // Sorted so the message is stable: a `HashMap` would otherwise name
        // the offending key differently on each run.
        let mut declared: Vec<&str> = recipe.statuses.iter().map(|s| s.key).collect();
        declared.sort_unstable();
        let mut unknown: Vec<&str> = answers
            .statuses
            .keys()
            .map(String::as_str)
            .filter(|k| !declared.contains(k))
            .collect();
        unknown.sort_unstable();
        if let Some(found) = unknown.first() {
            return Err(AnswersError::UnknownStatus {
                path: path.to_string(),
                recipe: recipe.label,
                found: (*found).to_string(),
                known: if declared.is_empty() {
                    "none — this recipe uses no status columns".to_string()
                } else {
                    declared.join(", ")
                },
            });
        }
        // Unknown before missing, deliberately: a typo trips both, and
        // "`implement_statuss` is not a key" points at the line to fix, while
        // "`implement_status` is missing" leaves the operator hunting.
        if let Some(slot) = recipe
            .statuses
            .iter()
            .find(|slot| !answers.statuses.contains_key(slot.key))
        {
            return Err(AnswersError::MissingStatus {
                path: path.to_string(),
                recipe: recipe.label,
                missing: slot.key,
                known: declared.join(", "),
            });
        }
        for slot in recipe.statuses {
            let value = &answers.statuses[slot.key];
            let reason = if value.trim().is_empty() {
                Some("is empty")
            } else if value.trim() != value {
                Some("has leading or trailing whitespace")
            } else {
                None
            };
            if let Some(reason) = reason {
                return Err(AnswersError::InvalidStatus {
                    path: path.to_string(),
                    key: slot.key,
                    reason,
                });
            }
        }
        for blank in recipe.blanks {
            let filled = match blank {
                Blank::Llm => answers.llm.is_some(),
                Blank::SlackUserId => answers.slack_user_id.is_some(),
                Blank::GitHub => answers.github.is_some(),
            };
            if !filled {
                return Err(AnswersError::MissingBlank {
                    path: path.to_string(),
                    recipe: recipe.label,
                    field: blank.field(),
                });
            }
        }
        Ok(answers)
    }

    /// Serialise for `--save-answers`.
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("Answers is always serialisable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::recipes::RECIPES;

    /// A typo'd status key must be refused, not dropped.
    ///
    /// `deny_unknown_fields` cannot see into a map, and substitution reads
    /// from the recipe side, so silently ignoring it would write the default
    /// and leave the operator with a green `doctor` and a `run` that picks
    /// nothing up — on the path the playbook recommends for a second machine.
    #[test]
    fn an_unrecognised_status_key_is_refused() {
        let text = r#"
version = 2
recipe = "minimal-github-herdr"
secret_backend = "keychain"

[[repositories]]
name = "totsuka"
path = "~/Workspace/totsuka"

[github]
owner = "tomoya-k31"
owner_type = "user"
project_number = 1
github_login = "tomoya-k31"

[statuses]
implement_statuss = "Ready"
"#;
        let err = Answers::from_toml_str("a.toml", text, RECIPES)
            .expect_err("a key the recipe does not use must be refused");
        let message = err.to_string();
        assert!(message.contains("implement_statuss"), "{message}");
        // The message has to say what the recipe *does* use, or the operator
        // cannot tell a typo from a key that moved.
        assert!(message.contains("implement_status"), "{message}");
    }

    /// Present is not the same as usable.
    ///
    /// `""` satisfies `contains_key`, renders `{ project_status = "" }`, and no
    /// task can ever match it — valid config, green `doctor`, a `run` that
    /// picks nothing up. Surrounding whitespace does the same while looking
    /// right on screen.
    #[test]
    fn a_status_value_that_cannot_name_a_column_is_refused() {
        let file = |value: &str| {
            format!(
                r#"
version = 2
recipe = "minimal-github-herdr"
secret_backend = "keychain"

[[repositories]]
name = "totsuka"
path = "~/Workspace/totsuka"

[github]
owner = "tomoya-k31"
owner_type = "user"
project_number = 1
github_login = "tomoya-k31"

[statuses]
implement_status = "{value}"
implement_done_status = "In review"
"#
            )
        };
        for (value, reason) in [
            ("", "is empty"),
            ("   ", "is empty"),
            (" Todo", "whitespace"),
        ] {
            let err = Answers::from_toml_str("a.toml", &file(value), RECIPES)
                .expect_err(&format!("{value:?} must be refused"));
            let message = err.to_string();
            assert!(message.contains("implement_status"), "{message}");
            assert!(message.contains(reason), "{value:?}: {message}");
        }

        // …and the same file with a usable value parses, so the check is not
        // simply refusing everything.
        Answers::from_toml_str("a.toml", &file("Todo"), RECIPES).expect("a real name is fine");
    }

    /// A recipe with no status columns must still refuse a `[statuses]` key
    /// rather than accept a section that does nothing.
    #[test]
    fn a_status_key_on_a_recipe_without_columns_is_refused() {
        let text = r#"
version = 2
recipe = "human-sign-off"
secret_backend = "keychain"

[[repositories]]
name = "totsuka"
path = "~/Workspace/totsuka"

[github]
owner = "tomoya-k31"
owner_type = "user"
project_number = 1
github_login = "tomoya-k31"

[statuses]
implement_status = "Ready"
"#;
        let err = Answers::from_toml_str("a.toml", text, RECIPES).expect_err("must be refused");
        assert!(err.to_string().contains("uses no status columns"), "{err}");
    }

    fn sample() -> Answers {
        Answers {
            version: ANSWERS_VERSION,
            recipe: RECIPES[0].key.to_string(),
            repositories: vec![RepositoryAnswer {
                name: "totsuka".to_string(),
                path: "~/Workspace/totsuka".to_string(),
                summary: Some("the orchestrator".to_string()),
            }],
            secret_backend: SecretBackend::Keychain,
            llm: None,
            slack_user_id: None,
            github: Some(GitHubAnswer {
                owner: "tomoya-k31".to_string(),
                owner_type: GitHubOwnerType::User,
                project_number: 1,
                github_login: "tomoya-k31".to_string(),
            }),
            statuses: RECIPES[0]
                .statuses
                .iter()
                .map(|s| (s.key.to_string(), s.default.to_string()))
                .collect(),
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let original = sample();
        let parsed = Answers::from_toml_str("x", &original.to_toml(), RECIPES).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn a_typo_is_rejected_rather_than_ignored() {
        let text = "version = 2\nrecipe = \"minimal-github-herdr\"\n\
                    secret_backend = \"keychain\"\nrepositorys = []\n";
        let err = Answers::from_toml_str("x", text, RECIPES).unwrap_err();
        assert!(matches!(err, AnswersError::Parse { .. }), "{err}");
    }

    #[test]
    fn a_future_version_is_refused_with_a_way_out() {
        let mut answers = sample();
        answers.version = 99;
        let err = Answers::from_toml_str("x", &answers.to_toml(), RECIPES).unwrap_err();
        assert!(
            matches!(err, AnswersError::Version { found: 99, .. }),
            "{err}"
        );
        assert!(err.to_string().contains("totsuka setup"), "{err}");
    }

    #[test]
    fn an_unknown_recipe_key_is_refused_and_lists_the_real_ones() {
        let mut answers = sample();
        answers.recipe = "no-such-recipe".to_string();
        let err = Answers::from_toml_str("x", &answers.to_toml(), RECIPES).unwrap_err();
        assert!(
            matches!(&err, AnswersError::UnknownRecipe { found, .. } if found == "no-such-recipe"),
            "{err}"
        );
        // A typo'd key is otherwise unanswerable without reading the source.
        for recipe in RECIPES {
            assert!(
                err.to_string().contains(recipe.key),
                "the message lists `{}`: {err}",
                recipe.key
            );
        }
    }

    /// A menu index would have made this file mean a different recipe the
    /// moment one was inserted above it; the key is what survives (#466).
    #[test]
    fn a_recipe_key_survives_the_menu_being_reordered() {
        let answers = sample();
        let reordered: Vec<Recipe> = RECIPES.iter().rev().copied().collect();
        let parsed = Answers::from_toml_str("x", &answers.to_toml(), &reordered)
            .expect("the key still resolves after a reorder");
        assert_eq!(parsed.recipe, answers.recipe);
    }

    #[test]
    fn no_repositories_is_refused() {
        let mut answers = sample();
        answers.repositories.clear();
        let err = Answers::from_toml_str("x", &answers.to_toml(), RECIPES).unwrap_err();
        assert!(matches!(err, AnswersError::NoRepositories { .. }), "{err}");
    }

    #[test]
    fn a_recipe_whose_blanks_are_unfilled_is_refused() {
        // Only a hand-written file can get here — the interview asks for
        // exactly the blanks its recipe declares. Accepting it would write a
        // config that loads and then fails at run time, which is the failure
        // mode the wizard exists to prevent.
        let slack = RECIPES
            .iter()
            .find(|r| r.blanks.contains(&Blank::SlackUserId))
            .expect("a recipe with blanks");

        let mut answers = sample();
        answers.recipe = slack.key.to_string();
        // The sample carries the *other* recipe's status columns; this one
        // declares none, so they would be refused as unknown before the blank
        // check is reached.
        answers.statuses.clear();
        let err = Answers::from_toml_str("x", &answers.to_toml(), RECIPES).unwrap_err();
        assert!(
            matches!(err, AnswersError::MissingBlank { .. }),
            "unfilled blanks were accepted: {err}"
        );
        // The message has to name the key so it can be fixed without reading
        // the recipe table.
        assert!(err.to_string().contains("slack_user_id"), "{err}");

        // Filling every blank the recipe declares makes it acceptable.
        answers.slack_user_id = Some("U123456".to_string());
        answers.llm = Some(LlmAnswer {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            model: "anthropic/claude-haiku-4-5".to_string(),
        });
        Answers::from_toml_str("x", &answers.to_toml(), RECIPES).unwrap();
    }

    #[test]
    fn reference_and_register_command_agree_on_the_account() {
        // The printed command has to register exactly what config.toml points
        // at — a mismatch here is a setup that looks complete and then fails
        // at run time with "secret not found".
        let account = "slack-user";
        assert_eq!(
            SecretBackend::Keychain.reference(account),
            "keychain:totsuka/slack-user"
        );
        let command = SecretBackend::Keychain.register_command(account).unwrap();
        assert!(command.contains("-s totsuka"), "{command}");
        assert!(command.contains("-a slack-user"), "{command}");

        assert_eq!(
            SecretBackend::OnePassword.reference(account),
            "op://Dev/totsuka/slack-user"
        );
        // Env has no register step; it must not pretend otherwise.
        assert_eq!(
            SecretBackend::Env.reference("hook-token"),
            "${TOTSUKA_HOOK_TOKEN}"
        );
        assert!(SecretBackend::Env.register_command(account).is_none());
    }

    #[test]
    fn no_secret_values_can_be_stored() {
        // `deny_unknown_fields` is what enforces this: an answers file that
        // tried to carry a token would fail to parse rather than silently
        // persisting it.
        let text = "version = 2\nrecipe = \"minimal-github-herdr\"\n\
                    secret_backend = \"keychain\"\nslack_token = \"xoxp-secret\"\n\
                    [[repositories]]\nname = \"r\"\npath = \"/r\"\n";
        let err = Answers::from_toml_str("x", text, RECIPES).unwrap_err();
        assert!(matches!(err, AnswersError::Parse { .. }), "{err}");
    }
}
