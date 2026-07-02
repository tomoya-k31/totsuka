//! This is the only test in this file, and the only test in the whole
//! `totsuka-config` test suite that mutates `PATH`. Each `tests/*.rs` file
//! compiles to its own process, so this cannot race other test files; a
//! second `#[test]` fn added to *this* file would race it via shared
//! process env, so keep this file single-test (see plan Task 2 constraint
//! in docs/superpowers/plans/2026-07-02-secrets-toml-1password-op-resolution.md).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use totsuka_config::Config;

const CONFIG_TOML: &str = r#"
[totsuka]
state_dir = "/var/state"
data_dir  = "/var/data"

[supervisor]
[supervisor.heartbeat]

[postgres]
image="ghcr.io/pgmq/pg18-pgmq:v1.11.1"
container="totsuka-pgmq"
host="127.0.0.1"
port=5432
database="totsuka"
user="postgres"
volume="totsuka_pgmq_data"
compose_file="deploy/docker-compose.yml"

[bus]
queue_name="totsuka_events"

[agent_adapter]
uds_path="/sock/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true

[orchestrator]
uds_path="/sock/orchestrator.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="/sock/adapter.sock"

[github]
project_owner="o"
project_number=1
[github.columns]
inbox="📥"
ready="📋"
design="🤖"
design_review="🚧"
impl_verify="🤖"
final_review="🚧"
awaiting_release="🚀"
released="🏁"

[github_watcher]
bind="127.0.0.1:7802"

[qa_service]
uds_path="/sock/qa-service.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="/sock/adapter.sock"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]
[notifications]
[retention]
[telemetry]
"#;

const SECRETS_TOML: &str = r#"
[postgres]
password = "op://Vault/Item/field"

[github_watcher]
github_token = "op://Vault/Item/field"
"#;

#[test]
fn op_refs_resolved_through_full_pipeline_and_cached() {
    let bin_dir = TempDir::new().unwrap();
    let calls_log = bin_dir.path().join("calls.log");
    let op_script = bin_dir.path().join("op");
    fs::write(
        &op_script,
        format!(
            "#!/bin/sh\necho \"$2\" >> {}\necho resolved-secret\n",
            calls_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&op_script, fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", bin_dir.path().display(), original_path),
    );

    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    fs::write(&cfg_path, CONFIG_TOML).unwrap();
    fs::write(cfg_dir.path().join("secrets.toml"), SECRETS_TOML).unwrap();

    let result = Config::load(&cfg_path);

    std::env::set_var("PATH", original_path);

    let c = result.expect("load");
    assert_eq!(c.postgres.password.expose(), "resolved-secret");
    assert_eq!(c.github_watcher.github_token.expose(), "resolved-secret");

    // Same op:// URI ("op://Vault/Item/field") appears twice above but must
    // only invoke the fake `op` script once, due to the in-process cache.
    let calls = fs::read_to_string(&calls_log).unwrap_or_default();
    assert_eq!(
        calls.lines().count(),
        1,
        "expected exactly one op invocation, got: {calls:?}"
    );
}
