---
type: Component
title: task-source-slack プラグイン
description: 自分宛の Slack メンションをタスク化し本人名義で代理返信するための公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。現在は骨格段階で、設定スキーマ・静的 config/validate・起動時 TokenGuard（auth.test + 本人一致検証）まで実装済み。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-slack
tags: [rust, crate, plugin, task-source, slack, socket-mode, token-guard]
timestamp: 2026-07-15T00:00:00Z
status: active
owner: tomoya-k31
---

# 責務

自分（`target_user_id`）宛の Slack メンションを検知して totsuka のタスクへ正規化し、承認後に **本人名義**（ユーザートークン `xoxp-`、Bot なし）でスレッド返信するための公式プラグイン。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。構造は [task-source-notion](/components/task-source-notion.md) / [task-source-github](/components/task-source-github.md) に準拠。orchestrator-core / plugin-protocol への変更なしで成立する（エピック #102 の設計判断）。

現在は **骨格段階**（#103）: crate 構成・設定スキーマ・stdio JSON-RPC ディスパッチ・TokenGuard までを実装し、`tasks/fetch`（空応答）・`task/update_status` / `result/publish`（受理して no-op）はスタブ。Slack Web API クライアント + Socket Mode WebSocket（#104）、メンション検知（#105）、リポジトリ解決（#106）、承認フロー（#107）が後続で載る。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/slack.toml`（= `InitializeParams.config`）を型付け。`app_token`（`xapp-`, Socket Mode 用）/ `user_token`（`xoxp-`, 本人名義）/ `target_user_id` / `thread_context_limit`（既定 6）/ `reply_style` / `source_name`（既定 `slack`）/ `[llm]`（リポジトリ選択用 OpenAI 互換 LLM: base_url / model / api_key / confidence_threshold 既定 0.6）/ `[[channel_groups]]`（prefix→候補 repos, 定義順 first-match）/ `[[repos]]`（候補リポジトリ: name / summary? / path?）/ `api_url`（テスト用上書き）/ `max_retries`。`deny_unknown_fields`。静的検証 `static_config_errors`: トークン prefix 形式、`[[repos]]` 非空・重複、repos 2 件以上での `[llm]` 必須、channel_groups→repos の参照整合、confidence_threshold 範囲 |
| `transport` | `SlackTransport` trait（`call(token_kind, method, body, idempotent)`）＋ reqwest 実装 `ReqwestTransport`（`TokenKind::App`/`User` で bearer を切替・タイムアウト・冪等呼び出しのみ指数バックオフ再試行）。`expect_ok` で Slack の `{"ok": bool}` エンベロープを解釈。録画レスポンスでテストするための seam |
| `error` | `SlackError`（Auth / IdentityMismatch / Api / Http / Transport / Timeout / InvalidResponse）。`is_retryable`（429・5xx・ネットワーク）と `is_credential`（TokenGuard 系）を分類。`auth_failure` が `invalid_auth` / `token_revoked` / `account_inactive` を原因別の再発行・再インストール手順付きメッセージへ写像 |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。initialize（TokenGuard 実行）/ config·validate（**静的検証のみ・ネットワーク不要**）/ shutdown / tasks·fetch / task·update_status / result·publish（スタブ）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]` の stdio ループ。`ReqwestFactory` を配線。ログは stderr |

# TokenGuard（起動時検証）

`initialize` 内で `auth.test`（user_token）を実行し、失敗時は原因別ガイダンス付きエラーで初期化を拒否する:

- `invalid_auth` / `token_revoked` / `account_inactive` → トークン再発行・アプリ再インストール・Keychain 更新手順を含むメッセージ（`CONFIG_INVALID`）
- 成功時は `user_id == target_user_id` を検証。不一致は **なりすまし防止** のためエラー（両 ID と修正手順を明示、`CONFIG_INVALID`）
- ネットワーク障害などクレデンシャル以外の失敗は `INTERNAL_ERROR` として区別

`config/validate` は意図的にオフライン（静的検証のみ）。ホストのプローブ（`totsuka config validate` / `doctor`）は launch ハンドシェイクで `initialize` も呼ぶため、TokenGuard はそこで実行される。

# capabilities（F-83）

manifest（`plugins/task-source-slack/plugin.toml`）と `initialize` 応答で `kind = task_source`・`outputs = ["source"]` を宣言（返信は `result/publish` でソース＝Slack スレッドへ書き戻す）。

# テスト

`SlackTransport` を録画レスポンスの fake に差し替え、JSON-RPC 境界越しに結合テスト（`tests/integration.rs`）: TokenGuard 成功→capabilities 宣言、auth 失敗 3 種の原因別ガイダンス、user_id 不一致拒否、ネットワーク障害の INTERNAL_ERROR 区別、オフライン config/validate（正常系はトランスポート無呼び出しを検証）、未初期化拒否、スタブ応答、shutdown・パースエラー・通知の各プロトコル配管。実バイナリでも `plugin install` / `enable` / `config validate`（正常＋未知キー・トークン形式・repos 不整合）/ `doctor` プローブ、およびローカル偽 Slack API と実 Slack API（invalid_auth）双方に対する TokenGuard を確認済み。

# 依存

- `plugin-protocol`（プラグイン境界）、`reqwest`（Web API）、`tokio`、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。既存 task_source プラグインと同一の依存集合（新規外部クレートなし。Socket Mode の WebSocket 依存は #104 で追加予定）。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [task-source-github](/components/task-source-github.md)
- [task-source-notion](/components/task-source-notion.md)
- [Spec §4.2 タスクソース / F-01・F-07・F-51・F-59・F-64・F-65](/product/orchestrator-spec.ja.md)
