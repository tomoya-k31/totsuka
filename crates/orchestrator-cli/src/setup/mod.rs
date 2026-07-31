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
//! Nothing is *configured* until the plan is printed and confirmed. So Ctrl-C
//! during the questions leaves no trace, and a failure during apply reports how
//! far it got — every step is idempotent, so re-running converges rather than
//! double-applying.
//!
//! The one file written outside apply is `--save-answers`, and deliberately so:
//! "let me see the plan and keep my answers, without applying them" is the
//! point of pairing it with `--dry-run`. It is called out here, and in what
//! `--dry-run` prints, because a blanket "nothing was written" would be a lie
//! the moment both flags are used together.
//!
//! # Secrets
//!
//! `setup` never handles a secret value. It picks a backend, writes the
//! *references* into the config, and prints the commands to register them. The
//! orchestrator's own contract is that it only ever reads secrets (F-65), and
//! a wizard that collected tokens would be the one place that broke it.

mod answers;
mod interview;
mod plugin_config;
mod recipes;

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use orchestrator_core::config::{
    RepositoryDraft, WorkflowDraft, set_llm, set_plugin_enabled, upsert_repository, upsert_workflow,
};

use crate::common::{CliError, Cx, EXIT_USAGE, ExitWith};
use crate::{bundled, doctor_cmd, from_source, init_cmd, plugin_cmd};

pub use answers::{
    Answers, GitHubAnswer, GitHubOwnerType, LlmAnswer, RepositoryAnswer, SecretBackend,
};
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
    /// Pin where bundled plugins are looked up, instead of detecting.
    ///
    /// Hidden, and the same affordance `plugin install --bundled-dir` provides,
    /// for the same reason: an E2E runs `totsuka` as a child process whose
    /// working directory is inside this checkout, so without a pin the wizard
    /// would detect a checkout and shell out to `cargo build` — which tests are
    /// not allowed to do (ADR-0018). An env var is not an option either
    /// (ADR-0009).
    pub bundled_dir: Option<PathBuf>,
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

    // Before the plan, so the answers survive even if the plan is rejected —
    // re-answering a dozen questions to recover them would be the worse
    // failure. `--dry-run`'s summary names this file so the claim it makes
    // stays true.
    if let Some(path) = &args.save_answers {
        std::fs::write(path, answers.to_toml())?;
        println!("Saved answers to {}", path.display());
    }

    let plan = Plan::new(cx, &answers, args)?;
    print!("{}", plan.render());
    if args.dry_run {
        match &args.save_answers {
            Some(path) => println!(
                "\n--dry-run: nothing was configured (only {} was written).",
                path.display()
            ),
            None => println!("\n--dry-run: nothing was written."),
        }
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
    Answers::from_toml_str(&display, &text, RECIPES).map_err(|e| CliError::from(e.to_string()))
}

