use std::fs;
use tempfile::TempDir;
use totsuka_config::Config;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const TOML_WITH_TILDES: &str = r#"
[totsuka]
state_dir = "~/.local/state/totsuka"
data_dir  = "~/.local/share/totsuka"

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
uds_path="~/sock/adapter.sock"
herdr_socket="~/.config/herdr/herdr.sock"
node_capacity=8
repos_root="~/work/repos"
auto_clone=true

[orchestrator]
uds_path="~/sock/orchestrator.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="~/sock/adapter.sock"

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
uds_path="~/sock/qa-service.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="~/sock/adapter.sock"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]
[notifications]
[retention]
[telemetry]
"#;

#[test]
fn from_toml_str_expands_tildes_using_live_home() {
    let _lock = ENV_LOCK.lock().unwrap();
    // HOME comes from the test process env.
    let home = std::env::var("HOME").expect("HOME unset in test env");
    let c = Config::from_toml_str(TOML_WITH_TILDES).expect("parse");
    assert_eq!(c.totsuka.state_dir, format!("{home}/.local/state/totsuka"));
    assert_eq!(c.totsuka.data_dir, format!("{home}/.local/share/totsuka"));
    assert_eq!(c.agent_adapter.uds_path, format!("{home}/sock/adapter.sock"));
    assert_eq!(
        c.agent_adapter.herdr_socket,
        format!("{home}/.config/herdr/herdr.sock")
    );
    assert_eq!(c.agent_adapter.repos_root, format!("{home}/work/repos"));
}

#[test]
fn load_expands_tilde_in_input_path() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Write a config under a temp dir, then load via a tilde'd path.
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("config.toml");
    fs::write(&cfg_path, TOML_WITH_TILDES).unwrap();

    // Point HOME at the temp dir so "~/config.toml" resolves to cfg_path.
    let original = std::env::var("HOME").ok();
    std::env::set_var("HOME", tmp.path());

    // Move the file to the new HOME's root so "~/config.toml" finds it.
    let target = tmp.path().join("config.toml");
    if cfg_path != target {
        fs::copy(&cfg_path, &target).unwrap();
    }

    let c = Config::load("~/config.toml").expect("load via tilde'd path");
    assert_eq!(
        c.totsuka.state_dir,
        format!("{}/.local/state/totsuka", tmp.path().display())
    );

    match original {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
