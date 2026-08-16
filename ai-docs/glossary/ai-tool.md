---
type: Term
title: AI Tool（AI ツール）と 2 軸モデル
description: pane 内で起動する AI エージェント CLI（Claude Code / Codex / OpenCode）。pane を管理する agent プラグイン（herdr 等）とは直交する軸で、[tools] レジストリと tool フィールド（workflow > repo > default_tool > 組み込み claude）で選択される。
tags: [tool, agent, glossary, 2-axis]
generated: { by: human:tomoya-k31, at: 2026-07-24T12:00:00Z }
status: stable
owner: tomoya-k31
---

# 定義

**AI ツール（tool）** は herdr pane の中で起動するエージェント CLI そのもの
（Claude Code / OpenAI Codex CLI / OpenCode）。**agent プラグイン**（pane
runner: herdr / orca、`[[workflows]].agent`）とは直交する 2 軸であり、
[ADR-0014](/decisions/adr-0014-tool-abstraction.md) でこの分離が決定された
（旧 `default_agent` はこの 2 軸を混同していた名残で削除済み）。

- **agent 軸**: pane をどう管理するか（起動・attach・状態監視・focus）
- **tool 軸**: pane の中でどの CLI を走らせるか（argv 組立・完了検知アダプタ・
  ケイパビリティ縮退）

選択は `[[workflows]].tool` > `[[repositories]].tool` > `default_tool` >
組み込み `claude` の優先順位（[設定リファレンス](/development/config-reference.md)）。
ツール知識は orchestrator-core の `tool` モジュール（`ToolKind` /
`ToolCapabilities` / `ToolProfile::launch_spec`）に集約され、プラグインには
完全解決済みの `ToolLaunchSpec`（argv/env）だけが渡る。

# ケイパビリティ縮退

kind ごとの `ToolCapabilities`（不可視注入・marker block・prompt 検証・
resume・plan・heartbeat・session id 捕捉）が dispatch/engine の縮退を駆動する。
セットアップと kind 別の注意は
[Codex](/operations/codex-tool-setup.md) / [OpenCode](/operations/opencode-tool-setup.md)
の各 Runbook を参照。
