---
type: Component
title: task-source-notion プラグイン
description: Notion データベースをタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。プロパティマッピングで任意の DB 構造を Task へ正規化し、ステータス書き戻しとページ本文への結果追記を行う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-notion
tags: [rust, crate, plugin, task-source, notion, rest, property-mapping]
timestamp: 2026-07-20T06:30:00Z
status: active
owner: tomoya-k31
---

# 責務

Notion データベースを totsuka のタスクソースとして接続する公式プラグイン（F-02/F-03）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。[task-source-github](/components/task-source-github.md) と同じ構造を Notion REST API へ適用したもの。#189（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md) Phase B）で protocol 0.1.6 の **push 型**へ移行 — [plugin-sdk](/components/plugin-sdk.md) の `poll_loop` が `initialize` 供給の triggers を内部 cadence（`poll_interval_secs`、既定 60s）で fetch し、各タスクを `task/submit` で push する。orchestrator 側のポーリングは行われない。

トークンは `initialize` の config で解決済みのものを受領し（F-65）、プラグイン自身は Keychain に触れない。JSON-RPC は stdout、診断ログは stderr（ホストがログへ転送）。GitHub と異なり、任意の DB 構造を扱うため **プロパティマッピング**（F-03）を設定で受け取り、共通 [`Task`](/components/plugin-protocol.md) スキーマ（F-01）へ正規化する。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/notion.toml`（= `InitializeParams.config`）を型付け。`token` / `database_id` / `notion_user_id`（F-08 の自己判定）/ `property_map`（title / status(+`status_kind` status\|select) / assignee / priority / repo_hint / body ↔ Notion プロパティ名, F-03）/ `body_source`（none\|property\|page）/ `in_progress_statuses` / `status_map`（orchestrator status→Notion option）/ `priority_map`（option 名→数値）/ `source_name` / `api_url` / `api_version` / `max_retries` / `rate_limit_rps`。`deny_unknown_fields` |
| `transport` | `NotionTransport` trait（`request(method, path, body, idempotent)`）＋ reqwest 実装 `ReqwestTransport`（bearer 認証・`Notion-Version` ヘッダ固定・タイムアウト・指数バックオフ §5.3・3rps スロットリング）。ロジックを録画レスポンスでテストするための seam |
| `blocks` | Notion ブロック ↔ Markdown 変換。読み（`blocks_to_markdown`, ページ本文→body）は主要ブロック型（heading/paragraph/bullet/numbered/to_do/quote/code）対応・未対応型はプレーンテキスト化。書き（`markdown_to_blocks`, F-07）は heading/bullet/quote/paragraph を生成し、2000 文字/リッチテキストの上限で分割（マルチバイト境界安全） |
| `client` | `NotionClient<T: NotionTransport>`。`fetch`（databases query をページング取得→property_map で `Task` 正規化→トリガー絞り込み→取り込み制御 F-08。body=page 時のみ生存タスクのブロックを取得）/ `update_status`（DB スキーマから option を検証、未知 option はエラー→ページ property を PATCH, F-84）/ `publish`（Markdown→blocks 変換、100 件バッチで追記, F-07）/ `validate`（users/me 疎通＋マップ先プロパティ存在確認 F-59） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。`Server::new(factory, SubmitClient)`（#189: SDK の stdio ランタイム[単一 writer タスク]で駆動され、`LineHandler` 実装経由で serve される）。initialize（config 型付け → client 構築 → triggers があれば SDK `poll_loop` を常駐 spawn — 各 tick で全 trigger を fetch し `task/submit` push。triggers 空なら poll なし。`poll_interval_secs = 0` は既定 60s へフォールバック[warn ログ]）/ config·validate / tasks·fetch（**deprecated の薄い delegate** — 同じ fetch 経路に委譲。`task_submit` 宣言により orchestrator は呼ばない。0.2.0 で削除予定）/ task·update_status / result·publish / shutdown。未初期化メソッドは拒否。Session drop（re-initialize 含む）で poll タスクを abort。`TransportFactory` で録画トランスポートを注入しテスト |
| `main` | SDK stdio ランタイム（`plugin_sdk::runtime::stdio` + `serve`）。`ReqwestFactory` を配線。ログは stderr |

# プロパティマッピング（F-03）

`property_map` で共通スキーマの各フィールド ↔ Notion プロパティ名を対応づける。`title` のみ必須、他は任意（未設定フィールドは抽出しない）。status は `status`/`select` 型の双方に対応（`status_kind` で write-back の本体形状と option 解決を切替）。priority は `number` プロパティを直接、または `select`/`status` の option 名を `priority_map` で数値化。body は property（`rich_text`）またはページ本文ブロック（`body_source`）から取得。これにより単一プラグインで任意の DB 構造を正規化できる。

# 取り込み制御（F-08）

fetch（`poll_loop` の各 tick と deprecated `tasks/fetch` で共通）は、まずトリガー（`status` / raw `filter`）で候補を絞り（可能なら databases query の server-side filter で削減）、次に多人数運用ゲーティングを適用する: assignee（people プロパティ）が他者のタスクを除外（自分は `notion_user_id` で判定、未設定時は未 assign のみ取り込み）、`in_progress_statuses` のステータスを除外。厳密な排他制御はしない。重複 push は orchestrator が `duplicate` ack で安価に破棄するため、プラグイン側に seen-set は持たない。

# capabilities（F-83）

manifest（`plugins/task-source-notion/plugin.toml`、`protocol_version = ">=0.1.6, <0.3"`）と `initialize` 応答で `kind = task_source`・`task_submit = true`・`outputs = ["source"]` を宣言。`result/publish` に対応する。

# テスト

`NotionTransport` を録画レスポンスの fake に差し替え、initialize→poll_loop→`task/submit` push（SubmitHarness で観測・ack 注入）、deprecated `tasks/fetch` delegate、property_map 正規化→ページ本文取得→update_status→result/publish の全経路を JSON-RPC 境界越しに結合テスト（`tests/integration.rs`）。取り込み制御（他者 assignee / 実行中 / トリガー不一致）、triggers 空での no-poll、2000 文字超の publish 分割、未知 option の update_status 拒否、トークン無効／マップ先プロパティ欠落時の `config/validate`（原因＋次アクション）を検証。実バイナリを stdio で駆動して疎通確認済み。

# 依存

- `plugin-protocol`（プラグイン境界）、[plugin-sdk](/components/plugin-sdk.md)（stdio ランタイム / `poll_loop` / `SubmitClient`）、`reqwest`（REST）、`tokio`、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。GitHub プラグインと同一の依存集合。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [plugin-sdk](/components/plugin-sdk.md)
- [task-source-github](/components/task-source-github.md)
- [ADR-0008 task/submit push 取り込み](/decisions/adr-0008-task-submit-push-ingestion.md)
- [Spec §4.2 タスクソース / F-01・F-03・F-07・F-08・F-84](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
