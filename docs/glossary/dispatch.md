---
type: Term
title: dispatch（ディスパッチ）
description: キュー済みタスクをエージェントに割り当てる操作。スロット確保 → worktree 準備 → task/dispatch RPC → セッションID永続化までを指す。
tags: [glossary, domain]
timestamp: 2026-07-13T04:30:00Z
status: active
owner: tomoya-k31
---

# dispatch（ディスパッチ）

`queued` のタスクをエージェントへ割り当てる一連の操作。3階層の同時実行スロット（global / repo / agent、F-40〜43）を確保し、worktree を準備した上で agent_ide プラグインの `task/dispatch` を呼び、返ってきたセッション ID を永続化する（F-37）。タスクは `dispatched` 状態になり、以後は `state/notification` がステートマシンを進める。
