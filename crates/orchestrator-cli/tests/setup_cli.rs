//! End-to-end test for `totsuka setup` (#348), driving the real CLI binary.
//!
//! A child process has no terminal, so the interactive path cannot be exercised
//! here — that is what the unit tests in `setup::interview` are for. What this
//! file covers is everything around it: the TTY gate, `--dry-run`, the
//! skip-existing rule, and the assertion that matters most — **the config the
//! wizard writes is one `totsuka config validate` accepts**.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A scratch XDG environment for one test.
struct Env {
    root: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("totsuka-setup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config/totsuka")).unwrap();
        Self { root }
    }

    fn config_toml(&self) -> PathBuf {
        self.root.join("config/totsuka/config.toml")
    }

    /// A real directory to register as a repository — `config validate`
    /// rejects a path that does not exist.
    fn repo(&self) -> PathBuf {
        let dir = self.root.join("repo");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn answers(&self, body: &str) -> PathBuf {
        let path = self.root.join("answers.toml");
        fs::write(&path, body).unwrap();
        path
    }

    /// A fake bundled plugins tree: `<root>/bundled/<name>/{plugin.toml,<name>}`.
    ///
    /// Every `--yes` run pins this. Without it the wizard would notice that the
    /// test's working directory is inside a totsuka checkout and shell out to
    /// `cargo build`, which tests must not do (ADR-0018) — and which would also
    /// make every one of these tests take minutes.
    fn bundled(&self, names: &[&str]) -> PathBuf {
        let root = self.root.join("bundled");
        for name in names {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            // The manifests mirror the real ones closely enough for
            // `config validate`, which cross-checks a workflow's `output`
            // against the source plugin's declared capabilities — a task
            // source without `outputs = ["source"]` makes every recipe here
            // invalid, so the fixture cannot skip it.
            let (kind, capabilities) = match *name {
                "herdr" => ("agent_ide", "plan_mode = true\npane_control = true\n"),
                "macos" => ("notifier", ""),
                _ => (
                    "task_source",
                    "task_submit = true\noutputs = [\"source\"]\n",
                ),
            };
            fs::write(
                dir.join("plugin.toml"),
                format!(
                    "name = \"{name}\"\nkind = \"{kind}\"\nversion = \"0.2.0\"\n\
                     protocol_version = \">=0.1.6, <0.5\"\n\n[capabilities]\n{capabilities}"
                ),
            )
            .unwrap();
            fs::write(dir.join(name), b"#!/bin/sh\necho hi\n").unwrap();
        }
        root
    }

    /// `totsuka setup --answers <file> --yes`, with the plugin source pinned.
    ///
    /// The exit code is whatever `doctor` decided: `setup` now ends by running
    /// it in-process and propagating exit 3, and a scratch environment has no
    /// registered secrets, so 3 is the expected outcome here. What each test
    /// asserts is what `setup` *wrote*; the doctor contract has its own test.
    fn setup_yes(&self, answers: &Path, bundled: &Path) -> (Option<i32>, String, String) {
        self.run(&[
            "setup",
            "--answers",
            answers.to_str().unwrap(),
            "--bundled-dir",
            bundled.to_str().unwrap(),
            "--yes",
        ])
    }

    /// Run `totsuka <args>` with stdin closed (so it is never a terminal).
    fn run(&self, args: &[&str]) -> (Option<i32>, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_totsuka"));
        cmd.args(args)
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars() {
            if key.starts_with("TOTSUKA_") {
                cmd.env_remove(key);
            }
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Answers selecting the minimal GitHub recipe.
///
/// The repository path has to be one that exists: `config validate` checks it
/// (`RepoPathMissing`), and the point of these tests is that the config the
/// wizard writes passes that check for real, not with the one finding excused.
fn minimal(repo: &Path) -> String {
    format!(
        "version = 1\nrecipe = 0\nsecret_backend = \"keychain\"\n\n\
         [[repositories]]\nname = \"totsuka\"\npath = \"{}\"\n\n\
         [github]\nowner = \"tomoya-k31\"\nowner_type = \"user\"\n\
         project_number = 1\ngithub_login = \"tomoya-k31\"\n",
        repo.display()
    )
}

/// Answers selecting the Slack recipe, which needs the extra blanks.
fn slack(repo: &Path) -> String {
    format!(
        "version = 1\nrecipe = 2\nsecret_backend = \"keychain\"\n\
         slack_user_id = \"U123456\"\n\n\
         [[repositories]]\nname = \"totsuka\"\npath = \"{}\"\n\n\
         [llm]\nbase_url = \"https://openrouter.ai/api/v1\"\n\
         model = \"anthropic/claude-haiku-4-5\"\n",
        repo.display()
    )
}

#[test]
fn without_a_terminal_it_refuses_rather_than_guessing() {
    // Falling back to defaults with nobody to ask would write a config the
    // user never chose. Exit 2 is "used wrong", not "failed".
    let env = Env::new("no-tty");
    let (code, _, err) = env.run(&["setup"]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("needs a terminal"), "{err}");
    assert!(
        err.contains("totsuka init"),
        "must offer a way forward: {err}"
    );
    assert!(
        !env.config_toml().exists() || fs::read_to_string(env.config_toml()).unwrap().is_empty()
    );
}

#[test]
fn dry_run_shows_the_plan_and_writes_nothing() {
    let env = Env::new("dry-run");
    let answers = env.answers(&minimal(&env.repo()));
    let _ = fs::remove_file(env.config_toml());

    let (code, out, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--dry-run"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("Plan"), "{out}");
    assert!(out.contains("implement"), "the workflow is named: {out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert!(!env.config_toml().exists(), "dry-run wrote a config");
}

#[test]
fn the_written_config_passes_config_validate() {
    // The wizard's contract. Everything else it does is cosmetic next to this.
    for name in ["minimal", "slack"] {
        let env = Env::new(&format!("valid-{name}"));
        let body = if name == "minimal" {
            minimal(&env.repo())
        } else {
            slack(&env.repo())
        };
        let answers = env.answers(&body);
        let bundled = env.bundled(&["github", "slack", "herdr", "macos"]);
        let _ = fs::remove_file(env.config_toml());

        let (_, out, err) = env.setup_yes(&answers, &bundled);
        assert!(
            env.config_toml().exists(),
            "{name}: no config written\n{out}{err}"
        );

        let (code, out, err) = env.run(&["config", "validate", "--offline"]);
        assert_eq!(
            code,
            Some(0),
            "{name}: the config setup wrote does not validate\nstdout: {out}\nstderr: {err}\n--- config ---\n{}",
            fs::read_to_string(env.config_toml()).unwrap()
        );
    }
}

#[test]
fn secret_references_are_written_but_never_values() {
    let env = Env::new("secrets");
    let answers = env.answers(&slack(&env.repo()));
    let bundled = env.bundled(&["slack", "herdr", "macos"]);
    let _ = fs::remove_file(env.config_toml());

    let (_, out, err) = env.setup_yes(&answers, &bundled);
    assert!(env.config_toml().exists(), "{out}{err}");

    let config = fs::read_to_string(env.config_toml()).unwrap();
    assert!(
        config.contains("keychain:totsuka/llm-api-key"),
        "reference not written: {config}"
    );
    // Every account the config points at must appear on the printed checklist,
    // otherwise the user finishes setup and hits "secret not found" at run time.
    for account in ["slack-user", "slack-app", "slack-bot", "llm-api-key"] {
        assert!(
            out.contains(account),
            "{account} missing from checklist: {out}"
        );
    }
    assert!(
        out.contains("security add-generic-password"),
        "no register command shown: {out}"
    );
}

#[test]
fn an_existing_config_is_skipped_not_overwritten() {
    let env = Env::new("skip-existing");
    let answers = env.answers(&minimal(&env.repo()));
    let bundled = env.bundled(&["github", "herdr"]);
    let hand_written = "# mine\nmax_concurrency = 9\n";
    fs::write(env.config_toml(), hand_written).unwrap();

    let (_, out, err) = env.setup_yes(&answers, &bundled);
    assert!(out.contains("skipped"), "{out}{err}");

    // The recipe is not written over what the user wrote: their settings stay,
    // and none of the wizard's own content appears.
    let after = fs::read_to_string(env.config_toml()).unwrap();
    assert!(after.starts_with(hand_written), "{after}");
    for absent in ["[[workflows]]", "[[repositories]]", "[llm]"] {
        assert!(
            !after.contains(absent),
            "the recipe was written into an existing config: {after}"
        );
    }
    // Enabling what it installed is not "overwriting the config" — it is the
    // same edit `plugin install --enable` makes, and it is what makes
    // re-running `setup` on a configured machine the de-facto repair
    // (ADR-0028: no separate `--repair`).
    assert!(after.contains("[plugins.github]"), "{after}");
}

#[test]
fn the_commented_skeleton_init_writes_is_filled_in() {
    // Otherwise everyone who followed the docs and ran `init` first would find
    // `setup` doing nothing at all.
    let env = Env::new("after-init");
    let (code, _, err) = env.run(&["init"]);
    assert_eq!(code, Some(0), "{err}");
    let skeleton = fs::read_to_string(env.config_toml()).unwrap();
    assert!(
        skeleton
            .lines()
            .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')),
        "this test assumes init writes only comments"
    );

    let answers = env.answers(&minimal(&env.repo()));
    let bundled = env.bundled(&["github", "herdr"]);
    let (_, out, err) = env.setup_yes(&answers, &bundled);
    assert!(env.config_toml().exists(), "{out}{err}");

    let filled = fs::read_to_string(env.config_toml()).unwrap();
    assert!(filled.contains("name = \"totsuka\""), "{filled}");
    assert!(filled.contains("[plugins.github]"), "{filled}");
    // The skeleton's guidance survives alongside the values.
    assert!(filled.contains("# totsuka configuration"), "{filled}");

    let (code, out, err) = env.run(&["config", "validate", "--offline"]);
    assert_eq!(code, Some(0), "{out}{err}\n---\n{filled}");
}

#[test]
fn one_run_goes_from_nothing_to_installed_enabled_and_diagnosed() {
    // #349's contract: `setup` is not finished when the config is written. It
    // installs the recipe's plugins, writes each plugin's own config, and hands
    // over to `doctor` — so a fresh machine needs one command, not four.
    let env = Env::new("full-flow");
    let answers = env.answers(&minimal(&env.repo()));
    let bundled = env.bundled(&["github", "herdr"]);
    let _ = fs::remove_file(env.config_toml());

    let (code, out, err) = env.setup_yes(&answers, &bundled);
    assert!(out.contains("Installed `github`"), "{out}{err}");
    assert!(out.contains("Installed `herdr`"), "{out}{err}");
    assert!(out.contains("Running `totsuka doctor`"), "{out}{err}");

    // `plugins/github.toml` exists, holds a reference rather than a token, and
    // carries the coordinates the plugin cannot default.
    let plugin_toml = env.root.join("config/totsuka/plugins/github.toml");
    let body = fs::read_to_string(&plugin_toml)
        .unwrap_or_else(|e| panic!("{} missing: {e}\n{out}", plugin_toml.display()));
    assert!(body.contains("keychain:totsuka/github-token"), "{body}");
    assert!(!body.contains("ghp_"), "a token value was written: {body}");
    assert!(body.contains("project_number = 1"), "{body}");

    // Both plugins are installed *and* enabled — the two are separate concepts
    // (F-56), and setup opts into both.
    let (_, listing, _) = env.run(&["plugin", "list", "--json"]);
    let rows: serde_json::Value = serde_json::from_str(&listing).unwrap();
    for name in ["github", "herdr"] {
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .unwrap_or_else(|| panic!("{name} not listed: {listing}"));
        assert_eq!(row["installed"], true, "{name}: {listing}");
        assert_eq!(row["enabled"], true, "{name}: {listing}");
    }

    // `doctor`'s verdict is propagated, not swallowed: unregistered secrets are
    // a real problem and exit 3 is how a script learns about it.
    assert_eq!(
        code,
        Some(3),
        "doctor found nothing to report in an environment with no secrets registered"
    );

    // …and what it reports is only work left for a human. Asserting the exact
    // set, not just "config is not in it": naming checks individually is how a
    // typo turns into an assertion that can never fire, and the check names
    // here (`config`, `plugin:<name>`) are not guessable from the flag names.
    let (_, report, _) = env.run(&["doctor", "--json"]);
    let checks: Vec<serde_json::Value> = serde_json::from_str(&report).unwrap();
    let names: Vec<&str> = checks.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert!(
        names.contains(&"config"),
        "no `config` check ran, so the assertion below proves nothing: {names:?}"
    );

    let failed: Vec<&str> = checks
        .iter()
        .filter(|c| c["ok"] == false)
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for name in &failed {
        // `state-db` needs a `totsuka run`, and the `plugin:*` probes launch
        // the fixture's shell stubs against real secrets neither of which
        // exists here. Anything else means setup wrote something broken.
        assert!(
            *name == "state-db" || name.starts_with("plugin:"),
            "setup left `{name}` failing: {failed:?}"
        );
    }
    assert!(
        !failed.contains(&"config"),
        "the config setup wrote does not pass doctor: {failed:?}"
    );

    // Re-running converges: the second pass writes nothing new.
    let config_before = fs::read_to_string(env.config_toml()).unwrap();
    let plugin_before = fs::read_to_string(&plugin_toml).unwrap();
    let (_, out, err) = env.setup_yes(&answers, &bundled);
    assert!(out.contains("skipped"), "{out}{err}");
    assert_eq!(
        fs::read_to_string(env.config_toml()).unwrap(),
        config_before,
        "a second run changed config.toml"
    );
    assert_eq!(
        fs::read_to_string(&plugin_toml).unwrap(),
        plugin_before,
        "a second run changed plugins/github.toml"
    );
}

#[test]
fn a_bad_answers_file_is_rejected_with_the_reason() {
    let env = Env::new("bad-answers");

    // Unknown field — a typo must fail loudly, not be ignored.
    let answers =
        env.answers("version = 1\nrecipe = 0\nsecret_backend = \"keychain\"\nrepositorys = []\n");
    let (code, _, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
    assert_ne!(code, Some(0));
    assert!(err.contains("not a valid answers file"), "{err}");

    // Out-of-range recipe.
    let answers = env.answers(
        "version = 1\nrecipe = 99\nsecret_backend = \"keychain\"\n\n[[repositories]]\nname = \"r\"\npath = \"/r\"\n",
    );
    let (code, _, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
    assert_ne!(code, Some(0));
    assert!(err.contains("recipe"), "{err}");

    // A recipe whose blanks are unfilled. Writing the config anyway would
    // produce one that loads (so `setup` reports success) and then fails at run
    // time — `verification = "llm"` with no `[llm]` block.
    let answers = env.answers(&format!(
        "version = 1\nrecipe = 2\nsecret_backend = \"keychain\"\n\n\
         [[repositories]]\nname = \"totsuka\"\npath = \"{}\"\n",
        env.repo().display()
    ));
    let (code, _, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
    assert_ne!(code, Some(0));
    assert!(err.contains("slack_user_id"), "{err}");

    // A missing file names the path.
    let (code, _, err) = env.run(&["setup", "--answers", "/nonexistent/answers.toml", "--yes"]);
    assert_ne!(code, Some(0));
    assert!(err.contains("/nonexistent/answers.toml"), "{err}");

    assert!(
        !Path::new(&env.config_toml()).exists()
            || fs::read_to_string(env.config_toml()).unwrap().is_empty()
    );
}

#[test]
fn save_answers_round_trips_into_a_second_run() {
    // The saved file is what makes a second machine reproducible; it has to be
    // accepted back verbatim.
    let env = Env::new("save-answers");
    let answers = env.answers(&slack(&env.repo()));
    let saved = env.root.join("saved.toml");
    let _ = fs::remove_file(env.config_toml());

    let (code, out, err) = env.run(&[
        "setup",
        "--answers",
        answers.to_str().unwrap(),
        "--save-answers",
        saved.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(saved.exists(), "nothing saved");
    // `--save-answers` writes before the plan, so answers survive a rejected
    // plan. That makes a blanket "nothing was written" false — the summary has
    // to name the one file it did write, or the claim is a lie.
    assert!(
        out.contains("nothing was configured") && out.contains("saved.toml"),
        "--dry-run claimed nothing was written while saving a file: {out}"
    );
    assert!(!env.config_toml().exists(), "dry-run wrote a config");

    let text = fs::read_to_string(&saved).unwrap();
    assert!(
        !text.contains("xoxp"),
        "an answers file must never hold a token"
    );

    let bundled = env.bundled(&["slack", "herdr", "macos"]);
    let (_, out, err) = env.setup_yes(&saved, &bundled);
    assert!(
        env.config_toml().exists(),
        "saved answers were not accepted back: {out}{err}"
    );
    let (code, _, _) = env.run(&["config", "validate", "--offline"]);
    assert_eq!(code, Some(0));
}
