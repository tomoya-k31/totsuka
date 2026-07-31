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
         [[repositories]]\nname = \"totsuka\"\npath = \"{}\"\n",
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
        let _ = fs::remove_file(env.config_toml());

        let (code, out, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
        assert_eq!(code, Some(0), "{name}: {out}{err}");
        assert!(env.config_toml().exists(), "{name}: no config written");

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
    let _ = fs::remove_file(env.config_toml());

    let (code, out, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");

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
    let hand_written = "# mine\nmax_concurrency = 9\n";
    fs::write(env.config_toml(), hand_written).unwrap();

    let (code, out, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains("skipped"), "{out}");
    assert_eq!(
        fs::read_to_string(env.config_toml()).unwrap(),
        hand_written,
        "an existing config must not be touched"
    );
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
    let (code, out, err) = env.run(&["setup", "--answers", answers.to_str().unwrap(), "--yes"]);
    assert_eq!(code, Some(0), "{out}{err}");

    let filled = fs::read_to_string(env.config_toml()).unwrap();
    assert!(filled.contains("name = \"totsuka\""), "{filled}");
    assert!(filled.contains("[plugins.github]"), "{filled}");
    // The skeleton's guidance survives alongside the values.
    assert!(filled.contains("# totsuka configuration"), "{filled}");

    let (code, out, err) = env.run(&["config", "validate", "--offline"]);
    assert_eq!(code, Some(0), "{out}{err}\n---\n{filled}");
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

    let text = fs::read_to_string(&saved).unwrap();
    assert!(
        !text.contains("xoxp"),
        "an answers file must never hold a token"
    );

    let (code, out, err) = env.run(&["setup", "--answers", saved.to_str().unwrap(), "--yes"]);
    assert_eq!(
        code,
        Some(0),
        "saved answers were not accepted back: {out}{err}"
    );
    let (code, _, _) = env.run(&["config", "validate", "--offline"]);
    assert_eq!(code, Some(0));
}
