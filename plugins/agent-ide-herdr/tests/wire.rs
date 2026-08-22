//! 生成した wire 型（`agent_ide_herdr::wire`）が、この設計が前提にしている
//! 3 つの読み方を実際にできることの検査（#518）。
//!
//! ここで確かめるのは形の再現ではなく、**設計上の主張**である:
//!
//! 1. 下限版（herdr 0.7.5）が必ず送るフィールドだけの応答が読める
//!    — 「下限版から生成すれば古い版は定義上読める」の実体
//! 2. **未知のフィールド**が増えても読める — 前方互換。`deny_unknown_fields`
//!    を付けないという判断がこれを支えている
//! 3. **未知の enum バリアント**が来ても読める — `#[serde(other)]` の実体。
//!    herdr は実測で 2 ヶ月に `EventKind` へ 3 個足している
//!
//! 2 と 3 が通らなくなったら、新しい herdr で totsuka が起動できなくなる
//! （#517 の大前提が壊れる）。1 が通らなくなったら、古い herdr のユーザを
//! 黙って切り捨てている。
//!
//! JSON は herdr 0.7.5 の実機応答を写したもので、**`required` だけに削って
//! ある**。実在のパスやセッション id は書かない（形だけが検査対象なので、
//! 実データである必要が無い）。

use agent_ide_herdr::wire::result::{
    AgentStatus, PaneInfoEnvelope, PaneListEnvelope, PaneReadEnvelope, PongEnvelope,
    WorkspaceCreatedEnvelope, WorkspaceListEnvelope,
};

/// `required` だけの `PaneInfo`。ここに足すたびに「下限版が必ず送るもの」の
/// 主張が強くなるので、schema の `required` と一致させること。
const MINIMAL_PANE: &str = r#"{
    "pane_id": "w1:p1",
    "terminal_id": "t1",
    "workspace_id": "w1",
    "tab_id": "w1:t1",
    "focused": true,
    "agent_status": "idle",
    "revision": 1
}"#;

fn pane_get(pane: &str) -> String {
    format!(r#"{{"type": "pane_info", "pane": {pane}}}"#)
}

#[test]
fn a_floor_version_response_carries_everything_the_types_require() {
    let env: PaneInfoEnvelope = serde_json::from_str(&pane_get(MINIMAL_PANE)).unwrap();
    assert_eq!(env.pane.pane_id, "w1:p1");
    assert_eq!(env.pane.agent_status, AgentStatus::Idle);
    // 任意フィールドは既定へ落ちる。読み手にとって「herdr が送らなかった」と
    // 「そんなフィールドは無い」は同じ意味でよい。
    assert!(env.pane.label.is_none());
    assert!(env.pane.tokens.is_empty());
}

#[test]
fn an_unknown_field_does_not_break_a_read() {
    // 新しい herdr が足したフィールド。`deny_unknown_fields` を付けていたら
    // ここで落ち、totsuka は新版で動かなくなる。
    let pane = MINIMAL_PANE.replace(
        "\"revision\": 1",
        "\"revision\": 1, \"something_herdr_added_later\": {\"nested\": [1, 2]}",
    );
    let env: PaneInfoEnvelope = serde_json::from_str(&pane_get(&pane)).unwrap();
    assert_eq!(env.pane.revision, 1);
}

#[test]
fn an_unknown_enum_variant_does_not_break_a_read() {
    let pane = MINIMAL_PANE.replace(
        "\"agent_status\": \"idle\"",
        "\"agent_status\": \"hibernating\"",
    );
    let env: PaneInfoEnvelope = serde_json::from_str(&pane_get(&pane)).unwrap();
    // 既知の値には化けない。呼び出し側が「知らない」と判断できることが要点で、
    // ここが `Idle` に潰れると状態機械が静かに間違った遷移をする。
    assert_eq!(env.pane.agent_status, AgentStatus::Unrecognized);
}

#[test]
fn the_envelopes_the_plugin_reads_all_parse() {
    let pong: PongEnvelope =
        serde_json::from_str(r#"{"type":"pong","version":"0.7.5","protocol":17}"#).unwrap();
    assert_eq!(pong.version, "0.7.5");

    let list: PaneListEnvelope = serde_json::from_str(&format!(
        r#"{{"type":"pane_list","panes":[{MINIMAL_PANE}]}}"#
    ))
    .unwrap();
    assert_eq!(list.panes.len(), 1);

    let ws: WorkspaceListEnvelope = serde_json::from_str(
        r#"{"type":"workspace_list","workspaces":[
             {"workspace_id":"w1","number":1,"label":"totsuka T1","focused":true,
              "pane_count":1,"tab_count":1,"active_tab_id":"w1:t1","agent_status":"idle"}]}"#,
    )
    .unwrap();
    assert_eq!(ws.workspaces[0].label, "totsuka T1");

    let created: WorkspaceCreatedEnvelope = serde_json::from_str(&format!(
        r#"{{"type":"workspace_created",
             "workspace":{{"workspace_id":"w1","number":1,"label":"totsuka T1","focused":true,
                "pane_count":1,"tab_count":1,"active_tab_id":"w1:t1","agent_status":"idle"}},
             "tab":{{"tab_id":"w1:t1","workspace_id":"w1","number":1,"label":"1","focused":true,
                "pane_count":1,"agent_status":"idle"}},
             "root_pane":{MINIMAL_PANE}}}"#
    ))
    .unwrap();
    assert_eq!(created.root_pane.pane_id, "w1:p1");

    let read: PaneReadEnvelope = serde_json::from_str(
        r#"{"type":"pane_read","read":{"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1",
             "source":"recent","format":"text","text":"$ ","revision":1,"truncated":false}}"#,
    )
    .unwrap();
    assert_eq!(read.read.text, "$ ");
}
