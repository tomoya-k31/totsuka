//! Round-trip: write the init template into a fake HOME, load it via
//! totsuka_config::Config::load, and verify the resulting Config has no
//! literal `~/` or `${...}` left in any path-shaped field.

use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;
use totsuka_config::Config;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const TEMPLATE: &str = include_str!("../src/commands/templates/config.toml.tmpl");
const SECRETS_TEMPLATE: &str = include_str!("../src/commands/templates/secrets.toml.tmpl");

#[test]
fn init_template_loads_with_fully_resolved_paths() {
    let _lock = ENV_LOCK.lock().unwrap();

    let tmp = TempDir::new().unwrap();
    let cfg_dir = tmp.path().join(".config").join("totsuka");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), TEMPLATE).unwrap();
    fs::write(cfg_dir.join("secrets.toml"), SECRETS_TEMPLATE).unwrap();

    let restore_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", tmp.path());

    let cfg = Config::load(cfg_dir.join("config.toml")).expect("load template");

    // Tilde paths resolved.
    assert_eq!(
        cfg.totsuka.state_dir,
        format!("{}/.local/state/totsuka", tmp.path().display())
    );
    assert_eq!(
        cfg.totsuka.data_dir,
        format!("{}/.local/share/totsuka", tmp.path().display())
    );
    // Cross-section refs resolved.
    assert_eq!(
        cfg.agent_adapter.uds_path,
        format!(
            "{}/.local/state/totsuka/sock/adapter.sock",
            tmp.path().display()
        )
    );
    assert_eq!(
        cfg.orchestrator.uds_path,
        format!(
            "{}/.local/state/totsuka/sock/orchestrator.sock",
            tmp.path().display()
        )
    );
    assert_eq!(
        cfg.orchestrator.adapter_uds,
        format!(
            "{}/.local/state/totsuka/sock/adapter.sock",
            tmp.path().display()
        )
    );
    assert_eq!(
        cfg.qa_service.uds_path,
        format!(
            "{}/.local/state/totsuka/sock/qa-service.sock",
            tmp.path().display()
        )
    );
    assert_eq!(
        cfg.qa_service.adapter_uds,
        format!(
            "{}/.local/state/totsuka/sock/adapter.sock",
            tmp.path().display()
        )
    );
    // env: ref resolved.
    assert_eq!(
        cfg.agent_adapter.repos_root,
        format!("{}/work/repos", tmp.path().display())
    );
    // Secrets merged from the secrets template (default values are empty strings).
    assert_eq!(cfg.postgres.password.expose(), "postgres");
    assert_eq!(cfg.github_watcher.github_token.expose(), "");

    match restore_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
