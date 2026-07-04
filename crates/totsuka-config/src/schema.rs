use serde::Deserialize;
use std::collections::HashMap;
use totsuka_core::{ColumnId, Secret};

/// Default helper for sensitive `Secret<String>` fields that may be absent in
/// dev TOML (production values come from env override or `secrets.toml`).
fn default_secret() -> Secret<String> {
    Secret::new(String::new())
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub totsuka: TotsukaSection,
    pub supervisor: SupervisorSection,
    pub postgres: PostgresSection,
    pub bus: BusSection,
    pub agent_adapter: AgentAdapterSection,
    pub orchestrator: OrchestratorSection,
    pub github: GithubSection,
    pub github_watcher: GithubWatcherSection,
    pub qa_service: QaServiceSection,
    pub notifications: NotificationsSection,
    pub retention: RetentionSection,
    pub telemetry: TelemetrySection,
    #[serde(default)]
    pub secrets: SecretsSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TotsukaSection {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub state_dir: String,
    pub data_dir: String,
    #[serde(default = "default_tz")]
    pub timezone: String,
}

fn default_log_level() -> String {
    "info".into()
}
fn default_tz() -> String {
    "Asia/Tokyo".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SupervisorSection {
    #[serde(default = "d_30")]
    pub ready_timeout_secs: u64,
    #[serde(default = "d_15")]
    pub shutdown_grace_secs: u64,
    #[serde(default = "d_5")]
    pub shutdown_kill_secs: u64,
    #[serde(default)]
    pub recreate_on_image_mismatch: bool,
    pub heartbeat: HeartbeatSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatSection {
    #[serde(default = "d_5")]
    pub healthz_interval_secs: u64,
    #[serde(default = "d_30")]
    pub readyz_interval_secs: u64,
    #[serde(default = "d_30")]
    pub pgmq_interval_secs: u64,
    #[serde(default = "d_3")]
    pub unhealthy_threshold: u32,
    #[serde(default = "d_2")]
    pub degraded_threshold: u32,
    #[serde(default = "d_restart_policy")]
    pub restart_policy: String,
    #[serde(default = "d_backoff")]
    pub restart_backoff_secs: Vec<u64>,
    #[serde(default = "d_5")]
    pub restart_max_attempts: u32,
    #[serde(default)]
    pub notify_on_degraded: bool,
}

fn d_restart_policy() -> String {
    "on-dead-only".into()
}
fn d_backoff() -> Vec<u64> {
    vec![5, 15, 60]
}
fn d_2() -> u32 {
    2
}
fn d_3() -> u32 {
    3
}
fn d_5<T: From<u32>>() -> T {
    T::from(5)
}
fn d_15() -> u64 {
    15
}
fn d_30() -> u64 {
    30
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostgresSection {
    pub image: String,
    pub container: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub volume: String,
    pub compose_file: String,
    #[serde(default = "default_secret")]
    pub password: Secret<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BusSection {
    pub queue_name: String,
    #[serde(default = "d_30")]
    pub visibility_secs: u64,
    #[serde(default = "d_bs")]
    pub batch_size: u32,
    #[serde(default = "d_pi")]
    pub poll_interval_ms: u64,
}

fn d_bs() -> u32 {
    16
}
fn d_pi() -> u64 {
    200
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentAdapterSection {
    pub uds_path: String,
    #[serde(default)]
    pub tcp_bind: String,
    pub herdr_socket: String,
    pub node_capacity: u32,
    pub repos_root: String,
    pub auto_clone: bool,
    #[serde(default = "d_72")]
    pub worktree_failed_ttl_hours: u64,
    #[serde(default = "d_3600")]
    pub worktree_orphan_scan_interval_secs: u64,
    #[serde(default)]
    pub vars: HashMap<String, String>,
    #[serde(default)]
    pub repos: HashMap<String, RepoSection>,
}

fn d_72() -> u64 {
    72
}
fn d_3600() -> u64 {
    3600
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoSection {
    pub description: String,
    #[serde(default)]
    pub repo_path: Option<String>,
    #[serde(default)]
    pub worktree_subdir: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OrchestratorSection {
    pub uds_path: String,
    pub wip_global: u32,
    pub phase_timeout_default_secs: u64,
    #[serde(default)]
    pub phase_timeout: HashMap<String, u64>,
    pub retry_max: u32,
    pub stuck_threshold_secs: u64,
    pub adapter_uds: String,
    #[serde(default)]
    pub claude_argv: ClaudeArgvSection,
    #[serde(default)]
    pub prompts: PromptsSection,
}

/// Per-phase prompt templates sent to a freshly spawned agent.
/// Placeholders: `{repo}`, `{issue_number}`, `{branch}`, `{task_id}`,
/// `{project_owner}`, `{project_number}`.
#[derive(Debug, Clone, Deserialize)]
pub struct PromptsSection {
    #[serde(default = "default_design_prompt")]
    pub design: String,
    #[serde(default = "default_impl_verify_prompt")]
    pub impl_verify: String,
}

impl Default for PromptsSection {
    fn default() -> Self {
        Self {
            design: default_design_prompt(),
            impl_verify: default_impl_verify_prompt(),
        }
    }
}

fn default_design_prompt() -> String {
    "あなたは {repo} の調査・設計フェーズ担当です。\
     `gh issue view {issue_number}` で issue #{issue_number} の内容を確認し、\
     このリポジトリのコードを調査した上で設計をまとめてください。\n\
     制約: ファイルの編集・作成、git commit・ブランチ作成・PR 作成は\
     行わないでください（読み取りと gh コマンドのみ）。\n\
     成果物: 設計を `gh issue comment {issue_number}` で issue #{issue_number} の\
     コメントとして投稿してください。\n\
     完了後: GitHub Project {project_number}（owner: {project_owner}）の\
     このカード（item ID: {task_id}）の Status を「🚧 設計レビュー」に\
     `gh project item-edit` で変更してください（field/option の ID は\
     `gh project field-list {project_number} --owner {project_owner}` で確認できます）。"
        .to_string()
}

fn default_impl_verify_prompt() -> String {
    "あなたは {repo} の実装・受入検証フェーズ担当です。以下の手順で進めてください。\n\
     1. コンテキスト把握: `gh issue view {issue_number} --comments` で issue \
     #{issue_number} の本文と全コメントを時系列に読んでください。設計フェーズが\
     投稿した設計コメントを含め、後のコメントほど新しい合意として優先します。\n\
     2. 実装: 現在のブランチ {branch} で実装し、このリポジトリのテスト・lint を\
     通した上でコミットしてください。\n\
     3. PR 作成: push して PR を作成してください。PR 本文に `Closes #{issue_number}` と\
     `Totsuka-Task: {task_id}` の行を含めてください。PR のマージは行わないでください。\n\
     4. CI 監視: `gh pr checks --watch` で CI を監視し、fail なら原因を修正して push し、\
     全チェックが green になるまで繰り返してください。\n\
     5. レビュー対応: CI green 後、10 分間を待機窓として 1〜2 分間隔でレビュー\
     （未解決レビュースレッド・PR コメント）をポーリングしてください。新規の指摘が\
     来たら返信・修正 push を行い（CI が再び green になることを確認）、待機窓を\
     仕切り直してください。10 分間新着がなく未解決スレッドが 0 になったら完了です。\n\
     6. 完了後: GitHub Project {project_number}（owner: {project_owner}）の\
     このカード（item ID: {task_id}）の Status を「🚧 最終レビュー」に\
     `gh project item-edit` で変更してください（field/option の ID は\
     `gh project field-list {project_number} --owner {project_owner}` で確認できます）。\n\
     エスカレーション: 修正を繰り返しても CI が通らない・指摘に対応しきれない場合は、\
     カードの Status は変更せず、状況を PR コメントで報告して終了してください\
     （PR 作成前に失敗した場合のみ issue #{issue_number} へのコメントで報告）。"
        .to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeArgvSection {
    #[serde(default)]
    pub global: Vec<String>,
    #[serde(default)]
    pub per_repo: HashMap<String, ClaudeArgvExtra>,
    #[serde(default)]
    pub per_phase: HashMap<String, ClaudeArgvExtra>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaudeArgvExtra {
    #[serde(default)]
    pub extra: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubSection {
    pub project_owner: String,
    pub project_number: u64,
    #[serde(default = "d_status")]
    pub status_field: String,
    pub columns: HashMap<ColumnId, String>,
}

fn d_status() -> String {
    "Status".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubWatcherSection {
    pub bind: String,
    #[serde(default = "d_20")]
    pub project_poll_interval_secs: u64,
    #[serde(default = "d_60")]
    pub issues_poll_interval_secs: u64,
    #[serde(default = "d_24")]
    pub catchup_window_hours: u64,
    #[serde(default = "d_100")]
    pub graphql_page_size: u32,
    #[serde(default = "default_secret")]
    pub github_token: Secret<String>,
}

fn d_20() -> u64 {
    20
}
fn d_60() -> u64 {
    60
}
fn d_24() -> u64 {
    24
}
fn d_100() -> u32 {
    100
}

#[derive(Debug, Clone, Deserialize)]
pub struct QaServiceSection {
    pub uds_path: String,
    pub allowed_user_ids: Vec<String>,
    pub catchup_channels: Vec<String>,
    pub reaction_trigger: String,
    pub default_mode: String, // "auto" | "delegated"
    pub adapter_uds: String,
    #[serde(default = "d_llm")]
    pub repo_select_mode: String,
    pub classifier: ClassifierSection,
    pub answer: AnswerSection,
    #[serde(default = "default_secret")]
    pub slack_app_token: Secret<String>,
    #[serde(default = "default_secret")]
    pub slack_bot_token: Secret<String>,
}

fn d_llm() -> String {
    "llm_classify".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClassifierSection {
    pub provider: String, // anthropic | openai | openrouter | litellm | openai_compatible
    pub model: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default = "default_secret")]
    pub api_key: Secret<String>,
    #[serde(default = "d_256")]
    pub max_tokens: u32,
    #[serde(default = "d_th")]
    pub confidence_threshold: f64,
    #[serde(default = "d_tc")]
    pub top_candidates: u32,
    #[serde(default = "d_low")]
    pub on_low_confidence: String,
    #[serde(default = "d_true")]
    pub include_thread_context: bool,
    #[serde(default = "d_15")]
    pub request_timeout_secs: u64,
}

fn d_256() -> u32 {
    256
}
fn d_th() -> f64 {
    0.70
}
fn d_tc() -> u32 {
    3
}
fn d_low() -> String {
    "delegated_reaction".into()
}
fn d_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnswerSection {
    #[serde(default = "d_sentinel")]
    pub sentinel: String,
    #[serde(default = "d_open")]
    pub answer_open_tag: String,
    #[serde(default = "d_close")]
    pub answer_close_tag: String,
    #[serde(default = "d_1500")]
    pub poll_interval_ms: u64,
    #[serde(default = "d_8")]
    pub stable_revision_secs: u64,
    #[serde(default = "d_180")]
    pub answer_timeout_secs: u64,
    #[serde(default = "d_1800")]
    pub pane_idle_ttl_secs: u64,
    #[serde(default = "d_4")]
    pub max_concurrent_answers: u32,
}

fn d_sentinel() -> String {
    "<<TOTSUKA_DONE>>".into()
}
fn d_open() -> String {
    "<answer>".into()
}
fn d_close() -> String {
    "</answer>".into()
}
fn d_1500() -> u64 {
    1500
}
fn d_8() -> u64 {
    8
}
fn d_180() -> u64 {
    180
}
fn d_1800() -> u64 {
    1800
}
fn d_4() -> u32 {
    4
}

#[derive(Debug, Clone, Deserialize)]
pub struct NotificationsSection {
    #[serde(default = "d_true")]
    pub config_error_notify: bool,
    #[serde(default = "d_600")]
    pub dedup_default_secs: u64,
    #[serde(default = "d_30u")]
    pub rate_limit_per_min: u32,
    #[serde(default)]
    pub dedup_ttl_secs: HashMap<String, u64>,
    #[serde(default)]
    pub slack: SlackNotifySection,
    #[serde(default)]
    pub github: GithubNotifySection,
}

fn d_600() -> u64 {
    600
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SlackNotifySection {
    #[serde(default = "default_secret")]
    pub webhook_url: Secret<String>,
    #[serde(default)]
    pub default_channel: String,
    #[serde(default)]
    pub channel_overrides: HashMap<String, String>,
    #[serde(default = "d_10")]
    pub bucket_capacity: u32,
    #[serde(default = "d_5")]
    pub bucket_refill_per_min: u32,
}

fn d_10() -> u32 {
    10
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct GithubNotifySection {
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RetentionSection {
    #[serde(default = "d_4")]
    pub events_weeks: u32,
    #[serde(default = "d_30u")]
    pub snapshot_days: u32,
    #[serde(default = "d_1024")]
    pub logs_max_mb: u32,
    #[serde(default = "d_50")]
    pub log_file_max_mb: u32,
}

fn d_30u() -> u32 {
    30
}
fn d_1024() -> u32 {
    1024
}
fn d_50() -> u32 {
    50
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelemetrySection {
    #[serde(default = "d_true")]
    pub metrics_enabled: bool,
    #[serde(default)]
    pub otlp_endpoint: String,
    #[serde(default = "d_ratio")]
    pub trace_sample_ratio: f64,
}

fn d_ratio() -> f64 {
    0.1
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SecretsSection {
    #[serde(default = "d_secret_days")]
    pub rotation_warn_days: u32,
}

fn d_secret_days() -> u32 {
    30
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_TOML: &str = r#"
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
project_owner="org"
project_number=1
[github.columns]
inbox="📥 Inbox"
ready="📋 Ready"
design="🤖 調査・設計"
design_review="🚧 設計レビュー"
impl_verify="🤖 実装・受入検証"
final_review="🚧 最終レビュー"
awaiting_release="🚀 リリース待ち"
released="🏁 完了"

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
    fn parses_minimal_config() {
        let c = Config::from_toml_str(MIN_TOML).expect("parse");
        assert_eq!(c.totsuka.timezone, "Asia/Tokyo"); // default applied
        assert_eq!(c.bus.batch_size, 16); // default applied
        assert_eq!(c.github.columns.len(), 8);
        assert_eq!(
            c.github.columns.get(&ColumnId::Design).unwrap(),
            "🤖 調査・設計"
        );
        assert_eq!(c.agent_adapter.worktree_failed_ttl_hours, 72);
    }

    #[test]
    fn missing_required_field_errors() {
        let bad = MIN_TOML.replace(r#"queue_name="totsuka_events""#, "");
        assert!(Config::from_toml_str(&bad).is_err());
    }

    #[test]
    fn default_impl_verify_prompt_drives_autonomous_loop() {
        let p = default_impl_verify_prompt();
        // orchestrator::prompt::render が展開する 6 placeholder を全て含む
        for ph in [
            "{repo}",
            "{issue_number}",
            "{branch}",
            "{task_id}",
            "{project_owner}",
            "{project_number}",
        ] {
            assert!(p.contains(ph), "missing placeholder {ph}");
        }
        // issue はコメント込み・時系列で読む
        assert!(p.contains("--comments"));
        // PR↔task linkage の保険 (watcher の body トレーラー解決)
        assert!(p.contains("Closes #{issue_number}"));
        assert!(p.contains("Totsuka-Task: {task_id}"));
        // CI 監視ループ
        assert!(p.contains("gh pr checks --watch"));
        // 完了時はエージェント自身がカードを最終レビューへ移動、マージは禁止
        assert!(p.contains("🚧 最終レビュー"));
        assert!(p.contains("gh project item-edit"));
        assert!(p.contains("マージは行わない"));
        // エスカレーションは PR コメント報告 (カードは動かさない)
        assert!(p.contains("PR コメント"));
    }
}
