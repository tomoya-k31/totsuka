use totsuka_config::{Config, LoadError, ValidationError};

#[test]
fn example_file_parses_and_validates() {
    let path = format!(
        "{}/../../examples/totsuka.toml.example",
        env!("CARGO_MANIFEST_DIR")
    );
    let txt = std::fs::read_to_string(&path).expect("read example");
    let cfg = Config::from_toml_str(&txt).expect("parse example");
    cfg.validate().expect("validate example");
}

#[test]
fn example_file_loads_via_load() {
    let path = format!(
        "{}/../../examples/totsuka.toml.example",
        env!("CARGO_MANIFEST_DIR")
    );
    Config::load(&path).expect("Config::load should succeed on the example file");
}

/// When both `agent_adapter.uds_path` and `orchestrator.uds_path` expand to the
/// same value via a shared `[vars]` entry, `Config::load` must return
/// `LoadError::Validation` containing `ValidationError::UdsCollision`.
#[test]
fn expand_then_validate_uds_collision() {
    let toml = r#"
[vars]
runtime_dir = "/tmp"

[totsuka]
state_dir = "/tmp/state"
data_dir  = "/tmp/data"

[supervisor]
[supervisor.heartbeat]

[postgres]
image="ghcr.io/pgmq/pg18-pgmq:v1.10.0"
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
uds_path="${runtime_dir}/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true

[orchestrator]
uds_path="${runtime_dir}/adapter.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="/tmp/sock/adapter.sock"

[github]
project_owner="org"
project_number=1
[github.columns]
inbox="Inbox"
ready="Ready"
design="Design"
design_review="Design Review"
impl_verify="Impl Verify"
final_review="Final Review"
awaiting_release="Awaiting Release"
released="Released"

[github_watcher]
bind="127.0.0.1:7802"

[qa_service]
uds_path="/tmp/sock/qa.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="/tmp/sock/adapter.sock"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]

[notifications]
[retention]
[telemetry]
"#;

    // Write to a temp file so Config::load can read it
    let tmp = std::env::temp_dir().join("totsuka_uds_collision_test.toml");
    std::fs::write(&tmp, toml).expect("write temp toml");

    let result = Config::load(&tmp);
    std::fs::remove_file(&tmp).ok();

    match result {
        Err(LoadError::Validation(errs)) => {
            assert!(
                errs.contains(&ValidationError::UdsCollision),
                "expected UdsCollision in {errs:?}"
            );
        }
        other => panic!("expected LoadError::Validation(UdsCollision), got: {other:?}"),
    }
}
