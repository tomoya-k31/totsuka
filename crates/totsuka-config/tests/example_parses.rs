use totsuka_config::{Config, LoadError, ValidationError};

/// Minimal TOML that satisfies the schema + every validation rule. Tests
/// build their fixtures by appending to this base (the [vars] block can be
/// prepended for expansion tests).
const MIN_TOML: &str = r#"
[totsuka]
state_dir = "/tmp/state"
data_dir  = "/tmp/data"

[supervisor]
[supervisor.heartbeat]

[postgres]
image        = "ghcr.io/pgmq/pg18-pgmq:v1.11.1"
container    = "totsuka-pgmq"
host         = "127.0.0.1"
port         = 5432
database     = "totsuka"
user         = "postgres"
volume       = "totsuka_pgmq_data"
compose_file = "deploy/docker-compose.yml"

[bus]
queue_name = "totsuka_events"

[agent_adapter]
uds_path     = "%ADAPTER%"
herdr_socket = "/tmp/herdr.sock"
node_capacity = 8
repos_root   = "/tmp/repos"
auto_clone   = true
[agent_adapter.repos."x/y"]
description     = "test repo"
worktree_subdir = ".worktree"

[orchestrator]
uds_path                   = "%ORCHESTRATOR%"
wip_global                 = 3
phase_timeout_default_secs = 1800
retry_max                  = 1
stuck_threshold_secs       = 600
adapter_uds                = "%ADAPTER%"

[github]
project_owner  = "org"
project_number = 1
[github.columns]
inbox            = "Inbox"
ready            = "Ready"
design           = "Design"
design_review    = "Design Review"
impl_verify      = "Impl Verify"
final_review     = "Final Review"
awaiting_release = "Awaiting Release"
released         = "Released"

[github_watcher]
bind = "127.0.0.1:7802"

[qa_service]
uds_path         = "/tmp/sock/qa.sock"
allowed_user_ids = ["U1"]
catchup_channels = ["C1"]
reaction_trigger = "memo"
default_mode     = "delegated"
adapter_uds      = "%ADAPTER%"
[qa_service.classifier]
provider = "anthropic"
model    = "claude-haiku-4-5-20251001"
[qa_service.answer]

[notifications]
[notifications.slack]
%SLACK_WEBHOOK%
[notifications.github]

[retention]
[telemetry]
"#;

fn render(adapter: &str, orchestrator: &str, slack_webhook_line: &str) -> String {
    MIN_TOML
        .replace("%ADAPTER%", adapter)
        .replace("%ORCHESTRATOR%", orchestrator)
        .replace("%SLACK_WEBHOOK%", slack_webhook_line)
}

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
fn example_file_loads_via_load_pipeline() {
    let path = format!(
        "{}/../../examples/totsuka.toml.example",
        env!("CARGO_MANIFEST_DIR")
    );
    let cfg = Config::load(&path).expect("Config::load on example");
    // Sensitive fields default to empty Secret<String> when absent (production
    // reads them from secrets.toml or env override).
    assert!(cfg.postgres.password.expose().is_empty());
    assert!(cfg.github_watcher.github_token.expose().is_empty());
}

#[test]
fn webhook_url_secret_masks_in_debug() {
    let toml = render(
        "/tmp/sock/a.sock",
        "/tmp/sock/o.sock",
        r#"webhook_url = "https://hooks.slack.com/services/SECRET-PATH""#,
    );
    let cfg = Config::from_toml_str(&toml).expect("parse");
    let dbg = format!("{:?}", cfg.notifications.slack.webhook_url);
    assert_eq!(dbg, "Secret(***)");
    assert!(
        !dbg.contains("SECRET-PATH"),
        "Debug of Secret<String> leaked inner value: {dbg}"
    );
    // .expose() returns the original for outbound HTTP construction.
    assert_eq!(
        cfg.notifications.slack.webhook_url.expose(),
        "https://hooks.slack.com/services/SECRET-PATH"
    );
}

#[test]
fn load_detects_post_expansion_uds_collision() {
    // Two UDS paths reference the same `[vars] runtime_dir`; once expanded
    // they collide and `validate()` should report `UdsCollision`. This proves
    // that expand_vars runs BEFORE validate (Important #7).
    let body = format!(
        "[vars]\nruntime_dir = \"/run/totsuka\"\n{}",
        render("${runtime_dir}/sock.sock", "${runtime_dir}/sock.sock", "",)
    );
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("totsuka.toml");
    std::fs::write(&p, body).unwrap();

    let err = Config::load(&p).expect_err("expected UdsCollision after expansion");
    match err {
        LoadError::Validation(errs) => {
            assert!(
                errs.iter()
                    .any(|e| matches!(e, ValidationError::UdsCollision)),
                "expected UdsCollision in errors: {errs:?}"
            );
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn dm_copy_enabled_defaults_true() {
    // [qa_service.answer] はキー省略可 — dm_copy_enabled は明示しなければ有効。
    let toml = render("/tmp/sock/a.sock", "/tmp/sock/o.sock", "");
    let cfg = Config::from_toml_str(&toml).expect("parse");
    assert!(cfg.qa_service.answer.dm_copy_enabled);
}

#[test]
fn self_mention_defaults_disabled() {
    let toml = render("/tmp/sock/a.sock", "/tmp/sock/o.sock", "");
    let cfg = Config::from_toml_str(&toml).expect("parse");
    assert_eq!(cfg.qa_service.self_mention_user_id, "");
    assert!(cfg.qa_service.slack_user_token.expose().is_empty());
}
