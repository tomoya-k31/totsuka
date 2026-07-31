//! `totsuka setup` — the interactive path from a fresh install to a config
//! that loads (#348).
//!
//! # Why this exists next to `init`
//!
//! `init` writes a skeleton in which **every line is a comment**, so the file
//! it produces does nothing until it is hand-edited against the reference docs.
//! That is the right behaviour for CI and for bootstrapping, and it stays
//! unchanged. `setup` is the human path: it asks, then writes values.
//!
//! | | `init` | `setup` |
//! |---|---|---|
//! | interactive | never | yes (hidden `--answers` for tests) |
//! | writes | dirs + commented skeleton | dirs + real values |
//! | existing files | skipped | skipped |
//! | secrets | untouched | **untouched** — references only |
//!
//! # Two phases
//!
//! The interview is pure: it builds [`Answers`] in memory and touches nothing.
//! Only after the plan is printed and confirmed does anything get written. So
//! Ctrl-C during the questions leaves no trace, and a failure during apply
//! reports how far it got — every step is idempotent, so re-running converges
//! rather than double-applying.
//!
//! # Secrets
//!
//! `setup` never handles a secret value. It picks a backend, writes the
//! *references* into the config, and prints the commands to register them. The
//! orchestrator's own contract is that it only ever reads secrets (F-65), and
//! a wizard that collected tokens would be the one place that broke it.

mod answers;
mod interview;
mod recipes;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use orchestrator_core::config::{
    RepositoryDraft, WorkflowDraft, set_llm, set_plugin_enabled, upsert_repository, upsert_workflow,
};

use crate::common::{CliError, Cx, EXIT_USAGE, ExitWith};
use crate::init_cmd;

pub use answers::{Answers, LlmAnswer, RepositoryAnswer, SecretBackend};
use interview::Prompt;
use recipes::{Blank, RECIPES, Recipe};

/// Options parsed from the command line.
#[derive(Debug, Default)]
pub struct SetupArgs {
    /// Read answers from a file instead of asking. Hidden; see [`answers`].
    pub answers: Option<PathBuf>,
    /// Write the collected answers here.
    pub save_answers: Option<PathBuf>,
    /// Print the plan and stop.
    pub dry_run: bool,
    /// Skip the final confirmation.
    pub yes: bool,
}

/// Run the wizard.
pub fn run(cx: &Cx, args: &SetupArgs) -> Result<(), CliError> {
    let mut stdout = std::io::stdout();
    let answers = match &args.answers {
        Some(path) => load_answers(path)?,
        None => {
            // Never fall back to defaults when there is nobody to ask: a
            // silently-guessed config is worse than no config.
            if !std::io::stdin().is_terminal() {
                return Err(ExitWith::new(
                    EXIT_USAGE,
                    "`totsuka setup` needs a terminal → run it interactively, or run \
                     `totsuka init` and edit config.toml by hand",
                )
                .into());
            }
            let stdin = std::io::stdin();
            let mut locked = stdin.lock();
            let mut prompt = Prompt::new(&mut locked, &mut stdout);
            interview(&mut prompt)?
        }
    };

    if let Some(path) = &args.save_answers {
        std::fs::write(path, answers.to_toml())?;
        println!("Saved answers to {}", path.display());
    }

    let plan = Plan::new(cx, &answers);
    print!("{}", plan.render());
    if args.dry_run {
        println!("\n--dry-run: nothing was written.");
        return Ok(());
    }
    if !args.yes {
        let stdin = std::io::stdin();
        let mut locked = stdin.lock();
        let mut prompt = Prompt::new(&mut locked, &mut stdout);
        if !prompt.confirm("\nApply this?", true)? {
            println!("Aborted; nothing was written.");
            return Ok(());
        }
    }

    apply(cx, &answers, &plan)
}

/// Read and validate an answers file.
fn load_answers(path: &Path) -> Result<Answers, CliError> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| {
        CliError::from(
            answers::AnswersError::Read {
                path: display.clone(),
                source,
            }
            .to_string(),
        )
    })?;
    Answers::from_toml_str(&display, &text, RECIPES.len())
        .map_err(|e| CliError::from(e.to_string()))
}

