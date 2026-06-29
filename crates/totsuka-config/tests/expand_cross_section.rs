use totsuka_config::Config;

const MIN_TOML_WITH_CROSS_REFS: &str = r#"
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
uds_path="${totsuka.state_dir}/sock/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true

[orchestrator]
uds_path="${totsuka.state_dir}/sock/orchestrator.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="${agent_adapter.uds_path}"

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
uds_path="${totsuka.state_dir}/sock/qa-service.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="${agent_adapter.uds_path}"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]

[notifications]
[retention]
[telemetry]
"#;

#[test]
fn cross_section_state_dir_expands() {
    let c = Config::from_toml_str(MIN_TOML_WITH_CROSS_REFS).expect("parse");
    assert_eq!(c.agent_adapter.uds_path, "/var/state/sock/adapter.sock");
    assert_eq!(c.orchestrator.uds_path, "/var/state/sock/orchestrator.sock");
    assert_eq!(c.qa_service.uds_path, "/var/state/sock/qa-service.sock");
}

#[test]
fn cross_section_adapter_uds_expands_transitively() {
    let c = Config::from_toml_str(MIN_TOML_WITH_CROSS_REFS).expect("parse");
    // ${agent_adapter.uds_path} itself contains ${totsuka.state_dir}; both must resolve.
    assert_eq!(c.orchestrator.adapter_uds, "/var/state/sock/adapter.sock");
    assert_eq!(c.qa_service.adapter_uds, "/var/state/sock/adapter.sock");
}

#[test]
fn vars_table_still_works_alongside_cross_section() {
    let toml_with_both = MIN_TOML_WITH_CROSS_REFS
        .replace(
            "[agent_adapter]",
            "[vars]\nworkdir = \"/workspace\"\n\n[agent_adapter]",
        )
        .replace(
            r#"herdr_socket="/tmp/herdr.sock""#,
            r#"herdr_socket="${workdir}/herdr.sock""#,
        );
    let c = Config::from_toml_str(&toml_with_both).expect("parse");
    assert_eq!(c.agent_adapter.herdr_socket, "/workspace/herdr.sock");
    // Cross-section ref still works
    assert_eq!(c.agent_adapter.uds_path, "/var/state/sock/adapter.sock");
}

#[test]
fn undefined_cross_section_ref_left_literal_lenient() {
    let bad = MIN_TOML_WITH_CROSS_REFS.replace(
        r#"adapter_uds="${agent_adapter.uds_path}""#,
        r#"adapter_uds="${nope.missing}/x""#,
    );
    let c = Config::from_toml_str(&bad).expect("parse");
    // Lenient: undefined ${name} survives as literal (consistent with current vars-table behavior).
    assert_eq!(c.orchestrator.adapter_uds, "${nope.missing}/x");
}
