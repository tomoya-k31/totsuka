---
type: Runbook
title: 運用ガイド（doctor / worktree 掃除 / FAQ）
description: totsuka 日常運用の手引き。doctor の読み方、worktree 掃除ポリシーと孤児掃除、run 停止・回復、よくある問題の切り分け。
resource: https://github.com/tomoya-k31/totsuka
tags: [operations, doctor, worktree, faq, troubleshooting]
timestamp: 2026-07-25T09:00:00Z
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
| `worktree-location` | 明示した `[worktree].location` / `[[repositories]].worktree_location` が展開できる | `${ENV}` の未設定変数を export、またはキーを削って既定値（`$XDG_STATE_HOME/totsuka` 配下、未設定なら `$HOME/.local/state/totsuka`）に戻す。**worktree 作成はディスパッチ時**なので、これを放置すると run は正常起動したまま全タスクが失敗する |
| `plugin:{name}` | 起動 + `config/validate` 疎通 | install 済みか / `plugins/{name}.toml` を修正 |
| `llm` | `api_key_ref` が解決する | 環境変数 export / Keychain 登録 |
| `worktrees` | 孤児 worktree なし | 対話的に掃除を提案（TTY） |
| `panes` | 孤児 agent pane なし（#211） | 対話的に解放を提案（TTY）。`pane_control` 宣言 agent が無い構成では出ない |

`--json` 出力は不具合報告に添付する（Issue テンプレートが要求、§10.3）。

# worktree 掃除

「1 タスク = 1 worktree」の後始末は掃除ポリシーで決まる。

- `[worktree].cleanup`（implement 既定 `manual`）/ `plan_cleanup`（plan 既定 `immediate`）: `immediate` / `manual` / `{ retention_days = N }`
- **未コミット変更のある worktree は決して自動削除しない**（データ損失防止、F-23）
- `retention_days` は完了後 N 日で削除。`run` の各サイクルで再評価される
- どのタスクにも属さない **孤児 worktree** は `totsuka doctor` が検出し、TTY 上で対話的に `git worktree remove` を提案する（F-24）。dirty なものは skip

手動で消す場合は `git worktree remove <path>`（committed-but-unpushed があるなら `--force` は慎重に）。**手動削除では pane 解放の連動（#210）が働かない**ため、残った pane は次の `totsuka doctor` の孤児 pane チェックで回収する（下記）。

# 孤児 pane の掃除（#211）

worktree↔pane の連動（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）が破れると herdr に totsuka の pane だけが残る（手動 `git worktree remove`・解放拒否・クラッシュ・#210 以前の残骸）。`totsuka doctor` が worktree と対称の受け皿になる（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）:

- `pane_control` 宣言の agent プラグインに `session/list`（protocol 0.2.2）で自分の pane を列挙させ、DB と突き合わせる。候補 = 「対応するタスクが DB に無い」または「タスクが終端かつ worktree も既に無い」。実行中タスクや保持ポリシー（`keep_7d` 等）で worktree が残っているタスクの pane は候補にならない
- TTY では 1 件ずつ `[y/N]` で解放（`session/release`）を提案。`--json` / 非 TTY は検出のみ（`panes` チェックの FAIL）。**無人自動解放はしない**（孤児 worktree と同方針）
- herdr が落ちている等で列挙できないときは warning に留まり、他のチェックは続行する

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