/// The pure phase: ask everything, write nothing.
fn interview(prompt: &mut Prompt) -> Result<Answers, CliError> {
    prompt.say("totsuka setup — this asks a few questions, shows what it will do,")?;
    prompt.say("and writes nothing until you confirm. Secrets are never entered here.")?;
    prompt.say("")?;

    let choices: Vec<(&str, &str)> = RECIPES.iter().map(|r| (r.label, r.blurb)).collect();
    let recipe_index = prompt.choose("Which setup do you want to start from?", &choices, 0)?;
    let recipe = &RECIPES[recipe_index];
    prompt.say("")?;

    let mut repositories = Vec::new();
    loop {
        let path = prompt.ask("Repository path", None)?;
        let default_name = default_repo_name(&path);
        let name = prompt.ask("  Name for it", Some(&default_name))?;
        let summary = prompt.ask(
            "  One-line summary (optional, aids repo selection)",
            Some(""),
        )?;
        repositories.push(RepositoryAnswer {
            name,
            path,
            summary: (!summary.is_empty()).then_some(summary),
        });
        if !prompt.confirm("Add another repository?", false)? {
            break;
        }
    }
    prompt.say("")?;

    let backend_index = prompt.choose(
        "Where do your secrets live? (setup only writes references, never values)",
        &[
            ("macOS Keychain", "keychain:totsuka/<name>"),
            ("1Password", "op://Dev/totsuka/<name>"),
            ("Environment variables", "${TOTSUKA_<NAME>}"),
        ],
        0,
    )?;
    let secret_backend = match backend_index {
        0 => SecretBackend::Keychain,
        1 => SecretBackend::OnePassword,
        _ => SecretBackend::Env,
    };

    let mut llm = None;
    let mut slack_user_id = None;
    for blank in recipe.blanks {
        prompt.say("")?;
        match blank {
            Blank::Llm => {
                prompt
                    .say("This setup needs an LLM to pick which repository a task belongs to.")?;
                llm = Some(LlmAnswer {
                    base_url: prompt.ask("  API base URL", Some("https://openrouter.ai/api/v1"))?,
                    model: prompt.ask("  Model", Some("anthropic/claude-haiku-4-5"))?,
                });
            }
            Blank::SlackUserId => {
                prompt.say("Your Slack member ID (profile → ⋮ → Copy member ID).")?;
                slack_user_id = Some(prompt.ask("  Member ID", None)?);
            }
        }
    }

    Ok(Answers {
        version: answers::ANSWERS_VERSION,
        recipe: recipe_index,
        repositories,
        secret_backend,
        llm,
        slack_user_id,
    })
}

/// A repository name guessed from its path, so the question has a default.
fn default_repo_name(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("repo")
        .to_string()
}

/// What apply will do, decided before anything is written so it can be shown
/// and so `--dry-run` and the real run cannot disagree.
struct Plan<'a> {
    recipe: &'a Recipe,
    answers: &'a Answers,
    /// `config.toml` is written only when absent or still a bare skeleton.
    write_config: bool,
    config_path: PathBuf,
}

