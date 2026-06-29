use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;
use totsuka_config::Config;

// Guard against test 3's env mutation racing against tests 1 & 2 which also
// read env via expand_string_leaf. All three tests must hold this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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
password = "supersecret"

[github_watcher]
github_token = "ghp_abcdef"

[qa_service]
slack_app_token = "xapp-1"
slack_bot_token = "xoxb-1"

[qa_service.classifier]
api_key = "sk-ant-1"
"#;

fn write_pair(dir: &std::path::Path, secrets: Option<&str>) -> std::path::PathBuf {
    let cfg = dir.join("config.toml");
    fs::write(&cfg, CONFIG_TOML).unwrap();
    if let Some(s) = secrets {
        fs::write(dir.join("secrets.toml"), s).unwrap();
    }
    cfg
}

#[test]
fn secrets_toml_values_merged_into_config() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let cfg_path = write_pair(tmp.path(), Some(SECRETS_TOML));
    let c = Config::load(&cfg_path).expect("load");
    assert_eq!(c.postgres.password.expose(), "supersecret");
    assert_eq!(c.github_watcher.github_token.expose(), "ghp_abcdef");
    assert_eq!(c.qa_service.slack_app_token.expose(), "xapp-1");
    assert_eq!(c.qa_service.slack_bot_token.expose(), "xoxb-1");
    assert_eq!(c.qa_service.classifier.api_key.expose(), "sk-ant-1");
}

#[test]
fn secrets_toml_optional_loader_works_without_file() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let cfg_path = write_pair(tmp.path(), None);
    let c = Config::load(&cfg_path).expect("load");
    // Default Secret<String> is the empty string.
    assert_eq!(c.postgres.password.expose(), "");
}

#[test]
fn env_override_wins_over_secrets_toml() {
    let _lock = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let cfg_path = write_pair(tmp.path(), Some(SECRETS_TOML));
    std::env::set_var("TOTSUKA__POSTGRES__PASSWORD", "envwins");
    let c = Config::load(&cfg_path).expect("load");
    assert_eq!(c.postgres.password.expose(), "envwins");
    std::env::remove_var("TOTSUKA__POSTGRES__PASSWORD");
}
