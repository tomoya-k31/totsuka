---
type: Component
title: orchestrator-cli クレート
description: totsuka の CLI エントリポイント（bin: totsuka）。§5.1 のコマンド体系（init / run / status / task / plugin / config / logs / doctor / completion）と共通フラグ（--config / --debug / --json）を提供する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [rust, crate, cli, plugin, run, status, doctor]
timestamp: 2026-07-13T04:30:00Z
status: active
owner: tomoya-k31
---

# 責務

ユーザー向けの CLI 表面。`clap` でコマンドを解釈し、[orchestrator-core](/components/orchestrator-core.md) のユースケースを呼び出す。

# 公開インターフェース

- bin 名: `totsuka`
- `plugin`（#52）: `install <dir> [--yes]` / `uninstall <name>` / `enable <name>` / `disable <name>` / `list [--json]`。install は取得元と SHA-256 を表示し確認を要求（§5.4）、GitHub Release からの取得は v1 未対応（ローカルディレクトリからの install に案内）。
- `run [--watch] [--dry-run]`（#63）: メインループの CLI 表面。設定ロード→`config::validate`（Error があれば起動拒否）→ログ初期化（§5.2）→単一インスタンスロック（F-74、dry-run は読み取り専用のため取得しない）→enabled プラグインを store から起動（`plugins/{name}.toml` のシークレット解決済み設定を `initialize` へ、F-58/64/65）→起動時回復（§5.3、再開不能タスクは `task retry/cancel` を案内）→孤児 worktree 警告（F-24）→[orchestrator-core の run Engine](/components/orchestrator-core.md) に委譲。終了時に summary（fetched/ingested/dispatched/done/failed と waiting/pending/queued の残タスク）を表示。SIGINT は graceful（実行中タスクは状態DBに残し次回回復）。
- `init`（#64）: config.toml 雛形（コメントアウト済みテンプレート）と XDG ディレクトリの生成 + git バージョン確認。既存ファイルは決して上書きしない。
- `status [--json]`（#64）: タスク/worktree 一覧と orchestrator 生存表示。SQLite 直読でプラグインを起動しない（§5.5）。run.lock の PID 生存確認で「not running (stale lock)」を明示（F-74）。
- `task list|show|cancel|retry <id> [--json]`（#64）: `show` は状態・セッション履歴・worktree・イベント全履歴（`StateDb::list_events`）。`cancel`/`retry` は状態DBへのステートマシン遷移で、エージェントセッションとスロットは次回 `run` の回復/再利用（F-44）が引き受ける。retry は failed/cancelled のみ受け付ける。
- `config validate [--offline] / show [--redacted]`（#64）: validate はオフライン検証（schema/静的参照/ワークフロー意味論）+ `--offline` でなければ enabled プラグインを一時起動して `config/validate` を委譲（F-59/63）。show は config.toml と plugins/*.toml を表示し、`--redacted` で token/secret/password/api_key を含むキーの値をマスク。
- `logs [-f] [--task <id>]`（#64): JSON Lines ログ（§5.2）の整形表示・追尾（日次ローテーション追随）・タスク別フィルタ。
- `doctor [--json]`（#64）: git / config / state DB / プラグイン（インストール+ライブ疎通 probe）/ LLM キー解決 / 孤児 worktree（F-24、TTY では対話確認つき掃除提案）。失敗チェックは「原因 + 次のアクション」で報告し非ゼロ終了。
- `completion <shell>`: clap_complete によるシェル補完生成（zsh / bash / fish 等）。
- 共通フラグ: `--config <path>`（設定ファイル上書き = F-66 の最上位レイヤ）、`--debug`（run のログレベルを debug に引き上げ）。`--json` は全読み取り系コマンドに用意。
- UX 規約（§7）: エラーは「原因 + 次のアクション」（`→` 区切り）。用語は [glossary](/glossary/index.md) に準拠。

# 依存

- `clap`（derive）
- [orchestrator-core](/components/orchestrator-core.md)

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [Spec §5.1 起動・CLI](/product/orchestrator-spec.ja.md)