impl<'a> Plan<'a> {
    fn new(cx: &Cx, answers: &'a Answers) -> Plan<'a> {
        Plan {
            recipe: &RECIPES[answers.recipe],
            answers,
            write_config: is_unconfigured(&cx.config_path),
            config_path: cx.config_path.clone(),
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("\nPlan\n────\n");
        out.push_str(&format!("Recipe:   {}\n", self.recipe.label));
        for repo in &self.answers.repositories {
            out.push_str(&format!("Repo:     {} → {}\n", repo.name, repo.path));
        }
        for plugin in self.recipe.plugins {
            out.push_str(&format!("Plugin:   {} ({})\n", plugin.name, plugin.kind));
        }
        for workflow in self.recipe.workflows {
            out.push_str(&format!(
                "Workflow: {} — {} → {} ({})\n",
                workflow.name,
                workflow.source,
                workflow.agent,
                workflow.mode.as_str()
            ));
        }
        if self.write_config {
            out.push_str(&format!("Write:    {}\n", self.config_path.display()));
        } else {
            out.push_str(&format!(
                "Skip:     {} already exists (left untouched)\n",
                self.config_path.display()
            ));
        }
        out
    }
}

/// Whether `config.toml` is absent or still the untouched skeleton.
///
/// `init` writes a file in which every line is a comment. Treating that as
/// "already configured" would make `setup` a no-op for everyone who ran `init`
/// first — which the docs told them to do. Anything with real content is left
/// alone.
fn is_unconfigured(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Err(_) => true,
        Ok(text) => text
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')),
    }
}

/// The effecting phase. Every step is idempotent so a failure part-way can be
/// recovered by running the command again.
fn apply(cx: &Cx, answers: &Answers, plan: &Plan) -> Result<(), CliError> {
    init_cmd::ensure_dirs(cx)?;

    if plan.write_config {
        let current = std::fs::read_to_string(&cx.config_path).unwrap_or_default();
        let updated = build_config(&current, answers, plan.recipe)?;
        write_atomically(&cx.config_path, &updated)?;
        println!("wrote: {}", cx.config_path.display());
    } else {
        println!(
            "skipped: {} already exists (left untouched)",
            cx.config_path.display()
        );
    }

    print_secret_checklist(answers, plan.recipe);
    println!();
    println!("next: install the plugins, then run `totsuka doctor`");
    Ok(())
}

/// Apply every config edit for `answers`, returning the new text.
///
/// Separated from the writing so it can be tested directly — the property that
/// matters is that the result loads and validates, not that a file appeared.
pub(crate) fn build_config(
    current: &str,
    answers: &Answers,
    recipe: &Recipe,
) -> Result<String, CliError> {
    let mut text = current.to_string();

    for repo in &answers.repositories {
        text = upsert_repository(
            &text,
            &RepositoryDraft {
                name: &repo.name,
                path: &repo.path,
                summary: repo.summary.as_deref(),
            },
        )?;
    }
    for plugin in recipe.plugins {
        text = set_plugin_enabled(&text, plugin.name, true, Some(plugin.kind))?;
    }
    for workflow in recipe.workflows {
        text = upsert_workflow(
            &text,
            &WorkflowDraft {
                name: workflow.name,
                source: workflow.source,
                trigger: workflow.trigger,
                mode: workflow.mode,
                agent: workflow.agent,
                output: workflow.output,
                verification: workflow.verification,
                on_success: workflow.on_success,
                on_failure: None,
            },
        )?;
    }
    if let Some(llm) = &answers.llm {
        let reference = answers.secret_backend.reference("llm-api-key");
        text = set_llm(&text, &llm.base_url, &llm.model, Some(&reference))?;
    }
    Ok(text)
}

/// Write via a temporary file and rename, so an interrupted write cannot leave
/// a half-config in place (the same staging discipline `commit_install` uses).
fn write_atomically(path: &Path, contents: &str) -> Result<(), CliError> {
    let staged = path.with_extension("toml.new");
    std::fs::write(&staged, contents)?;
    if let Err(e) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(e.into());
    }
    Ok(())
}

/// Print the secrets the chosen recipe needs and how to register them.
fn print_secret_checklist(answers: &Answers, recipe: &Recipe) {
    let accounts = required_secrets(answers, recipe);
    if accounts.is_empty() {
        return;
    }
    println!();
    println!("Secrets to register (setup never handles the values):");
    for account in accounts {
        let reference = answers.secret_backend.reference(&account);
        println!("  {reference}");
        if let Some(command) = answers.secret_backend.register_command(&account) {
            println!("    {command}");
        }
    }
    println!("  `totsuka doctor` verifies these once they exist.");
}

