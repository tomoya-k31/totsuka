---
type: Term
title: Agent IDE（エージェントIDE）
description: コーディングエージェントを動かす実行環境（herdr / orca 等）。agent_ide プラグインが task/dispatch・session/attach・state/subscribe を実装して接続する。
tags: [glossary, plugin]
generated: { by: human:tomoya-k31, at: 2026-07-31T00:00:00Z }
status: stable
owner: tomoya-k31
---

# Agent IDE（エージェントIDE）

AI コーディングエージェントをセッションとして起動・監視できるツール（例: [agent-ide-herdr](/components/agent-ide-herdr.md)、[agent-ide-orca](/components/agent-ide-orca.md)）。`agent_ide` kind のプラグインが `task/dispatch`（worktree 上で作業開始、F-31）、`session/attach`（再起動後の再接続、F-37）、`state/subscribe` → `state/notification`（状態ストリーム、F-38）を実装する。worktree は detached HEAD で渡り、ブランチの命名・作成からコミット・push・PR 作成までがエージェントの責務（F-86、[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）。Orchestrator は push しない。
