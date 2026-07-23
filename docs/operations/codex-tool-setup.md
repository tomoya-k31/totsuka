---
type: Runbook
title: Codex ツールのセットアップと hooks trust 運用
description: リポジトリ/ワークフローを Codex CLI で動かすための一回きりのセットアップ手順（インストール確認・config 設定・hooks trust・対象リポジトリの trust）と、trust が壊れた場合の復旧手順。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core/src/hooks
tags: [operations, runbook, codex, tool, hooks, trust, doctor]
timestamp: 2026-07-24T10:00:00Z
status: active
owner: tomoya-k31
---

# 概要

`[tools]` レジストリ（#196 / [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）で
`kind = "codex"` のツールをリポジトリやワークフローに割り当てると、pane 内で
Claude Code の代わりに OpenAI Codex CLI が起動する。完了検知は Claude と同じ
UDS フック契約（[POST /agent-events](/apis/agent-events.md)）で、Codex 側は
`$CODEX_HOME/hooks.json`（既定 `~/.codex/hooks.json`）への**グローバル登録** +
`TOTSUKA_JOB_ID` env ゲートで実現する（`totsuka run` / `totsuka doctor` が
自動同期。env の無い個人セッションではフックが即 exit 0 する）。

検証済みバージョン: **codex-cli 0.145.0**（2026-07-24 実機スパイク）。
確認済み: Stop hook の `{"decision":"block"}` / exit 2 ブロック（R-03）、
Stop stdin の `last_assistant_message` / `turn_id`、UserPromptSubmit
`additionalContext` の不可視注入、フックへの env 継承、`codex resume <id>`。

既知の縮退（`ToolCapabilities`）:

- plan permission mode が存在しない → plan モードは `--sandbox read-only` で代替
- heartbeat 相当のイベントが無い → 長時間タスクは workflow `timeout_secs` を
  長めに設定する（中間シグナルが無いままタイムアウト・エスカレーションしうる）
- prompt 型 Stop フックが無い → `verification = "llm"` は claude ピン推奨
  （validate が警告。codex のままだと検収は事実上素通しになる）
- SessionEnd フックは codex 側で timeout 3s にクランプされる（POST は通常
  ミリ秒だが、受信側ハング時は spool される前に kill されうる）

# セットアップ手順

1. **codex CLI インストール + サインイン**: `codex --version` が通ること。
2. **config.toml にツールを割り当て**（組み込み `codex` があるため `[tools]`
   セクションは不要。コマンド上書き時のみ定義）:

   ```toml
   [[repositories]]
   name = "my-repo"
   path = "~/src/my-repo"
   tool = "codex"
   ```

3. **登録同期**: `totsuka doctor` を実行。config が codex-kind ツールを参照して
   いれば `$CODEX_HOME/hooks.json` に totsuka 管理エントリ
   （Stop / SessionStart / SessionEnd / UserPromptSubmit / PermissionRequest）が
   追記される。自前で登録済みの hooks エントリは位置ごと保全される。
4. **hooks trust（一回きり・対話必須）**: `codex` を一度 TUI で起動すると
   起動時フックレビューが出るので **"Trust all and continue"** を選ぶ。
   - 未 trust のフックは codex が**サイレントにスキップ**する（エラーも警告も
     出ない）。その状態で dispatch すると完了通知が一切届かず全タスクが
     timeout エスカレーション行きになる。`totsuka doctor` の `codex-hooks`
     チェックが未 trust を警告するので、必ず green にしてから使う。
   - trust は hooks.json の**エントリ（コマンドパス・timeout 等）単位**で
     `$CODEX_HOME/config.toml` の `[hooks.state]` に永続化される。totsuka の
     スクリプト**本体**の更新（バージョンアップ）では再 trust 不要。エントリ
     自体が変わったとき（パス変更・同一イベント内での並び順変化）のみ再 trust。
5. **対象リポジトリの trust**: 各対象リポジトリで一度 `codex` を起動し
   フォルダ trust を済ませる。totsuka の作業 worktree は codex が
   **メインリポジトリの root に解決して trust 判定する**ため、リポジトリ本体を
   一度 trust すれば worktree ごとの再確認は出ない。

# トラブルシュート

- **タスクが常に timeout する / 完了イベントが届かない** → まず
  `totsuka doctor` の `codex-hooks` チェック。未 trust 警告が出ていれば手順 4。
- **`codex-hooks: hooks.json is inconsistent`** → 手編集で totsuka 管理エントリが
  壊れた可能性。doctor が自己修復（再同期）を試みる。パース不能な JSON は設計上
  絶対に上書きしないので、その場合は JSON を手で直してから再実行する。
- **再 trust を繰り返し要求される** → 同一イベント配列内でエントリの並び順が
  変わると codex の trust キー（index ベース）が無効化される（codex 側仕様）。
  自前フックの追加・削除の後は再度手順 4 を行う。
- フック全般の切り分け（spool・escalation・verify）は
  [フック完了判定のトラブルシューティング](/operations/hook-troubleshooting.md) を参照。
