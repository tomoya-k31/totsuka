//! spec §11.4: build the display-name ↔ ColumnId map from [github].columns.
//! Unknown display names returned by the GitHub API are surfaced as
//! WatcherError::UnknownColumn at resolve time (see polling/project.rs).

use crate::error::WatcherError;
use totsuka_config::Config;
use totsuka_core::{ColumnMap, ColumnMapError};

pub fn build(config: &Config) -> Result<ColumnMap, WatcherError> {
    ColumnMap::try_new(config.github.columns.clone())
        .map_err(|e: ColumnMapError| WatcherError::ColumnMap(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use totsuka_core::ColumnId;

    fn cfg_with(columns: HashMap<ColumnId, String>) -> Config {
        // Minimal Config construction is awkward; assemble TOML and parse.
        let mut toml = String::from(
            r#"
[totsuka]
state_dir = "/tmp/s"
data_dir  = "/tmp/d"

[supervisor]
[supervisor.heartbeat]

[postgres]
image = "x"
container = "x"
host = "127.0.0.1"
port = 5432
database = "totsuka"
user = "postgres"
volume = "/tmp/v"
compose_file = "/tmp/c"

[bus]
queue_name = "q"

[agent_adapter]
uds_path     = "/tmp/a.sock"
herdr_socket = "/tmp/h.sock"
node_capacity = 4
repos_root   = "/tmp/repos"
auto_clone   = false

[orchestrator]
uds_path                    = "/tmp/o.sock"
wip_global                  = 4
phase_timeout_default_secs  = 3600
retry_max                   = 3
stuck_threshold_secs        = 3600
adapter_uds                 = "/tmp/a.sock"

[orchestrator.claude_argv]

[github]
project_owner  = "acme"
project_number = 1

[github.columns]
"#,
        );
        for (id, name) in &columns {
            toml.push_str(&format!("{} = \"{}\"\n", id.as_snake(), name));
        }
        toml.push_str(
            r#"
[github_watcher]
bind = "127.0.0.1:7802"

[qa_service]
uds_path         = "/tmp/q.sock"
allowed_user_ids = []
catchup_channels = []
reaction_trigger = "memo"
default_mode     = "auto"
adapter_uds      = "/tmp/a.sock"

[qa_service.classifier]
provider  = "anthropic"
model     = "claude-haiku-4-5-20251001"

[qa_service.answer]

[notifications]
[notifications.slack]
[notifications.github]

[retention]
[telemetry]
"#,
        );
        Config::from_toml_str(&toml).unwrap()
    }

    fn full_map() -> HashMap<ColumnId, String> {
        let mut m = HashMap::new();
        m.insert(ColumnId::Inbox, "📥 Inbox".into());
        m.insert(ColumnId::Ready, "📋 Ready".into());
        m.insert(ColumnId::Design, "🤖 調査・設計".into());
        m.insert(ColumnId::DesignReview, "🚧 設計レビュー".into());
        m.insert(ColumnId::ImplVerify, "🤖 実装・受入検証".into());
        m.insert(ColumnId::FinalReview, "🚧 最終レビュー".into());
        m.insert(ColumnId::AwaitingRelease, "🚀 リリース待ち".into());
        m.insert(ColumnId::Released, "🏁 完了".into());
        m
    }

    #[test]
    fn build_succeeds_with_full_map() {
        let c = cfg_with(full_map());
        let m = build(&c).unwrap();
        assert_eq!(m.resolve("📥 Inbox"), Some(ColumnId::Inbox));
        assert_eq!(m.resolve("🏁 完了"), Some(ColumnId::Released));
    }
}
