---
type: Runbook
title: OpenCode ツールのセットアップと運用
description: リポジトリ/ワークフローを OpenCode で動かすためのセットアップ（インストール確認・config 設定・アセット自動配置）と、Codex/Claude と異なる縮退（block 不可・指示が可視・llm 検収不可）の運用上の注意。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core/src/hooks
tags: [operations, runbook, opencode, tool, plugin, doctor]
generated: { by: human:tomoya-k31, at: 2026-07-24T12:00:00Z }
status: stable
owner: tomoya-k31
---

# 概要

`[tools]` レジストリ（#196 / [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）で `kind = "opencode"` のツールを割り当てると、
pane 内で OpenCode（TUI）が起動する。完了検知は同一の UDS フック契約
（[POST /agent-events](/apis/agent-events.md)）で、OpenCode 側は `$XDG_CONFIG_HOME/opencode/plugins/` へ
自動配置される totsuka の **JS プラグイン**（`totsuka-opencode.js`）が担う。
plan モードは自動配置される **totsuka-plan エージェント**（`--agent totsuka-plan`、
edit/bash/task 全 deny）で実現する。

検証済みバージョン: **opencode 1.14.39**（2026-07-24 実機スパイク）。
確認済み: `-s <session_id>` resume（文脈込み）、`session.status`/`session.idle`
イベント、`client.session.messages` での最終メッセージ取得、プラグインからの
UDS POST（Bun fetch `unix:`）、permission 全 deny の plan agent。

Codex と違い **trust 手順は不要**（opencode は plugins/ 配下を無条件に読み込む。
そのぶんディレクトリ自体がセキュリティ境界なので、書き込み権限の管理に注意）。
プラグインは `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_JOB_ID` が無い個人セッションでは
フックを一切登録しない。

# セットアップ手順

1. **opencode インストール + サインイン**: `opencode --version` が通ること。
   一度起動して `~/.config/opencode/` が存在すること。
2. **config.toml にツールを割り当て**（組み込み `opencode` があるため `[tools]`
   セクションは不要）:

   ```toml
   [[repositories]]
   name = "my-repo"
   path = "~/src/my-repo"
   tool = "opencode"
   ```

3. **アセット配置**: `totsuka doctor` を実行。`plugins/totsuka-opencode.js` と
   `agents/totsuka-plan.md` が自動配置される（SHA 冪等・改竄検出つき）。
   `opencode-assets` チェックが green ならセットアップ完了。

# 既知の縮退と運用上の注意（ToolCapabilities）

- **marker_block なし**: マーカー無し停止をその場でブロック・再依頼できない。
  UNKNOWN が連続すると engine の streak（既定 3）でエスカレーションする。
  マーカー規約は可視の指示文として毎回渡るため、通常は初回から付く。
- **invisible_injection なし**: タスク指示 + マーカー規約は**可視の
  extra_context** として pane に表示される（Claude/Codex の不可視注入と異なる）。
- **prompt 型検収なし**: `verification = "llm"` は不可（validate が警告）。
  human か none を使う。
- **heartbeat なし**: 長時間タスクは workflow `timeout_secs` を長めに。
- `opencode run`（非対話モード）には既知の不安定 issue があるため、totsuka の
  pane は TUI 起動のみを使う。
- OpenCode プロジェクトは anomalyco/opencode へ移管が進行中。将来の
  イベント形変更（`session.idle` の廃止完了等）に注意 — プラグインは
  `session.status`/`session.idle` 両対応で吸収している。

# トラブルシュート

- **タスクが常に timeout する** → `totsuka doctor` の `opencode-assets`。
  アセット欠落/改竄なら自動修復される。プラグイン読込は opencode の再起動後に
  有効になる点に注意（起動中の pane には反映されない）。
- **plan タスクでファイルが編集される** → `agents/totsuka-plan.md` が改竄されて
  いないか doctor で確認（edit/bash/task の全 deny が必須。permission だけの
  部分 deny ではサブエージェント委譲で貫通する — 実機確認済みの罠）。
