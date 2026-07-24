//! End-to-end integration test for `totsuka plugin ...`, driving the real CLI
//! binary against a temporary XDG environment (F-52/F-55/F-56/F-57).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A scratch XDG environment for one test.
struct Env {
    root: PathBuf,
}

impl Env {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("totsuka-cli-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config/totsuka")).unwrap();
        Self { root }
    }

    fn config_toml(&self) -> PathBuf {
        self.root.join("config/totsuka/config.toml")
    }

    /// Run `totsuka <args>` with this env and optional stdin. Returns
    /// (success, stdout, stderr).
    fn run(&self, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
        self.run_with_env(args, &[], stdin)
    }

    /// Like [`Env::run`], with extra `TOTSUKA_*` overrides. Inherited
    /// `TOTSUKA_*` vars are stripped first so an agent session's exports do
    /// not leak into the assertions (plugin commands read the env layer since
    /// #175).
    fn run_with_env(
        &self,
        args: &[&str],
        vars: &[(&str, &str)],
        stdin: Option<&str>,
    ) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_totsuka"));
        cmd.args(args)
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, _) in std::env::vars() {
            if key.starts_with("TOTSUKA_") {
                cmd.env_remove(key);
            }
        }
        for (key, value) in vars {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().unwrap();
        if let Some(input) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
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

/// Write a fake plugin source directory (plugin.toml + binary).
fn fake_source(dir: &Path, name: &str, protocol_req: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("plugin.toml"),
        format!(
            "name = \"{name}\"\nkind = \"task_source\"\nversion = \"0.2.0\"\nprotocol_version = \"{protocol_req}\"\n"
        ),
    )
    .unwrap();
    fs::write(dir.join(name), b"#!/bin/sh\necho hi\n").unwrap();
}

#[test]
fn full_lifecycle_install_list_enable_disable_uninstall() {
    let env = Env::new("lifecycle");
    let src = env.root.join("src");
    fake_source(&src, "github", ">=0.1.6, <0.3");

    // install --yes shows source + checksum.
    let (ok, out, _) = env.run(&["plugin", "install", src.to_str().unwrap(), "--yes"], None);
    assert!(ok, "install failed");
    assert!(out.contains("SHA-256:"), "checksum not shown: {out}");
    assert!(out.contains("Installed `github`"));

    // list --json reflects the install.
    let (ok, out, _) = env.run(&["plugin", "list", "--json"], None);
    assert!(ok);
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["name"], "github");
    assert_eq!(rows[0]["installed"], true);
    assert_eq!(rows[0]["enabled"], false);
    // JSON kind is stable snake_case, not Debug ("TaskSource").
    assert_eq!(rows[0]["kind"], "task_source");

    // Seed a commented config, then enable/disable and verify formatting is kept.
    fs::write(
        env.config_toml(),
        "# my config\n[plugins.github]  # source\nenabled = false\nkind = \"task_source\"\n",
    )
    .unwrap();

    let (ok, _, _) = env.run(&["plugin", "enable", "github"], None);
    assert!(ok);
    let cfg = fs::read_to_string(env.config_toml()).unwrap();
    assert!(cfg.contains("enabled = true"));
    assert!(cfg.contains("# my config"), "comment lost: {cfg}");
    assert!(
        cfg.contains("[plugins.github]  # source"),
        "inline comment lost: {cfg}"
    );

    let (ok, _, _) = env.run(&["plugin", "disable", "github"], None);
    assert!(ok);
    let cfg = fs::read_to_string(env.config_toml()).unwrap();
    assert!(cfg.contains("enabled = false"));
    assert!(cfg.contains("# source"));

    // uninstall.
    let (ok, out, _) = env.run(&["plugin", "uninstall", "github"], None);
    assert!(ok);
    assert!(out.contains("Uninstalled"));
    let (_, out, _) = env.run(&["plugin", "list", "--json"], None);
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    // Still configured (declaration remains) but no longer installed.
    assert_eq!(rows[0]["installed"], false);
}