/// The pure phase: ask everything, write nothing.
fn interview(prompt: &mut Prompt) -> Result<Answers, CliError> {
    prompt.say("totsuka setup — this asks a few questions, shows what it will do,")?;
    prompt.say("and changes nothing until you confirm. Secrets are never entered here.")?;
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
    let mut github = None;
    for blank in recipe.blanks {
        prompt.say("")?;
        match blank {
            Blank::GitHub => {
                prompt.say("Which GitHub Project board should tasks come from?")?;
                let owner = prompt.ask("  Owner (user or org that owns the board)", None)?;
                let owner_type = match prompt.choose(
                    "  Is that a user or an organization?",
                    &[
                        ("User", "a personal account"),
                        ("Organization", "a GitHub organization"),
                    ],
                    0,
                )? {
                    0 => GitHubOwnerType::User,
                    _ => GitHubOwnerType::Organization,
                };
                // Free text rather than a number prompt: re-asking is the
                // interview's job, and `ask` already loops until non-empty.
                let project_number = loop {
                    let typed = prompt.ask("  Project number (from the board's URL)", None)?;
                    match typed.trim().parse::<i64>() {
                        Ok(n) => break n,
                        Err(_) => prompt.say("  → that is not a number; try again")?,
                    }
                };
                let github_login = prompt.ask(
                    "  Your GitHub login (whose cards get picked up)",
                    Some(&owner),
                )?;
                github = Some(GitHubAnswer {
                    owner,
                    owner_type,
                    project_number,
                    github_login,
                });
            }
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
        github,
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

/// Where the recipe's plugins will come from.
///
/// Decided once, up front, so the plan shown is the plan run. The order is
/// "what this machine already has": a release tarball carries its plugins next
/// to the binary, a developer has a checkout to build from, and a bare
/// `cargo install` has neither — which is not an error, just a setup that
/// cannot finish the last step by itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PluginSource {
    /// A bundled tree next to the binary (#345).
    Bundled(PathBuf),
    /// A totsuka checkout to `cargo build` in (#346).
    Checkout(PathBuf),
    /// Neither; the user is told what to run.
    Unavailable,
}

impl PluginSource {
    fn detect(explicit: Option<&Path>) -> PluginSource {
        // An explicit root is taken as given — never falling through to a
        // checkout build, which is the whole point of pinning it.
        if let Some(root) = bundled::locate(explicit) {
            return PluginSource::Bundled(root);
        }
        if explicit.is_none()
            && let Ok(cwd) = std::env::current_dir()
            && let Some(root) = from_source::find_checkout_root(&cwd, &from_source::is_checkout)
        {
            return PluginSource::Checkout(root);
        }
        PluginSource::Unavailable
    }
}

/// What apply will do, decided before anything is written so it can be shown
/// and so `--dry-run` and the real run cannot disagree.
struct Plan<'a> {
    recipe: &'a Recipe,
    answers: &'a Answers,
    /// `config.toml` is written only when absent or still a bare skeleton.
    write_config: bool,
    config_path: PathBuf,
    /// `plugins/<name>.toml` files to write, minus the ones already there.
    plugin_configs: Vec<PathBuf>,
    plugin_source: PluginSource,
}

impl<'a> Plan<'a> {
    fn new(cx: &Cx, answers: &'a Answers, args: &SetupArgs) -> Result<Plan<'a>, CliError> {
        let recipe = &RECIPES[answers.recipe];
        let plugin_dir = cx.plugin_config_dir();
        let mut plugin_configs = Vec::new();
        for draft in plugin_config::drafts_for(answers, recipe) {
            let path = plugin_dir.join(format!("{}.toml", draft.name));
            if is_absent(&path)? {
                plugin_configs.push(path);
            }
        }
        Ok(Plan {
            recipe,
            answers,
            write_config: is_unconfigured(&cx.config_path)?,
            config_path: cx.config_path.clone(),
            plugin_configs,
            plugin_source: PluginSource::detect(args.bundled_dir.as_deref()),
        })
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
        for path in &self.plugin_configs {
            out.push_str(&format!("Write:    {}\n", path.display()));
        }
        out.push_str(&match &self.plugin_source {
            PluginSource::Bundled(root) => {
                format!("Install:  from the plugins bundled at {}\n", root.display())
            }
            PluginSource::Checkout(root) => {
                format!("Install:  by building from {}\n", root.display())
            }
            PluginSource::Unavailable => {
                "Install:  nothing to install from → the commands will be printed\n".to_string()
            }
        });
        out.push_str("Then:     totsuka doctor\n");
        out
    }
}

/// Whether `config.toml` is absent or still the untouched skeleton.
///
/// `init` writes a file in which every line is a comment. Treating that as
/// "already configured" would make `setup` a no-op for everyone who ran `init`
/// first — which the docs told them to do. Anything with real content is left
/// alone.
///
/// Only *absent* counts as unconfigured. Any other read failure — a permission
/// error, a directory in the way — is reported rather than assumed empty: the
/// answer decides whether `setup` overwrites the file, and the rename in
/// [`write_atomically`] only needs the *directory* to be writable, so a config
/// that cannot be read can still be clobbered.
fn is_unconfigured(path: &Path) -> Result<bool, CliError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(CliError::from(format!(
            "cannot read {} ({e}) → refusing to overwrite a config it cannot inspect",
            path.display()
        ))),
    }
}

