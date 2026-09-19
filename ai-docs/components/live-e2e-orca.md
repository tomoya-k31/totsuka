---
type: Tool
title: live-e2e-orca スキル
description: 実 Slack / 実 GitHub / 実 orca + 実 Claude Code に対して totsuka を通しで動かす実機検証の手順と、orca 側の準備・観測スクリプト（orca 版）。GitHub / Slack の駆動・$E2E_HOME・サンドボックスは live-e2e-herdr のものを共用し、orca 固有の前提（repo 登録・external worktree 表示・プラグインの入れ直し・agent の切り替え）とシナリオ O1〜O6・症状表だけを持つ。
resource: https://github.com/tomoya-k31/totsuka/tree/main/.claude/skills/live-e2e-orca
tags: [testing, e2e, skill, tooling, orca, agent-ide]
generated: { by: claude-code/opus-5, at: 2026-09-19T05:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

[agent-ide-orca](/components/agent-ide-orca.md) を実機（実 orca + 実 Claude Code）で、Orchestrator を通して検収する。
CI は fake orca CLI までしか見ないので、orca 本体の挙動（`--json` の形・端末の起動と入力・タイトル・
external worktree の見え方）に依存する不具合はここでしか捕まらない。

[live-e2e-herdr](/components/live-e2e.md) の**エージェントだけを差し替えた**位置付けで、task source の経路
（GitHub / Slack）はエージェントに依存しないため、その駆動・観測スクリプト・`$E2E_HOME`・サンドボックスを共用する。
スクリプトを複製しないのは、同じ `tt` 定義・同じレート対策・同じ基準時刻の扱いが 2 か所で分岐するのを避けるため。

# 構成

| パス | 内容 |
|---|---|
| `SKILL.md` | 流れ。orca 固有の準備（`preflight` の 5 検査）、プラグインの入れ直し、再起動の依頼、代行できない操作と目視項目 |
| `references/scenarios.md` | O1 GitHub/implement・O2 pane control・O3 deadman・O4 cancel・O5 Slack と resume・O6 GUI。**未実施の範囲を明記**している |
| `references/troubleshooting.md` | orca 固有の症状表（プロンプトが届かない・タイトルの上書き・サイドバーに出ない・`ps` に出ない等） |
| `scripts/orca.sh` | `preflight` / `use <orca\|herdr>` / `sessions` / `inspect <task>` / `snapshot <task>` / `exit-agent <task>` / `cleanup-hints` |

`orca.sh` は `tt` の定義を herdr 版の `scripts/_common.sh` から読み、orca の `--json` envelope を
自前で読む（拒否も stdout に `ok: false` で出るため、終了コードだけでは判定しない）。

# 前提（`preflight` が検査する）

- orca ランタイムに届く
- サンドボックス repo 2 つが orca に登録済みで、`externalWorktreeVisibility = show`
  — 登録は人間の Orca に見えるプロジェクトを増やすので**承認を取ってから**行う
- インストール済みの orca プラグインがソースより新しい（`tt run` が起動するのはインストール済みのコピー）
- E2E 設定で `[plugins.orca]` が有効かつワークフローの `agent = "orca"`（`use orca` が隔離環境の設定だけを書き換える）、
  `[orca]` に廃止キーが無い、`[hooks].auth_token_ref` がある

# 検証状況

2026-09-19 に Orchestrator を通して O1〜O5 を実施し、合格した（詳細はスキルの `references/scenarios.md` の
「実機での実施状況」）。その過程で agent-ide-orca の不具合を 5 件見つけて直した — deadman の誤報、所有マーカーの
上書き、worktree の発見の遅れ、解放済み端末の空パス、`exit-agent` の設計。**未実施**は生きている端末への
`tt focus`、`tt doctor` の pane チェック、O6（GUI の見え方）。

# 関連

- [live-e2e-herdr スキル](/components/live-e2e.md)（共用元）
- [agent-ide-orca](/components/agent-ide-orca.md) / [ADR-0081](/decisions/adr-0081-orca-herdr-parity.md)
- [orca CLI 制御サーフェス](/references/orca-cli-control.md)
- [リリース前チェックリスト](/quality/release-checklist.md)
