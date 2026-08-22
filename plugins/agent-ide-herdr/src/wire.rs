//! herdr Socket API の wire 型。**生成物 — 手で編集しない。**
//!
//! 生成元: `plugins/agent-ide-herdr/schemas/herdr-0.7.5.json`
//! （= herdr 0.7.5 の API schema を `schemas/methods.json` の 22 メソッドへ
//! スライスしたもの）。再生成は `bash scripts/herdr-types-build.sh`。
//!
//! # なぜ下限版から生成するのか
//!
//! 型は 1 組だけで、版ごとの分岐は作らない。古い版は生成元そのものなので
//! 定義上読める。新しい版は未知フィールド無視 + `#[serde(other)]` で読み、
//! **下限版の型で読めること自体を CI の schema 差分が検査する**（削除・
//! プロパティの型の入れ替え・`required` の向き・enum バリアントの削除を
//! 落とす。検出しないのは `pattern` / `maxProperties` のような制約の厳格化と、
//! schema に出ない振る舞いの変化）。
//!
//! # 実行時は寛容、CI は厳格
//!
//! `deny_unknown_fields` は**付けない**。前方互換はこの結合の無料の利点で、
//! 捨てる理由が無い。result 封筒は `type` タグを**検査しない**のも同じ判断で、
//! タグの改名を報せるのはコミット済み schema の差分（マージ前）であって、
//! 実行時の失敗ではない。