/// Whether `path` is absent, refusing to guess when the answer is unclear.
///
/// The counterpart of [`is_unconfigured`] for files that have no skeleton form,
/// and it exists for the same reason: `Path::exists` folds *every* failure —
/// permission denied, a symlink loop, a directory in the way — into `false`,
/// which here reads as "safe to create". It is not. [`write_atomically`]'s
/// `rename` only needs the **directory** to be writable, so a `plugins/*.toml`
/// that cannot be examined can still be replaced, and these files hold the
/// user's secret references.
fn is_absent(path: &Path) -> Result<bool, CliError> {
    path.try_exists().map(|exists| !exists).map_err(|e| {
        CliError::from(format!(
            "cannot examine {} ({e}) → refusing to overwrite a file it cannot inspect",
            path.display()
        ))
    })
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

    write_plugin_configs(cx, answers, plan.recipe)?;
    install_plugins(cx, plan)?;
    print_secret_checklist(answers, plan.recipe);

    // `doctor` last, and in-process. It is the only step that can tell the user
    // whether the setup they just ran actually works, and its exit code 3 is
    // the contract scripts read — swallowing it here would make `setup` report
    // success over a config `doctor` has already found problems with.
    println!();
    println!("Running `totsuka doctor` …");
    println!();
    doctor_cmd::run(cx, false, false)
}

/// Write each `plugins/<name>.toml` the recipe needs, skipping ones that exist.
///
/// Same rule as `config.toml`, minus the skeleton exception: nothing generates
/// a commented placeholder for these, so a file that exists was written by a
/// human or by an earlier `setup`, and either way it is theirs. "Exists" is
/// decided by [`is_absent`], which errors rather than guessing — the same
/// hazard [`is_unconfigured`] guards against.
fn write_plugin_configs(cx: &Cx, answers: &Answers, recipe: &Recipe) -> Result<(), CliError> {
    let dir = cx.plugin_config_dir();
    for draft in plugin_config::drafts_for(answers, recipe) {
        let path = dir.join(format!("{}.toml", draft.name));
        if !is_absent(&path)? {
            println!(
                "skipped: {} already exists (left untouched)",
                path.display()
            );
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        write_atomically(&path, &draft.body)?;
        println!("wrote: {}", path.display());
    }
    Ok(())
}

/// Install and enable the recipe's plugins from whichever source this build has.
fn install_plugins(cx: &Cx, plan: &Plan) -> Result<(), CliError> {
    let names: Vec<&str> = plan.recipe.plugins.iter().map(|p| p.name).collect();
    match &plan.plugin_source {
        PluginSource::Bundled(root) => {
            println!();
            println!("Installing plugins from {}", root.display());
        }
        PluginSource::Checkout(root) => {
            println!();
            println!("Building plugins from {}", root.display());
        }
        PluginSource::Unavailable => {
            println!();
            println!("No plugins to install from: this `totsuka` ships none and you are not in");
            println!("a checkout. Install them, then re-run `totsuka setup`:");
            for name in &names {
                println!("  totsuka plugin install <dir-with-{name}> --enable");
            }
            return Ok(());
        }
    }

    for name in names {
        // One at a time rather than `--all`: a recipe asks for a specific set,
        // and installing whatever else happens to be bundled would enable
        // plugins nobody chose.
        plugin_cmd::run(
            cx,
            plugin_cmd::PluginCommand::Install {
                source: Some(name.to_string()),
                bundled: matches!(plan.plugin_source, PluginSource::Bundled(_)),
                from_source: matches!(plan.plugin_source, PluginSource::Checkout(_)),
                repo: match &plan.plugin_source {
                    PluginSource::Checkout(root) => Some(root.clone()),
                    _ => None,
                },
                bundled_dir: match &plan.plugin_source {
                    PluginSource::Bundled(root) => Some(root.clone()),
                    _ => None,
                },
                all: false,
                // The plan was already confirmed; a second prompt per plugin
                // would ask the same question up to three more times.
                enable: true,
                yes: true,
                profile: plugin_cmd::BuildProfile::Release,
                print_plan: false,
            },
        )?;
    }
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
        println!("  {reference}  — {}", purpose_of(&account));
        if let Some(command) = answers.secret_backend.register_command(&account) {
            println!("    {command}");
        }
    }
    println!("  Every one of these is required: the config references it, so a");
    println!("  missing one stops the plugin from starting. `totsuka doctor`");
    println!("  verifies them once they exist.");
}

