//! End-to-end exercise of the config pipeline against real files and a real
//! environment: parse `config.toml` + `plugins/{name}.toml` from disk, resolve
//! a `${ENV}` secret, expand a plugin path, and run static validation against
//! an existing repository directory.
//!
//! Uses `CARGO_TARGET_TMPDIR` so no extra dependency is needed.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use orchestrator_core::config::{
    PluginRawConfig, RootConfig, SecretResolver, expand_path, validate_static,
};
use orchestrator_core::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// A secret store that has nothing — proves `${ENV}` resolution needs no store.
struct EmptyStore;
impl SecretStore for EmptyStore {
    fn get(&self, r: &SecretRef) -> Result<SecretString, SecretError> {
        Err(SecretError::NotFound {
            service: r.service().to_string(),
            account: r.account().to_string(),
        })
    }
}

#[test]
fn full_config_pipeline_on_disk() {
    // A real, existing directory to point a repository at.
    let repo_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("config_e2e");
    fs::create_dir_all(&tmp).unwrap();

    let config_toml = format!(
        r#"
version = 1

[plugins.github]
enabled = true
kind = "task_source"

[plugins.herdr]
enabled = true
kind = "agent_ide"
max_concurrency = 3

[[repositories]]
name = "totsuka"
path = "{repo}"
default_agent = "herdr"

[[workflows]]
name = "implement"
source = "github"
trigger = {{ project_status = "実装待ち" }}
mode = "implement"
agent = "herdr"
output = "pull_request"
on_success = {{ set_status = "レビュー待ち" }}
"#,
        repo = repo_dir.display()
    );
    let config_path = tmp.join("config.toml");
    fs::write(&config_path, &config_toml).unwrap();

    let plugin_toml = r#"
socket_path = "${XDG_RUNTIME_DIR}/herdr.sock"
design_preview = "side_pane"
api_key_ref = "${HERDR_TOKEN}"
"#;
    let plugin_path = tmp.join("herdr.toml");
    fs::write(&plugin_path, plugin_toml).unwrap();

    // Parse from disk.
    let cfg = RootConfig::from_toml_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(cfg.repositories[0].name, "totsuka");
    assert_eq!(cfg.workflows.len(), 1);

    let raw = PluginRawConfig::from_toml_str(&fs::read_to_string(&plugin_path).unwrap()).unwrap();

    // A controlled environment (not the process env, so the test is stable).
    let env_map: HashMap<String, String> = [
        ("XDG_RUNTIME_DIR", "/run/user/501"),
        ("HERDR_TOKEN", "tok_abc123"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    let env = |k: &str| env_map.get(k).cloned();

    // Resolve a ${ENV} secret from the plugin file (F-62/65).
    let resolver = SecretResolver::new(EmptyStore, &env);
    let token_ref = raw.as_table().get("api_key_ref").unwrap().as_str().unwrap();
    let token = resolver.resolve(token_ref).unwrap();
    assert_eq!(token.expose(), "tok_abc123");
    // The secret must not leak through Display.
    assert_eq!(format!("{token}"), "***");

    // Expand a plugin socket path template.
    let socket = raw.as_table().get("socket_path").unwrap().as_str().unwrap();
    assert_eq!(
        expand_path(socket, &env).unwrap(),
        PathBuf::from("/run/user/501/herdr.sock")
    );

    // Static validation against the real repo dir => no errors.
    let errors = validate_static(&cfg, &env);
    assert!(
        errors.is_empty(),
        "unexpected validation errors: {errors:?}"
    );

    fs::remove_dir_all(&tmp).ok();
}
