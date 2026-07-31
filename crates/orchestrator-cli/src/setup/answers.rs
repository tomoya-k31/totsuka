//! What the interview collected, and the non-interactive form of it (#348).
//!
//! The wizard is interactive by design, but the collected answers are a plain
//! serialisable value with a hidden `--answers <file>` entry point. That is not
//! a product feature — it is the only way to test the wizard end-to-end: a CLI
//! E2E spawns `totsuka` as a child process, which has no terminal, so without
//! this the strongest assertion available would be "it refuses to run", and
//! "the config it writes actually loads" would go unchecked.
//!
//! **No secret values live here.** `setup` writes secret *references*
//! (`keychain:totsuka/slack-user`) and prints the commands to register them;
//! the values never pass through this process. So an answers file is safe to
//! keep in a dotfiles repository.

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
    /// The value `plugins/github.toml` uses.
    pub fn as_str(self) -> &'static str {
        match self {
            GitHubOwnerType::User => "user",
            GitHubOwnerType::Organization => "organization",
        }
    }
}

/// What `plugins/github.toml` needs beyond the token.
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
    pub version: u32,
    /// Index into [`super::recipes::RECIPES`].
    pub recipe: usize,
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
}

/// The version this build writes and accepts.
pub const ANSWERS_VERSION: u32 = 1;

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
    /// The recipe index does not exist.
    #[error("{path} selects recipe {found}, but only {count} are available")]
    UnknownRecipe {
        /// Path that was tried.
        path: String,
        /// Index in the file.
        found: usize,
        /// How many recipes exist.
        count: usize,
    },
    /// No repository was given.
    #[error("{path} lists no repositories → at least one is required")]
    NoRepositories {
        /// Path that was tried.
        path: String,
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
        let answers: Answers = toml::from_str(text).map_err(|source| AnswersError::Parse {
            path: path.to_string(),
            source,
        })?;
        if answers.version != ANSWERS_VERSION {
            return Err(AnswersError::Version {
                path: path.to_string(),
                found: answers.version,
                expected: ANSWERS_VERSION,
            });
        }
        let Some(recipe) = recipes.get(answers.recipe) else {
            return Err(AnswersError::UnknownRecipe {
                path: path.to_string(),
                found: answers.recipe,
                count: recipes.len(),
            });
        };
        if answers.repositories.is_empty() {
            return Err(AnswersError::NoRepositories {
                path: path.to_string(),
            });
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

    fn sample() -> Answers {
        Answers {
            version: ANSWERS_VERSION,
            recipe: 0,
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
        let text = "version = 1\nrecipe = 0\nsecret_backend = \"keychain\"\n\
                    repositorys = []\n";
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
    fn an_out_of_range_recipe_is_refused() {
        let mut answers = sample();
        answers.recipe = 42;
        let err = Answers::from_toml_str("x", &answers.to_toml(), RECIPES).unwrap_err();
        assert!(
            matches!(err, AnswersError::UnknownRecipe { found: 42, .. }),
            "{err}"
        );
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
            .position(|r| r.blanks.contains(&Blank::SlackUserId))
            .expect("a recipe with blanks");

        let mut answers = sample();
        answers.recipe = slack;
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
        let text = "version = 1\nrecipe = 0\nsecret_backend = \"keychain\"\n\
                    slack_token = \"xoxp-secret\"\n\
                    [[repositories]]\nname = \"r\"\npath = \"/r\"\n";
        let err = Answers::from_toml_str("x", text, RECIPES).unwrap_err();
        assert!(matches!(err, AnswersError::Parse { .. }), "{err}");
    }
}
