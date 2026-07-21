---
type: Guide
title: プラグイン開発ガイド
description: totsuka プラグインの作り方。plugin-protocol クレートの型、JSON-RPC(NDJSON/stdio) メソッド、plugin.toml マニフェスト、capability 宣言、ビルド手順（Cargo バイナリ名と plugin.toml の name 不一致時の対処）、install/enable の流れ、参照実装。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-protocol
tags: [plugin, protocol, json-rpc, manifest, guide]
timestamp: 2026-07-21T10:00:00Z
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
protocol_version = ">=0.2.0, <0.3"  # 対応する Orchestrator プロトコル範囲(F-54)

[capabilities]                  # 実際に対応する機能だけ宣言(F-33)
plan_mode = true                # agent: plan モード対応
design_preview = false          # agent: 設計プレビュー(F-34)
state_stream = true             # agent: state/subscribe ストリーム(F-38)
outputs = ["source"]            # task_source: result/publish 対応(F-83)
task_submit = true              # task_source: push 型ソース宣言（必須。[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）
```

Orchestrator は起動前に `protocol_version` の互換性を検査し（F-54）、宣言された capability のみ要求する。**プロトコル 0.2.0 以降、task_source は push（`task_submit = true`）専用**（`tasks/fetch` は削除済み）。`^0.1` を宣言する manifest は 0.2.0 の Orchestrator に起動拒否される — 0.1 系を引き続きサポートする場合は `>=0.1.6, <0.3` のように 0.2 をまたぐ範囲で宣言する（0.1.6 より前は `task_submit` capability 自体が無いので、その場合は `push` 対応版を新たにリリースしてから範囲を広げること）。

# メソッド（§11 付録 A）

**O→P** = Orchestrator→Plugin 呼び出し、**P→O** = Plugin→Orchestrator 通知。

## 共通

| メソッド | 方向 | 内容 |
|---|---|---|
| `initialize` | O→P | 解決済み config + プロトコル版を渡す。plugin_version + capabilities を返す（F-65）。**task_source には orchestrator の `[[repositories]]` も `repositories: [{name, summary?, path?}]` として供給される**（0.1.1、#109。任意フィールド — 使わなければ無視してよい。ソース側でリポジトリ解決するプラグインは自前設定の重複を省ける）。**同じく orchestrator の `[llm]` も `llm: {base_url, model, api_key?}` として供給される**（0.1.2、#119。api_key は解決済み。プラグイン自身の LLM 設定があればそちらを優先する default + override を推奨）。**task_source には `triggers: [{workflow, trigger}]`（`[[workflows]]` 定義順）と `poll_interval_secs: Option<u64>` も供給される**（0.1.6、[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)。監視条件と自プラグイン内部の fetch 周期。イベント駆動ソースは `poll_interval_secs` を無視してよい） |
| `config/validate` | O→P | プラグイン設定を検証（F-59） |
| `shutdown` | O→P | 猶予付き終了要求 |

## task_source

`task_source` は **push 専用**（プロトコル 0.2.0、[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）。タスクを見つけたら `task/submit` を Orchestrator へ**自分から**送る — Orchestrator がタスクを取りに来る RPC（旧 `tasks/fetch`）は存在しない。イベント駆動ソース（Webhook/Socket 等）は受信のたびに、ポーリングが自然なソース（GitHub/Notion 等）は `initialize` で受け取った `triggers`/`poll_interval_secs` で自前タイマーを回して、それぞれ `task/submit` を呼ぶ（[plugin-sdk](/components/plugin-sdk.md) の `poll_loop` がこのタイマー実装を提供する）。

| メソッド | 方向 | 内容 |
|---|---|---|
| `task/submit` | **P→O request** | プラグインが見つけたタスクを Orchestrator へ push（persist-before-ack）。応答は `accepted`（永続化）/ `duplicate`（冪等キー衝突、破棄してよい）/ `rejected`（恒久的に処理不能、reason 付き）のいずれかで**すべて最終**（同じタスクを reason で再送しない）。`NOT_ACCEPTING`/`SUBMIT_OVERLOADED`/`INTERNAL_ERROR` は再送可能（submit は冪等なのでバックオフ再送してよい） |
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

# ビルド

各プラグインはリポジトリルートの Cargo ワークスペースの通常メンバー（`plugins/{crate}/`）。ワークスペースルートから対象クレートを指定してビルドする。

```sh
cargo build --release -p task-source-github
```

生成物はクレート単体の `target/` ではなく、ワークスペース共有の `target/release/{Cargoパッケージ名}` に置かれる。

**注意: `totsuka plugin install <dir>` が探すバイナリ名は Cargo パッケージ名ではなく `plugin.toml` の `name` フィールドと一致していなければならない。** インストーラは `<dir>` 直下に `name` と**同名**のファイルが存在することを要求し（無ければ「plugin binary not found」で失敗）、`$XDG_DATA_HOME/totsuka/plugins/{name}/{name}` としてコピーする。実行時も同じ名前でプロセスを起動するため、Cargo のバイナリ名と `plugin.toml` の `name` が異なるプラグイン（例: `task-source-github` の Cargo バイナリ名に対し `plugin.toml` は `name = "github"`）では、install に渡す前にリネーム/コピーしてまとめる必要がある。

```sh
mkdir -p dist/github
cp target/release/task-source-github dist/github/github
cp plugins/task-source-github/plugin.toml dist/github/
totsuka plugin install ./dist/github
```

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