/// totsuka が herdr へ**送る**型。
///
/// `#[serde(other)]` はここには無い。知らない値を送り返すことになるうえ、
/// 送る側の未知バリアントは「herdr が知らない値を totsuka が作った」という
/// totsuka 自身のバグだからである。未設定の任意フィールドはキーごと落とす
/// （`skip_serializing_if`）— 明示的な `null` が「未指定」と同じ扱いを
/// されるとは限らない。
pub mod request {
    use serde::Serialize;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, Serialize)]
    pub struct AgentPromptParams {
        pub target: String,
        pub text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub wait: Option<AgentPromptWaitOptions>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct AgentPromptWaitOptions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub until: Vec<AgentStatus>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct AgentSendKeysParams {
        pub keys: Vec<String>,
        pub target: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct AgentStartParams {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub args: Vec<String>,
        pub kind: String,
        pub name: String,
        pub pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64>,
    }

    #[derive(Debug, Clone, Serialize, Copy, PartialEq, Eq)]
    pub enum AgentStatus {
        #[serde(rename = "idle")]
        Idle,
        #[serde(rename = "working")]
        Working,
        #[serde(rename = "blocked")]
        Blocked,
        #[serde(rename = "done")]
        Done,
        #[serde(rename = "unknown")]
        Unknown,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct AgentWaitParams {
        pub target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub timeout_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub until: Vec<AgentStatus>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct EmptyParams {}

    #[derive(Debug, Clone, Serialize)]
    pub struct EventsSubscribeParams {
        pub subscriptions: Vec<Subscription>,
    }

    #[derive(Debug, Clone, Serialize)]
    #[serde(tag = "type")]
    pub enum OutputMatch {
        #[serde(rename = "substring")]
        Substring { value: String },
        #[serde(rename = "regex")]
        Regex { value: String },
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneListParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneReadParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub format: Option<ReadFormat>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub lines: Option<u32>,
        pub pane_id: String,
        pub source: ReadSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub strip_ansi: Option<bool>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneRenameParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String>,
        pub pane_id: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneReportMetadataParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub agent: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub applies_to_source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub clear_display_agent: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub clear_state_labels: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub clear_title: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub display_agent: Option<String>,
        pub pane_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64>,
        pub source: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub state_labels: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub title: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub tokens: BTreeMap<String, Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ttl_ms: Option<u64>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneSendKeysParams {
        pub keys: Vec<String>,
        pub pane_id: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneSplitParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
        pub direction: SplitDirection,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub focus: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ratio: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target_pane_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workspace_id: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PaneTarget {
        pub pane_id: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct PingParams {}

    #[derive(Debug, Clone, Serialize, Copy, PartialEq, Eq)]
    pub enum ReadFormat {
        #[serde(rename = "text")]
        Text,
        #[serde(rename = "ansi")]
        Ansi,
    }

    #[derive(Debug, Clone, Serialize, Copy, PartialEq, Eq)]
    pub enum ReadSource {
        #[serde(rename = "visible")]
        Visible,
        #[serde(rename = "recent")]
        Recent,
        #[serde(rename = "recent_unwrapped")]
        RecentUnwrapped,
        #[serde(rename = "detection")]
        Detection,
    }

    #[derive(Debug, Clone, Serialize, Copy, PartialEq, Eq)]
    pub enum SplitDirection {
        #[serde(rename = "right")]
        Right,
        #[serde(rename = "down")]
        Down,
    }

    #[derive(Debug, Clone, Serialize)]
    #[serde(tag = "type")]
    pub enum Subscription {
        #[serde(rename = "workspace.created")]
        WorkspaceCreated,
        #[serde(rename = "workspace.updated")]
        WorkspaceUpdated,
        #[serde(rename = "workspace.metadata_updated")]
        WorkspaceMetadataUpdated,
        #[serde(rename = "workspace.renamed")]
        WorkspaceRenamed,
        #[serde(rename = "workspace.moved")]
        WorkspaceMoved,
        #[serde(rename = "workspace.closed")]
        WorkspaceClosed,
        #[serde(rename = "workspace.focused")]
        WorkspaceFocused,
        #[serde(rename = "worktree.created")]
        WorktreeCreated,
        #[serde(rename = "worktree.opened")]
        WorktreeOpened,
        #[serde(rename = "worktree.removed")]
        WorktreeRemoved,
        #[serde(rename = "tab.created")]
        TabCreated,
        #[serde(rename = "tab.closed")]
        TabClosed,
        #[serde(rename = "tab.focused")]
        TabFocused,
        #[serde(rename = "tab.renamed")]
        TabRenamed,
        #[serde(rename = "tab.moved")]
        TabMoved,
        #[serde(rename = "pane.created")]
        PaneCreated,
        #[serde(rename = "pane.closed")]
        PaneClosed,
        #[serde(rename = "pane.updated")]
        PaneUpdated,
        #[serde(rename = "pane.focused")]
        PaneFocused,
        #[serde(rename = "pane.moved")]
        PaneMoved,
        #[serde(rename = "pane.exited")]
        PaneExited,
        #[serde(rename = "pane.agent_detected")]
        PaneAgentDetected,
        #[serde(rename = "pane.output_matched")]
        PaneOutputMatched {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            lines: Option<u32>,
            r#match: OutputMatch,
            pane_id: String,
            source: ReadSource,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            strip_ansi: Option<bool>,
        },
        #[serde(rename = "pane.agent_status_changed")]
        PaneAgentStatusChanged {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            agent_status: Option<AgentStatus>,
            pane_id: String,
        },
        #[serde(rename = "pane.scroll_changed")]
        PaneScrollChanged { pane_id: String },
        #[serde(rename = "layout.updated")]
        LayoutUpdated,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct TabTarget {
        pub tab_id: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorkspaceCreateParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        pub env: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub focus: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorkspaceRenameParams {
        pub label: String,
        pub workspace_id: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorkspaceReportMetadataParams {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub seq: Option<u64>,
        pub source: String,
        pub tokens: BTreeMap<String, Option<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ttl_ms: Option<u64>,
        pub workspace_id: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorkspaceTarget {
        pub workspace_id: String,
    }
}

/// totsuka が herdr から**読む**型。
///
/// 封筒（`*Envelope`）は `result` オブジェクトそのものの形で、`type` タグの
/// フィールドは持たない（上記「実行時は寛容」）。
pub mod result {
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, Deserialize)]
    pub struct AgentInfo {
        #[serde(default)]
        pub agent: Option<String>,
        #[serde(default)]
        pub agent_session: Option<AgentSessionInfo>,
        pub agent_status: AgentStatus,
        #[serde(default)]
        pub cwd: Option<String>,
        #[serde(default)]
        pub display_agent: Option<String>,
        pub focused: bool,
        #[serde(default)]
        pub foreground_cwd: Option<String>,
        #[serde(default)]
        pub interactive_ready: Option<bool>,
        #[serde(default)]
        pub launch_pending: Option<bool>,
        #[serde(default)]
        pub name: Option<String>,
        pub pane_id: String,
        pub revision: u64,
        #[serde(default)]
        pub screen_detection_skipped: Option<bool>,
        #[serde(default)]
        pub state_change_seq: Option<u64>,
        #[serde(default)]
        pub state_labels: BTreeMap<String, String>,
        pub tab_id: String,
        pub terminal_id: String,
        #[serde(default)]
        pub terminal_title: Option<String>,
        #[serde(default)]
        pub terminal_title_stripped: Option<String>,
        #[serde(default)]
        pub title: Option<String>,
        #[serde(default)]
        pub tokens: BTreeMap<String, String>,
        pub workspace_id: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct AgentSessionInfo {
        pub agent: String,
        pub kind: AgentSessionRefKind,
        pub source: String,
        pub value: String,
    }

    #[derive(Debug, Clone, Deserialize, Copy, PartialEq, Eq)]
    pub enum AgentSessionRefKind {
        #[serde(rename = "id")]
        Id,
        #[serde(rename = "path")]
        Path,
        /// この生成が知らない値。herdr はリリースの合間にバリアントを足す
        /// （実測: 2 ヶ月で `EventKind` に 3 個）ので、読みは 1 個の追加で
        /// 落ちてはならない。追加を報せるのはコミット済み schema の差分で
        /// あって、デシリアライズの失敗ではない。
        #[serde(other)]
        Unrecognized,
    }

    #[derive(Debug, Clone, Deserialize, Copy, PartialEq, Eq)]
    pub enum AgentStatus {
        #[serde(rename = "idle")]
        Idle,
        #[serde(rename = "working")]
        Working,
        #[serde(rename = "blocked")]
        Blocked,
        #[serde(rename = "done")]
        Done,
        #[serde(rename = "unknown")]
        Unknown,
        /// この生成が知らない値。herdr はリリースの合間にバリアントを足す
        /// （実測: 2 ヶ月で `EventKind` に 3 個）ので、読みは 1 個の追加で
        /// 落ちてはならない。追加を報せるのはコミット済み schema の差分で
        /// あって、デシリアライズの失敗ではない。
        #[serde(other)]
        Unrecognized,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PaneInfo {
        #[serde(default)]
        pub agent: Option<String>,
        #[serde(default)]
        pub agent_session: Option<AgentSessionInfo>,
        pub agent_status: AgentStatus,
        #[serde(default)]
        pub cwd: Option<String>,
        #[serde(default)]
        pub display_agent: Option<String>,
        pub focused: bool,
        #[serde(default)]
        pub foreground_cwd: Option<String>,
        #[serde(default)]
        pub label: Option<String>,
        pub pane_id: String,
        pub revision: u64,
        #[serde(default)]
        pub scroll: Option<PaneScrollInfo>,
        #[serde(default)]
        pub state_labels: BTreeMap<String, String>,
        pub tab_id: String,
        pub terminal_id: String,
        #[serde(default)]
        pub terminal_title: Option<String>,
        #[serde(default)]
        pub terminal_title_stripped: Option<String>,
        #[serde(default)]
        pub title: Option<String>,
        #[serde(default)]
        pub tokens: BTreeMap<String, String>,
        pub workspace_id: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PaneReadResult {
        pub format: ReadFormat,
        pub pane_id: String,
        pub revision: u64,
        pub source: ReadSource,
        pub tab_id: String,
        pub text: String,
        pub truncated: bool,
        pub workspace_id: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PaneScrollInfo {
        pub max_offset_from_bottom: u64,
        pub offset_from_bottom: u64,
        pub viewport_rows: u64,
    }

    #[derive(Debug, Clone, Deserialize, Copy, PartialEq, Eq)]
    pub enum ReadFormat {
        #[serde(rename = "text")]
        Text,
        #[serde(rename = "ansi")]
        Ansi,
        /// この生成が知らない値。herdr はリリースの合間にバリアントを足す
        /// （実測: 2 ヶ月で `EventKind` に 3 個）ので、読みは 1 個の追加で
        /// 落ちてはならない。追加を報せるのはコミット済み schema の差分で
        /// あって、デシリアライズの失敗ではない。
        #[serde(other)]
        Unrecognized,
    }

    #[derive(Debug, Clone, Deserialize, Copy, PartialEq, Eq)]
    pub enum ReadSource {
        #[serde(rename = "visible")]
        Visible,
        #[serde(rename = "recent")]
        Recent,
        #[serde(rename = "recent_unwrapped")]
        RecentUnwrapped,
        #[serde(rename = "detection")]
        Detection,
        /// この生成が知らない値。herdr はリリースの合間にバリアントを足す
        /// （実測: 2 ヶ月で `EventKind` に 3 個）ので、読みは 1 個の追加で
        /// 落ちてはならない。追加を報せるのはコミット済み schema の差分で
        /// あって、デシリアライズの失敗ではない。
        #[serde(other)]
        Unrecognized,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct ServerCapabilities {
        #[serde(default)]
        pub detached_server_daemon: Option<bool>,
        pub live_handoff: bool,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct TabInfo {
        pub agent_status: AgentStatus,
        pub focused: bool,
        pub label: String,
        pub number: u64,
        pub pane_count: u64,
        pub tab_id: String,
        pub workspace_id: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct WorkspaceInfo {
        pub active_tab_id: String,
        pub agent_status: AgentStatus,
        pub focused: bool,
        pub label: String,
        pub number: u64,
        pub pane_count: u64,
        pub tab_count: u64,
        #[serde(default)]
        pub tokens: BTreeMap<String, String>,
        pub workspace_id: String,
        #[serde(default)]
        pub worktree: Option<WorkspaceWorktreeInfo>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct WorkspaceWorktreeInfo {
        pub checkout_path: String,
        pub is_linked_worktree: bool,
        pub repo_key: String,
        pub repo_name: String,
        pub repo_root: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct AgentStartedEnvelope {
        pub agent: AgentInfo,
        pub argv: Vec<String>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PaneInfoEnvelope {
        pub pane: PaneInfo,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PaneListEnvelope {
        pub panes: Vec<PaneInfo>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PaneReadEnvelope {
        pub read: PaneReadResult,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct PongEnvelope {
        #[serde(default)]
        pub capabilities: Option<ServerCapabilities>,
        pub protocol: u32,
        pub version: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct WorkspaceCreatedEnvelope {
        pub root_pane: PaneInfo,
        pub tab: TabInfo,
        pub workspace: WorkspaceInfo,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct WorkspaceListEnvelope {
        pub workspaces: Vec<WorkspaceInfo>,
    }
}
