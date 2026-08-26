//! End-to-end exercise of the config pipeline against a real file and a real
//! environment: parse `config.toml` from disk — the Orchestrator's own keys
//! and a plugin's `[<name>]` table in one document (#554) — resolve a `${ENV}`
//! secret out of that table, expand a plugin path, and run static validation
//! against an existing repository directory.
//!
//! Uses `CARGO_TARGET_TMPDIR` so no extra dependency is needed.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use orchestrator_core::config::{RootConfig, SecretResolver, expand_path, validate_static};
use orchestrator_core::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// A secret store that has nothing — proves `${ENV}` resolution needs no store.
struct EmptyStore;
impl SecretStore for EmptyStore {
    fn get(&self, r: &SecretRef) -> Result<SecretString, SecretError> {
        Err(SecretError::NotFound {
            reference: r.to_string(),
        })
    }
}

/// A store answering one 1Password reference — the e2e stand-in for a fake
/// `op read` runner (the real wiring is `PlatformSecretStore`'s composite).
struct OpStore;
impl SecretStore for OpStore {
    fn get(&self, r: &SecretRef) -> Result<SecretString, SecretError> {
        match r {
            SecretRef::OnePassword { uri } if uri == "op://Dev/Slack/user_token" => {
                Ok(SecretString::new("xoxp-from-op"))
            }
            _ => Err(SecretError::NotFound {
                reference: r.to_string(),
            }),
        }
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
tool = "claude"

[[workflows]]
name = "implement"
source = "github"
trigger = {{ status = "実装待ち" }}
mode = "implement"
agent = "herdr"
output = "source"
on_success = {{ status = "レビュー待ち" }}

[herdr]
socket_path = "${{XDG_RUNTIME_DIR}}/herdr.sock"
request_timeout_secs = 30
api_key_ref = "${{HERDR_TOKEN}}"
user_token = "op://Dev/Slack/user_token"
"#,
        repo = repo_dir.display()
    );
    let config_path = tmp.join("config.toml");
    fs::write(&config_path, &config_toml).unwrap();

    // Parse from disk. One document now carries both layers.
    let cfg = RootConfig::from_toml_str(&fs::read_to_string(&config_path).unwrap()).unwrap();
    assert_eq!(cfg.repositories[0].name, "totsuka");
    assert_eq!(cfg.workflows.len(), 1);

    // The plugin's own table came through uninterpreted, and only it — the
    // Orchestrator's own keys are named fields and never land here.
    let raw = cfg
        .plugin_settings("herdr")
        .expect("[herdr] is held verbatim");
    assert_eq!(
        cfg.plugin_settings.keys().collect::<Vec<_>>(),
        vec!["herdr"],
        "only the plugin table is leftover"
    );

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
    let token_ref = raw.get("api_key_ref").unwrap().as_str().unwrap();
    let token = resolver.resolve(token_ref).unwrap();
    assert_eq!(token.expose(), "tok_abc123");
    // The secret must not leak through Display.
    assert_eq!(format!("{token}"), "***");

    // Resolve an `op://` secret from an arbitrary plugin string leaf (#156):
    // same resolver entry point, routed to the store instead of the env.
    let op_resolver = SecretResolver::new(OpStore, &env);
    let user_token_ref = raw.get("user_token").unwrap().as_str().unwrap();
    let user_token = op_resolver.resolve(user_token_ref).unwrap();
    assert_eq!(user_token.expose(), "xoxp-from-op");
    assert_eq!(format!("{user_token}"), "***");

    // Expand a plugin socket path template.
    let socket = raw.get("socket_path").unwrap().as_str().unwrap();
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
