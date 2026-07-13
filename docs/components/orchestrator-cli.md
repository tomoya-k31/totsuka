---
type: Component
title: orchestrator-cli クレート
description: totsuka の CLI エントリポイント（bin: totsuka）。run（ワンショット / --watch / --dry-run）と plugin サブコマンドを提供し、残りのコマンド体系（§5.1）は #64 で実装する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [rust, crate, cli, plugin, run]
timestamp: 2026-07-13T03:30:00Z
status: active
owner: tomoya-k31
---

# 責務

ユーザー向けの CLI 表面。`clap` でコマンドを解釈し、[orchestrator-core](/components/orchestrator-core.md) のユースケースを呼び出す。

# 公開インターフェース

- bin 名: `totsuka`
- `plugin`（#52）: `install <dir> [--yes]` / `uninstall <name>` / `enable <name>` / `disable <name>` / `list [--json]`。install は取得元と SHA-256 を表示し確認を要求（§5.4）、GitHub Release からの取得は v1 未対応（ローカルディレクトリからの install に案内）。
- `run [--watch] [--dry-run]`（#63）: メインループの CLI 表面。設定ロード→`config::validate`（Error があれば起動拒否）→ログ初期化（§5.2）→単一インスタンスロック（F-74、dry-run は読み取り専用のため取得しない）→enabled プラグインを store から起動（`plugins/{name}.toml` のシークレット解決済み設定を `initialize` へ、F-58/64/65）→起動時回復（§5.3、再開不能タスクは `task retry/cancel` を案内）→孤児 worktree 警告（F-24）→[orchestrator-core の run Engine](/components/orchestrator-core.md) に委譲。終了時に summary（fetched/ingested/dispatched/done/failed と waiting/pending/queued の残タスク）を表示。SIGINT は graceful（実行中タスクは状態DBに残し次回回復）。
- 残りのサブコマンド体系（`status` / `task` / `config` / `doctor` / `logs` / `completion` と共通フラグ `--debug` / `--json` / `--config`）は #64 で実装する。

# 依存

- `clap`（derive）
- [orchestrator-core](/components/orchestrator-core.md)

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [Spec §5.1 起動・CLI](/product/orchestrator-spec.ja.md)
