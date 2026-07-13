---
type: Guide
title: プラグイン開発ガイド
description: totsuka プラグインの作り方。plugin-protocol クレートの型、JSON-RPC(NDJSON/stdio) メソッド、plugin.toml マニフェスト、capability 宣言、install/enable の流れ、参照実装。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-protocol
tags: [plugin, protocol, json-rpc, manifest, guide]
timestamp: 2026-07-14T03:00:00Z
status: active
owner: tomoya-k31
---

# 概要

プラグインは **stdio 上で JSON-RPC 2.0（1 行 1 メッセージ = NDJSON）を話す単一実行バイナリ**。3 種の kind がある: `task_source`（タスク供給）、`agent_ide`（エージェント駆動）、`notifier`（通知）。プロトコルの単一の正は [plugin-protocol クレート](/components/plugin-protocol.md)（型定義を公開）。

# 依存

```toml
[dependencies]
plugin-protocol = { git = "https://github.com/tomoya-k31/totsuka" }
```

`plugin_protocol` が提供する型（`Task`、`InitializeParams/Result`、各メソッドの params/result、`Manifest`、`Capabilities`、`jsonrpc` ヘルパ）を使う。プロトコル版は **アプリ本体と独立**（#50）。

# マニフェスト（plugin.toml）

各プラグインは `plugin.toml` を同梱する。

```toml
name = "github"                 # インスタンスバイナリ名と一致
kind = "task_source"            # task_source | agent_ide | notifier
version = "0.1.0"               # プラグイン自身の版
protocol_version = "^0.1"       # 対応する Orchestrator プロトコル範囲(F-54)

[capabilities]                  # 実際に対応する機能だけ宣言(F-33)
plan_mode = true                # agent: plan モード対応
design_preview = false          # agent: 設計プレビュー(F-34)
state_stream = true             # agent: state/subscribe ストリーム(F-38)
outputs = ["source"]            # task_source: result/publish 対応(F-83)
```

Orchestrator は起動前に `protocol_version` の互換性を検査し（F-54）、宣言された capability のみ要求する。

# メソッド（§11 付録 A）

**O→P** = Orchestrator→Plugin 呼び出し、**P→O** = Plugin→Orchestrator 通知。

## 共通

| メソッド | 方向 | 内容 |
|---|---|---|
| `initialize` | O→P | 解決済み config + プロトコル版を渡す。plugin_version + capabilities を返す（F-65） |
| `config/validate` | O→P | プラグイン設定を検証（F-59） |
| `shutdown` | O→P | 猶予付き終了要求 |

## task_source

| メソッド | 方向 | 内容 |
|---|---|---|
| `tasks/fetch` | O→P | trigger 条件でタスク取得 → 共通 `Task` スキーマ（F-01）で返す |
| `task/update_status` | O→P | ソース側ステータス遷移（F-84） |
| `result/publish` | O→P | 成果物をソースへ書き戻し（F-07） |

## agent_ide

| メソッド | 方向 | 内容 |
|---|---|---|
| `task/dispatch` | O→P | worktree 上で作業開始 → セッション ID を返す（F-31）。**責務はコミットまで。push/PR は行わない（F-86）** |
| `task/cancel` | O→P | 実行中タスクのキャンセル |
| `session/attach` | O→P | 既存セッションへ再接続（F-37）。attached + 現在状態を返す |
| `state/subscribe` | O→P | 状態/ログのストリーム購読（応答後に通知を流す） |
| `state/notification` | P→O | 状態変化 + ログ断片の通知（F-38）。`state` は `idle`/`running`/`waiting_input`/`done`/`failed` |

## notifier

| メソッド | 方向 | 内容 |
|---|---|---|
| `notify` | O→P（通知・応答不要） | イベント（`waiting_input`/`done`/`failed`/`pending`）配送（F-90）。**配送失敗はタスク実行に影響させない（F-93）** |

# 状態の対応（F-32）

エージェントの状態 `AgentState` は Orchestrator のステートマシンへ写像される（dispatched→running は `Start`、blocked は `waiting_input` でスロット解放、done は publishing へ）。プラグインは自分のツールの状態を 5 値へ正直に写像する。

# install / enable の流れ

- `totsuka plugin install <dir>`: `plugin.toml` + バイナリを含むディレクトリを検証（SHA-256 表示・確認）し `$XDG_DATA_HOME/totsuka/plugins/{name}/` へコピー（§5.4）
- `totsuka plugin enable {name}`: `config.toml` の `[plugins.{name}] enabled = true` を書き換え
- **install（バイナリの存在）と enable（設定の宣言）は分離**（F-56）

# 参照実装

- task_source: [task-source-github](/components/task-source-github.md)（GraphQL）、[task-source-notion](/components/task-source-notion.md)（REST + プロパティマッピング）
- agent_ide: [agent-ide-herdr](/components/agent-ide-herdr.md)（Socket API アダプタ）、[agent-ide-orca](/components/agent-ide-orca.md)（CLI ラップ）
- notifier: [notifier-macos](/components/notifier-macos.md)（osascript）
- 最小骨格: `crates/orchestrator-core/src/bin/mock_plugin.rs`（config 駆動で全 kind を演じるテスト用モック）

# 動作確認

`totsuka config validate`（online で `config/validate` を委譲）と `totsuka doctor`（ライブ疎通 probe）で自作プラグインの疎通を確認できる。
