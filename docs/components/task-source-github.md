---
type: Component
title: task-source-github プラグイン
description: GitHub Issues / ProjectsV2 をタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。GraphQL で fetch→正規化、ProjectsV2 ステータス書き戻し、Issue コメント publish を行う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-github
tags: [rust, crate, plugin, task-source, github, graphql, projectsv2]
timestamp: 2026-07-13T00:00:00Z
status: active
owner: tomoya-k31
---

# 責務

GitHub Issues / ProjectsV2 を totsuka のタスクソースとして接続する公式プラグイン（F-02）。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。ワークスペース初の `plugins/` 配下クレート。

トークンは `initialize` の config で解決済みのものを受領し（F-65）、プラグイン自身は Keychain に触れない。JSON-RPC は stdout、診断ログは stderr（ホストがログへ転送）。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/github.toml`（= `InitializeParams.config`）を型付け。`token` / `owner`(+`owner_type` user\|org) / `project_number` / `status_field` / `github_login`（F-08 の自己判定）/ `in_progress_statuses` / `status_map`（orchestrator status→Project option）/ `repos` フィルタ / `source_name` / `api_url` / `max_retries`。`deny_unknown_fields` |
| `transport` | `GithubTransport` trait（`post_graphql`）＋ reqwest 実装 `ReqwestTransport`（bearer 認証・User-Agent 必須・タイムアウト・指数バックオフ §5.3）。ロジックを録画レスポンスでテストするための seam |
| `client` | `GithubClient<T: GithubTransport>`。`fetch`（ProjectsV2 items を GraphQL 取得→`Task` 正規化→トリガー絞り込み→取り込み制御 F-08）/ `update_status`（SingleSelect option を解決して mutation、未知 option はエラー F-84）/ `publish`（Issue コメント、長文は `<details>` 折りたたみ F-07）/ `validate`（viewer 疎通 F-59）。GraphQL は plain JSON で構築（GraphQL クレート不使用） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。initialize / config·validate / tasks·fetch / task·update_status / result·publish / shutdown。未初期化メソッドは拒否。`TransportFactory` で録画トランスポートを注入しテスト |
| `main` | `#[tokio::main]` の stdio ループ。`ReqwestFactory` を配線 |

# 取り込み制御（F-08）

`tasks/fetch` は、まずワークフローの trigger（`project_status` / `label`）で候補を絞り、次に多人数運用ゲーティングを適用する: assignee が他者のタスクを除外（自分は `github_login` で判定・大小無視）、`in_progress_statuses` のステータスを除外、`repos` フィルタ外を除外。厳密な排他制御はしない。

# capabilities（F-83）

manifest（`plugins/task-source-github/plugin.toml`）と `initialize` 応答で `kind = task_source`・`outputs = ["source"]` を宣言。`result/publish` に対応する。

# テスト

`GithubTransport` を録画レスポンスの fake に差し替え、initialize→fetch→正規化→update_status→result/publish の全経路を JSON-RPC 境界越しに結合テスト（`tests/integration.rs`）。取り込み制御（他者 assignee / 実行中）と、トークン無効時の `config/validate`（原因＋次アクション）も検証。実バイナリを stdio で駆動して疎通確認済み。

# 依存

- `plugin-protocol`（プラグイン境界）、`reqwest`（GraphQL）、`tokio`、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [Spec §4.2 タスクソース / F-02・F-04・F-07・F-08・F-84](/product/orchestrator-spec.ja.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