/// Which secret accounts the chosen recipe needs, in a stable order.
pub(crate) fn required_secrets(answers: &Answers, recipe: &Recipe) -> Vec<String> {
    let mut accounts: Vec<String> = Vec::new();
    for plugin in recipe.plugins {
        match plugin.name {
            "slack" => accounts.extend(
                ["slack-user", "slack-app", "slack-bot"]
                    .iter()
                    .map(|s| s.to_string()),
            ),
            "github" => accounts.push("github-token".to_string()),
            "notion" => accounts.push("notion-token".to_string()),
            _ => {}
        }
    }
    if answers.llm.is_some() {
        accounts.push("llm-api-key".to_string());
    }
    accounts
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::config::RootConfig;

    fn answers_for(recipe: usize) -> Answers {
        Answers {
            version: answers::ANSWERS_VERSION,
            recipe,
            repositories: vec![RepositoryAnswer {
                name: "totsuka".to_string(),
                path: "~/Workspace/totsuka".to_string(),
                summary: Some("the orchestrator".to_string()),
            }],
            secret_backend: SecretBackend::Keychain,
            llm: RECIPES[recipe]
                .blanks
                .contains(&Blank::Llm)
                .then(|| LlmAnswer {
                    base_url: "https://openrouter.ai/api/v1".to_string(),
                    model: "anthropic/claude-haiku-4-5".to_string(),
                }),
            slack_user_id: RECIPES[recipe]
                .blanks
                .contains(&Blank::SlackUserId)
                .then(|| "U123456".to_string()),
        }
    }

    #[test]
    fn every_recipe_produces_a_config_that_loads_and_validates() {
        // The wizard's whole job. A recipe that writes an unloadable config
        // would fail at `run`, long after the questions that caused it.
        for (index, recipe) in RECIPES.iter().enumerate() {
            let answers = answers_for(index);
            let text = build_config("", &answers, recipe)
                .unwrap_or_else(|e| panic!("{}: {e}", recipe.label));

            let cfg = RootConfig::from_toml_str(&text)
                .unwrap_or_else(|e| panic!("{}: does not load: {e}\n---\n{text}", recipe.label));

            let no_env = |_: &str| None;
            let unexpected: Vec<String> = orchestrator_core::config::validate_static(&cfg, &no_env)
                .into_iter()
                .map(|e| e.to_string())
                // The repository path does not exist in a test environment.
                .filter(|e| !e.contains("path"))
                .collect();
            assert!(
                unexpected.is_empty(),
                "{}: {unexpected:?}\n---\n{text}",
                recipe.label
            );

            assert_eq!(cfg.workflows.len(), recipe.workflows.len());
            for plugin in recipe.plugins {
                assert!(
                    cfg.plugins.get(plugin.name).is_some_and(|p| p.enabled),
                    "{}: {} not enabled",
                    recipe.label,
                    plugin.name
                );
            }
        }
    }

    #[test]
    fn applying_twice_is_a_no_op() {
        // `setup` is expected to be re-run; a second pass must converge, not
        // append duplicate `[[workflows]]` (which `validate` rejects outright).
        for (index, recipe) in RECIPES.iter().enumerate() {
            let answers = answers_for(index);
            let once = build_config("", &answers, recipe).unwrap();
            let twice = build_config(&once, &answers, recipe).unwrap();
            assert_eq!(once, twice, "{}", recipe.label);
        }
    }

    #[test]
    fn the_commented_skeleton_counts_as_unconfigured() {
        let dir = std::env::temp_dir().join(format!("totsuka-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        // Absent.
        assert!(is_unconfigured(&path));

        // What `init` writes: comments and blank lines only.
        std::fs::write(&path, "# totsuka configuration\n\n# max_concurrency = 4\n").unwrap();
        assert!(
            is_unconfigured(&path),
            "an untouched skeleton must not block setup"
        );

        // One real key is enough to mean "hands off".
        std::fs::write(&path, "# comment\nmax_concurrency = 4\n").unwrap();
        assert!(!is_unconfigured(&path));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn secret_checklist_matches_what_the_config_references() {
        // A reference in config.toml that the checklist forgets to mention is
        // a setup that looks finished and then fails with "secret not found".
        let slack = RECIPES
            .iter()
            .position(|r| r.plugins.iter().any(|p| p.name == "slack"))
            .unwrap();
        let answers = answers_for(slack);
        let text = build_config("", &answers, &RECIPES[slack]).unwrap();
        let accounts = required_secrets(&answers, &RECIPES[slack]);

        assert!(accounts.iter().any(|a| a == "slack-user"), "{accounts:?}");
        assert!(accounts.iter().any(|a| a == "llm-api-key"), "{accounts:?}");

        // Every reference the config carries is on the checklist.
        let llm_ref = answers.secret_backend.reference("llm-api-key");
        assert!(text.contains(&llm_ref), "{text}");
        assert!(accounts.iter().any(|a| llm_ref.ends_with(a)));
    }

    #[test]
    fn a_repository_name_is_guessed_from_the_path() {
        assert_eq!(default_repo_name("~/Workspace/totsuka"), "totsuka");
        assert_eq!(default_repo_name("/a/b/dotfiles/"), "dotfiles");
        assert_eq!(default_repo_name(""), "repo");
    }
}
