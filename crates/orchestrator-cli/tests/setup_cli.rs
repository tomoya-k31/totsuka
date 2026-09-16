//! End-to-end test for `totsuka setup` (#348, rebuilt in #705), driving the
//! real CLI binary.
//!
//! A child process has no terminal, so the plugin picker cannot be exercised
//! here — that is what the unit tests in `setup::template` are for. What this
//! file covers is everything around it: the TTY gate and the `--plugins`
//! escape from it, `--dry-run`, the append-what-is-missing rule, and the
//! assertion that matters most — **the config setup writes is one
//! `totsuka config validate` accepts, and it enables nothing**.

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

    /// A fake bundled plugins tree: `<root>/bundled/<name>/{plugin.toml,<name>}`.
    ///
    /// Every run that installs pins this. Without it setup would notice that
    /// the test's working directory is inside a totsuka checkout and shell out
    /// to `cargo build`, which tests must not do (ADR-0018) — and which would
    /// also make every one of these tests take minutes.
    fn bundled(&self, names: &[&str]) -> PathBuf {
        let root = self.root.join("bundled");
        for name in names {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            // The manifests mirror the real ones closely enough for
            // `config validate`, which cross-checks a workflow's `output`
            // against the source plugin's declared capabilities.
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
                     protocol_version = \">=0.6.0, <0.8\"\n\n[capabilities]\n{capabilities}"
                ),
            )
            .unwrap();
            fs::write(dir.join(name), b"#!/bin/sh\necho hi\n").unwrap();
        }
        root
    }

    /// `totsuka setup --plugins <list>`, with the plugin source pinned.
    fn setup(&self, plugins: &str, bundled: &Path) -> (Option<i32>, String, String) {
        self.run(&[
            "setup",
            "--plugins",
            plugins,
            "--bundled-dir",
            bundled.to_str().unwrap(),
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

/// **Without a terminal there is no guessing.** A default selection would
/// install plugins nobody chose; an empty one would look like a run that
/// worked. The error names the escape and the plugins it takes.
#[test]
fn without_a_terminal_setup_stops_and_names_the_flag() {
    let env = Env::new("tty-gate");
    let (code, _, err) = env.run(&["setup"]);
    assert_eq!(code, Some(2), "stderr: {err}");
    assert!(err.contains("--plugins"), "{err}");
    assert!(err.contains("github"), "the list is named: {err}");
    assert!(err.contains("all"), "the bulk answers are named: {err}");
    assert!(!env.config_toml().exists(), "the gate wrote a config");
}

/// `--plugins` and `--secret-backend` are documented; `--bundled-dir` is the
/// test affordance and stays hidden (#466).
#[test]
fn setup_help_advertises_the_real_flags_but_not_the_test_affordance() {
    let env = Env::new("help");
    let (_, out, _) = env.run(&["setup", "--help"]);
    assert!(out.contains("--plugins"), "{out}");
    assert!(out.contains("--secret-backend"), "{out}");
    assert!(out.contains("--dry-run"), "{out}");
    assert!(!out.contains("--bundled-dir"), "{out}");
}

/// `--dry-run` prints what it would do and touches nothing.
#[test]
fn dry_run_shows_the_plan_and_writes_nothing() {
    let env = Env::new("dry-run");
    let bundled = env.bundled(&["github", "herdr"]);
    let (code, out, err) = env.run(&[
        "setup",
        "--plugins",
        "github,herdr",
        "--bundled-dir",
        bundled.to_str().unwrap(),
        "--dry-run",
    ]);
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(out.contains("Setup plan"), "{out}");
    assert!(out.contains("github, herdr"), "{out}");
    assert!(out.contains("nothing was written"), "{out}");
    assert!(!env.config_toml().exists(), "dry-run wrote a config");
}

/// One run, from nothing to a config that validates.
///
/// **The two halves of the assertion are equally load-bearing.** That the file
/// validates is the baseline. That it activates nothing but `version` is the
/// point of the whole redesign: `totsuka config validate` launches every
/// enabled plugin, so a setup that enabled what it installed would leave the
/// command meant to confirm it failing on its own output.
#[test]
fn one_run_writes_a_config_that_validates_and_enables_nothing() {
    let env = Env::new("fresh");
    let bundled = env.bundled(&["github", "herdr", "macos"]);
    let (code, out, err) = env.setup("github,herdr,macos", &bundled);
    assert_eq!(code, Some(0), "stdout: {out}\nstderr: {err}");
    assert!(out.contains("created:"), "{out}");

    let text = fs::read_to_string(env.config_toml()).unwrap();
    let parsed: toml::Table = text.parse().expect("the written config must be valid TOML");
    assert_eq!(
        parsed.keys().collect::<Vec<_>>(),
        vec!["version"],
        "setup must leave everything but `version` commented out"
    );

    let (code, _, err) = env.run(&["config", "validate", "--offline"]);
    assert_eq!(code, Some(0), "the written config must validate: {err}");

    // The plugins are installed, and none of them enabled.
    let (_, out, _) = env.run(&["plugin", "list"]);
    for name in ["github", "herdr", "macos"] {
        assert!(out.contains(name), "`{name}` was not installed: {out}");
    }
}

/// Only the selected plugins appear; the rest are not in the file at all.
#[test]
fn the_config_documents_the_selected_plugins_and_no_others() {
    let env = Env::new("selection");
    let bundled = env.bundled(&["github"]);
    let (_, _, err) = env.setup("github", &bundled);
    let text = fs::read_to_string(env.config_toml()).unwrap();
    assert!(text.contains("# [github]"), "stderr: {err}");
    assert!(text.contains("# [plugins.github]"));
    for absent in ["notion", "slack", "discord", "orca"] {
        assert!(
            !text.contains(&format!("# [plugins.{absent}]")),
            "`{absent}` was not selected but is in the config"
        );
    }
    // Core is unconditional, and so is the recipe section that replaced the
    // wizard's knowledge.
    assert!(text.contains("# [[workflows]]"));
    assert!(text.contains("[worktree]"));
    assert!(text.contains("Recipes"));
}

/// `--plugins none` is a real answer: write the config, install nothing.
#[test]
fn plugins_none_writes_the_config_and_installs_nothing() {
    let env = Env::new("none");
    let bundled = env.bundled(&["github"]);
    let (code, out, err) = env.setup("none", &bundled);
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(env.config_toml().exists());
    assert!(!out.contains("Installing plugins"), "{out}");
}

/// **A typo is refused.** `--plugins gihub` installing nothing would be
/// indistinguishable from a successful run.
#[test]
fn a_misspelled_plugin_is_refused_with_the_real_names() {
    let env = Env::new("typo");
    let (code, _, err) = env.run(&["setup", "--plugins", "gihub"]);
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("gihub"), "{err}");
    assert!(err.contains("github"), "{err}");
    assert!(!env.config_toml().exists());
}

/// Re-running adds the sections a later selection needs, and leaves the
/// operator's own lines alone.
///
/// This is how someone who picked `github` in January gets the commented
/// `[notion]` skeleton in March without going to the docs — and the reason the
/// merge appends rather than rewrites: the file it is appending to is the one
/// holding their secret references.
#[test]
fn a_second_run_appends_the_sections_the_first_did_not_have() {
    let env = Env::new("append");
    let bundled = env.bundled(&["github", "notion"]);
    env.setup("github", &bundled);

    let before = fs::read_to_string(env.config_toml()).unwrap();
    assert!(!before.contains("# [notion]"));

    // An edit of their own, which must survive.
    let edited = format!(
        "{before}\n[[repositories]]\nname = \"mine\"\npath = \"{}\"\n",
        env.repo().display()
    );
    fs::write(env.config_toml(), &edited).unwrap();

    let (code, out, err) = env.setup("notion", &bundled);
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(out.contains("updated:"), "{out}");

    let after = fs::read_to_string(env.config_toml()).unwrap();
    assert!(
        after.starts_with(&edited),
        "the existing file was rewritten, not appended to"
    );
    assert!(
        after.contains("# [notion]"),
        "the new section was not added"
    );
    assert_eq!(
        after.matches("# [github] —").count(),
        1,
        "the section it already had must not be added twice"
    );

    let (code, _, err) = env.run(&["config", "validate", "--offline"]);
    assert_eq!(
        code,
        Some(0),
        "the merged config must still validate: {err}"
    );
}

/// **References, never values.** The backend chosen on the command line is
/// what lands in the file, not only in the printed checklist: a checklist that
/// says `security add-generic-password` over a config full of `op://` sends
/// the operator to register a secret nothing will read.
#[test]
fn secret_references_are_written_in_the_chosen_backend_and_never_values() {
    let env = Env::new("secrets");
    let bundled = env.bundled(&["github"]);
    let (_, out, err) = env.run(&[
        "setup",
        "--plugins",
        "github",
        "--secret-backend",
        "keychain",
        "--bundled-dir",
        bundled.to_str().unwrap(),
    ]);
    assert_eq!(err, "", "stderr should be empty: {err}");

    let text = fs::read_to_string(env.config_toml()).unwrap();
    assert!(
        text.contains(r#"# token = "keychain:totsuka/github-token""#),
        "{text}"
    );
    assert!(
        !text.contains(r#"= "op://"#),
        "another backend's form was written as a value; `op://` may only appear \
         in the `Other forms:` comment"
    );
    assert!(
        text.contains("Other forms: op://"),
        "the alternatives are still named, so switching store needs no docs"
    );
    assert!(
        out.contains("security add-generic-password"),
        "the register command is printed: {out}"
    );
}

/// The last thing it prints is where the file is and that it needs editing.
#[test]
fn the_run_ends_by_naming_the_file_and_asking_for_an_edit() {
    let env = Env::new("next-steps");
    let bundled = env.bundled(&["github"]);
    let (_, out, _) = env.setup("github", &bundled);
    assert!(out.contains("Your configuration is at:"), "{out}");
    assert!(
        out.contains(env.config_toml().to_str().unwrap()),
        "the absolute path is printed: {out}"
    );
    assert!(out.contains("Nothing in it is active yet"), "{out}");
    assert!(out.contains("totsuka config validate"), "{out}");
    assert!(out.contains("totsuka doctor"), "{out}");
    assert!(out.contains("totsuka run --dry-run"), "{out}");
}