/// What each account is for, so the checklist is not four opaque names.
///
/// `slack-bot` in particular looks skippable — the plugin treats `bot_token` as
/// opt-in — but the recipe writes it, so leaving it unregistered breaks the
/// whole source rather than just the nudge. Saying what it buys is what makes
/// that a choice instead of a surprise.
fn purpose_of(account: &str) -> &'static str {
    match account {
        "slack-user" => "posts the reply under your own name",
        "slack-app" => "opens the Socket Mode connection",
        "slack-bot" => "sends the notification nudge (self-replies raise none)",
        "github-token" => "reads the Project board and writes results back",
        "notion-token" => "reads the database and writes results back",
        "llm-api-key" => "picks which repository a task belongs to",
        _ => "used by the config setup just wrote",
    }
}

/// Which secret accounts the chosen recipe needs, in a stable order.
///
/// "Needs" means the generated config **references** it, which is a stronger
/// condition than the plugin's own required-field list: `slack`'s `bot_token`
/// is optional to the plugin, but the reply-as-yourself recipe writes it
/// because a self-reply produces no Slack notification at all without the nudge
/// (ADR-0021). Once written, an unregistered reference fails the plugin's
/// launch — so it belongs on this list, not in a footnote.
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
            github: RECIPES[recipe]
                .blanks
                .contains(&Blank::GitHub)
                .then(|| GitHubAnswer {
                    owner: "tomoya-k31".to_string(),
                    owner_type: GitHubOwnerType::User,
                    project_number: 1,
                    github_login: "tomoya-k31".to_string(),
                }),
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
        assert!(is_unconfigured(&path).unwrap());

        // What `init` writes: comments and blank lines only.
        std::fs::write(&path, "# totsuka configuration\n\n# max_concurrency = 4\n").unwrap();
        assert!(
            is_unconfigured(&path).unwrap(),
            "an untouched skeleton must not block setup"
        );

        // One real key is enough to mean "hands off".
        std::fs::write(&path, "# comment\nmax_concurrency = 4\n").unwrap();
        assert!(!is_unconfigured(&path).unwrap());

        // Unreadable is not the same as absent. Reporting it beats assuming
        // "empty" and overwriting — the rename only needs a writable
        // directory, so an unreadable config is still clobberable.
        let blocked = dir.join("blocked");
        std::fs::create_dir(&blocked).unwrap();
        let err = is_unconfigured(&blocked).unwrap_err().to_string();
        assert!(err.contains("blocked"), "{err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unexaminable_plugin_config_is_reported_not_assumed_absent() {
        // The same hazard `is_unconfigured` guards for `config.toml`, and it
        // was reintroduced here once by reaching for `Path::exists` — which
        // folds every failure into `false`, i.e. "safe to create". These files
        // hold the user's secret references, and `write_atomically`'s rename
        // only needs the *directory* to be writable.
        let dir = std::env::temp_dir().join(format!("totsuka-absent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        assert!(is_absent(&dir.join("nope.toml")).unwrap());
        std::fs::write(dir.join("there.toml"), "x = 1\n").unwrap();
        assert!(!is_absent(&dir.join("there.toml")).unwrap());

        // A path whose *parent* is a file, not a directory: `try_exists` reports
        // the error instead of claiming the file is not there.
        let err = is_absent(&dir.join("there.toml/child.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("child.toml"), "{err}");
        assert!(err.contains("refusing to overwrite"), "{err}");

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
