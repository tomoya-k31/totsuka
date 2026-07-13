---
type: Runbook
title: 運用ガイド（doctor / worktree 掃除 / FAQ）
description: totsuka 日常運用の手引き。doctor の読み方、worktree 掃除ポリシーと孤児掃除、run 停止・回復、よくある問題の切り分け。
resource: https://github.com/tomoya-k31/totsuka
tags: [operations, doctor, worktree, faq, troubleshooting]
timestamp: 2026-07-14T03:00:00Z
status: active
owner: tomoya-k31
---

# doctor の読み方

`totsuka doctor`（`--json` で機械可読）は次を診断する。各失敗は「原因 + 次のアクション」を表示する（§7）。

| チェック | ok の意味 | FAIL 時の代表対応 |
|---|---|---|
| `git` | git が PATH 上にある | git を導入 |
| `config` | config.toml が検証を通る | `totsuka config validate` で全エラー確認 |
| `state-db` | 状態 DB が開ける | 一度 `totsuka run` |
| `plugin:{name}` | 起動 + `config/validate` 疎通 | install 済みか / `plugins/{name}.toml` を修正 |
| `llm` | `api_key_ref` が解決する | 環境変数 export / Keychain 登録 |
| `worktrees` | 孤児 worktree なし | 対話的に掃除を提案（TTY） |

`--json` 出力は不具合報告に添付する（Issue テンプレートが要求、§10.3）。

# worktree 掃除

「1 タスク = 1 worktree」の後始末は掃除ポリシーで決まる。

- `[worktree].cleanup`（implement 既定 `manual`）/ `plan_cleanup`（plan 既定 `immediate`）: `immediate` / `manual` / `{ retention_days = N }`
- **未コミット変更のある worktree は決して自動削除しない**（データ損失防止、F-23）
- `retention_days` は完了後 N 日で削除。`run` の各サイクルで再評価される
- どのタスクにも属さない **孤児 worktree** は `totsuka doctor` が検出し、TTY 上で対話的に `git worktree remove` を提案する（F-24）。dirty なものは skip

手動で消す場合は `git worktree remove <path>`（committed-but-unpushed があるなら `--force` は慎重に）。

# 停止・回復

- `run --watch` は SIGINT で graceful 停止。実行中タスクは状態 DB に残し、ロックを解放する（F-74）
- 異常終了（SIGKILL 含む）後の再起動は、状態 DB からセッション ID を復元し `session/attach` で再接続を試みる（§5.3）。再接続不能なタスクは **自動 failed にせず**「継続確認待ち」として残り、`totsuka task retry <id>` / `task cancel <id>` を人間が選ぶ
- `run` の多重起動は `$XDG_STATE_HOME/totsuka/run.lock` + PID で防止。`totsuka status` は run 停止中に stale を明示する

# タスク操作

- `totsuka status [--json]`: 実行中 / 待機（waiting_input・pending）タスクと worktree 一覧
- `totsuka task show <id>`: 状態・セッション履歴・worktree・イベント全履歴
- `totsuka task cancel <id>` / `retry <id>`: retry は failed/cancelled のみ。worktree/セッションを再利用して再開（F-44）
- `totsuka logs [-f] [--task <id>]`: JSON Lines ログの整形表示。機密は logging layer で無条件マスク（§5.2）

# FAQ / 切り分け

- **`config not found`**: `totsuka init` で雛形生成 → 編集
- **`state database not found`**: 一度 `totsuka run` すると作成される
- **プラグインが `enabled but not installed`**: `totsuka plugin install <dir>`
- **タスクが取り込まれない**: `totsuka run --dry-run` でトリガーマッチ・リポジトリ選択・エージェント割当を副作用ゼロで確認。ワークフローの `source` は `[plugins.{name}]` のインスタンス名と一致させる
- **リポジトリ選択が `pending`**: `[llm]` 未設定 or 確信度が低い。単一リポジトリなら自動選択、複数なら `[llm]` を設定するか `repo_hint` を付与
- **pull_request が「コミットゼロ」で失敗**: エージェントがコミットしていない（agent の責務はコミットまで、F-86）。retry で再開可能
- **通知が来ない**: `[plugins.{notifier}] enabled` と `notifier` プラグイン疎通を `doctor` で確認。配送失敗はタスク実行を止めない（F-93）

リリース前の実機確認は [リリース前手動チェックリスト](/quality/release-checklist.md) を参照。
