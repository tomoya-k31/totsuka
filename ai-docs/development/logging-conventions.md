---
type: Convention
title: ログ規約（JSON Lines・機密マスキング）
description: totsuka の構造化ログ規約。JSON Lines 1行1イベント、task_id 相関、機密マスキング（フィールド denylist＋値パターン）、log_prompts、日次ローテーションと世代保持。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core/src/logging
tags: [logging, tracing, security, convention]
generated: { by: human:tomoya-k31, at: 2026-07-12T00:40:00Z }
status: stable
owner: tomoya-k31
---

# 出力形式

- ファイル: `$XDG_STATE_HOME/totsuka/logs/totsuka.log.YYYY-MM-DD`（`tracing-appender` 日次ローテーション）。**JSON Lines**（1 行 = 1 イベント = 1 JSON オブジェクト、`jq` でパース可能）。
- ターミナル: 人間可読の 1 行形式。`NO_COLOR` と 非 TTY を尊重（§7）。
- 各行のキー: `timestamp`（ISO 8601 UTC）/ `level`（ERROR..TRACE）/ `target` / 任意 `message` / イベントフィールド。

# task_id 相関

タスクに紐づくイベントは **`task_id` フィールド**を付与する。`logs --task <id>`（#64）はこのフィールドで絞り込む。

# 機密マスキング（§5.2・必須）

型レベルの [`SecretString`](/components/orchestrator-core.md) に加え、logging レイヤで**無条件に** redact する最終防衛線を持つ。

- **フィールド denylist**: 名前が秘匿的（`api_key` / `authorization` / `*_token` / `*_secret` / `password` 等、大文字小文字問わず）なら値全体を `***` に置換。`max_tokens` / `token_count` 等の観測用数値は誤爆しないよう除外。
- **値パターン**: `Bearer <token>` → `Bearer ***`、`ghp_…` / `github_pat_…` / `sk-…` / `xoxb-…` / `secret_…` / `AKIA…` 等のトークン形状は任意フィールド内でも `***` に置換。

# プロンプト / RPC ペイロード

- `prompt` / `payload` / `rpc_payload` / `request_body` / `response_body` フィールドは **debug 以上でのみ**出力し、`[log] log_prompts = false` で完全に抑止できる（false のとき当該フィールドは行に出力されない）。

# レベルとローテーション

- レベル: `error` / `warn` / `info` / `debug` / `trace`。`[log] level` または `--debug`（#64）で調整。
- 日次ローテーション＋世代保持: `[log] max_files`（既定 7）を超える古い日次ファイルを起動時に削除。

# 設定（`[log]`）

```toml
[log]
level = "info"        # 省略時 info（--debug で debug）
log_prompts = false   # プロンプト本文をログに出さない
max_files = 7         # 日次ログの保持世代数
```

# 関連

- [orchestrator-core](/components/orchestrator-core.md)（`logging` モジュール）
- [Spec §5.2 ログ / §5.4 セキュリティ](/product/orchestrator-spec.ja.md)