#[test]
fn install_then_enable_produces_loadable_config() {
    // The natural flow with no pre-seeded config: enable must write `kind` so
    // the resulting config.toml is schema-valid and later commands still work.
    let env = Env::new("install_enable");
    let src = env.root.join("src");
    fake_source(&src, "github", ">=0.1.6, <0.3");
    // A config exists (as after `init`) but has no [plugins.github] section yet.
    fs::write(env.config_toml(), "version = 1\n").unwrap();

    let (ok, _, _) = env.run(&["plugin", "install", src.to_str().unwrap(), "--yes"], None);
    assert!(ok);

    let (ok, _, stderr) = env.run(&["plugin", "enable", "github"], None);
    assert!(ok, "enable failed: {stderr}");
    let cfg = fs::read_to_string(env.config_toml()).unwrap();
    assert!(cfg.contains("enabled = true"));
    assert!(
        cfg.contains("kind = \"task_source\""),
        "kind missing: {cfg}"
    );

    // A subsequent command must not choke on the config we just wrote.
    let (ok, out, stderr) = env.run(&["plugin", "list", "--json"], None);
    assert!(ok, "list failed after enable: {stderr}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows[0]["enabled"], true);

    // Enabling something neither installed nor configured is refused.
    let (ok, _, stderr) = env.run(&["plugin", "enable", "ghost"], None);
    assert!(!ok);
    assert!(
        stderr.contains("neither installed nor declared"),
        "stderr: {stderr}"
    );
}

#[test]
fn install_requires_confirmation() {
    let env = Env::new("confirm");
    let src = env.root.join("src");
    fake_source(&src, "notion", ">=0.1.6, <0.3");

    // Answer "n": nothing is installed.
    let (ok, out, _) = env.run(&["plugin", "install", src.to_str().unwrap()], Some("n\n"));
    assert!(ok, "aborting is not an error");
    assert!(out.contains("Aborted"), "expected abort message: {out}");

    let (_, out, _) = env.run(&["plugin", "list", "--json"], None);
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(
        rows.as_array().unwrap().is_empty(),
        "must not install without confirmation"
    );
}

#[test]
fn incompatible_manifest_is_rejected() {
    let env = Env::new("incompat");
    let src = env.root.join("src");
    fake_source(&src, "future", ">=1.0.0");

    let (ok, _, stderr) = env.run(&["plugin", "install", src.to_str().unwrap(), "--yes"], None);
    assert!(!ok, "incompatible install must fail");
    assert!(stderr.contains("protocol-incompatible"), "stderr: {stderr}");
}

#[test]
fn github_source_reports_not_yet_supported() {
    let env = Env::new("github");
    let (ok, _, stderr) = env.run(&["plugin", "install", "github:owner/repo", "--yes"], None);
    assert!(!ok);
    assert!(stderr.contains("not yet available"), "stderr: {stderr}");
}

/// #175: plugin commands resolve paths through `Cx` like every other command,
/// so `--config` (F-66's highest layer) applies to them too — `Locations` used
/// to silently ignore it.
#[test]
fn plugin_list_honors_config_override() {
    let env = Env::new("config-override");
    // A config *outside* the XDG config dir, declaring one plugin.
    let alt = env.root.join("elsewhere.toml");
    fs::write(
        &alt,
        "[plugins.github]\nenabled = true\nkind = \"task_source\"\n",
    )
    .unwrap();

    let (ok, out, stderr) = env.run(
        &[
            "plugin",
            "list",
            "--json",
            "--config",
            alt.to_str().unwrap(),
        ],
        None,
    );
    assert!(ok, "stderr: {stderr}");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "github")
        .expect("the declaration from the overridden config path appears");
    assert_eq!(row["enabled"], true, "{row}");

    // The default XDG location has no config at all: without the override the
    // declaration must not be visible (or-default semantics, not an error).
    let (ok, out, _) = env.run(&["plugin", "list", "--json"], None);
    assert!(ok, "list works before `totsuka init`");
    let rows: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(rows.as_array().unwrap().is_empty(), "{rows}");
}

/// #175: plugin commands read the env layer fail-loud like every other
/// command — and the failure must land *before* the store is touched, so a
/// broken override never produces a half-done install.
#[test]
fn broken_env_override_fails_before_install_side_effects() {
    let env = Env::new("env-fail-loud");
    let src = env.root.join("src");
    fake_source(&src, "github", ">=0.1.6, <0.3");

    let (ok, _, stderr) = env.run_with_env(
        &["plugin", "install", src.to_str().unwrap(), "--yes"],
        &[("TOTSUKA_MAX_CONCURRENCY", "abc")],
        None,
    );
    assert!(!ok, "a broken override fails the command");
    assert!(
        stderr.contains("TOTSUKA_MAX_CONCURRENCY"),
        "the error names the variable: {stderr}"
    );
    assert!(
        !env.root.join("data/totsuka/plugins/github").exists(),
        "nothing may be installed when the env layer is broken"
    );
}
