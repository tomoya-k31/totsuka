use std::path::Path;
use tempfile::TempDir;
use totsukactl::child::spec::{specs_from_config, RUST_BINS_IN_ORDER};
use totsukactl::paths::Paths;

const MIN_TOML: &str = r#"
[totsuka]
log_level="trace"
state_dir="/tmp/state"
data_dir="/tmp/data"
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
uds_path="/tmp/sock/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true
[orchestrator]
uds_path="/tmp/sock/orc.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="/tmp/sock/adapter.sock"
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

#[test]
fn specs_cover_all_four_bins_in_startup_order() {
    let cfg = totsuka_config::Config::from_toml_str(MIN_TOML).unwrap();
    let tmp = TempDir::new().unwrap();
    let paths = Paths {
        state_dir: tmp.path().into(),
        data_dir: tmp.path().into(),
        log_dir: tmp.path().join("logs"),
        pid_dir: tmp.path().join("pids"),
        sock_dir: tmp.path().join("sock"),
    };
    let exe_dir = Path::new("/usr/local/bin");
    let specs = specs_from_config(&cfg, &paths, exe_dir, "/etc/totsuka/config.toml");
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, RUST_BINS_IN_ORDER);
    let s = &specs[0];
    assert_eq!(s.bin_path, exe_dir.join("agent-adapter"));
    assert_eq!(
        s.args,
        vec![
            "--config".to_string(),
            "/etc/totsuka/config.toml".to_string()
        ]
    );
    assert!(s
        .env
        .iter()
        .any(|(k, v)| k == "TOTSUKA_CONFIG" && v == "/etc/totsuka/config.toml"));
    assert!(s.env.iter().any(|(k, v)| k == "RUST_LOG" && v == "trace"));
    assert_eq!(s.log_path, paths.log_dir.join("agent-adapter.log"));
}
