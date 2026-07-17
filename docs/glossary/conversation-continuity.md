---
type: Term
title: 会話継続（conversation continuity）
description: 同一 Slack スレッドへの追いメンションを新タスクとして取り込みつつ、先行タスクの Claude セッションを claude --resume で再開して文脈を引き継ぐ仕組み。thread_key で永続相関する。
tags: [glossary, domain, slack, hook]
timestamp: 2026-07-18T00:00:00Z
status: active
owner: tomoya-k31
---

# 会話継続（conversation continuity）

同一 Slack スレッドへの追いメンションを **新しい [Task](/glossary/task.md)** として取り込みつつ、そのスレッドの先行タスクが使っていた Claude セッションを `claude --resume` で再開し、前回までの文脈を引き継ぐ仕組み（#140 / エピック #131 の設計判断 D-10）。ベストエフォートであり、条件が揃わなければ警告なしで通常の新規 [dispatch](/glossary/dispatch.md) にフォールバックする（ハードフェイルしない）。

## 相関キー: thread_key

会話は `Task.thread_key`（Slack では `{channel}:{thread_ts}`）で永続相関する。[task-source-slack](/components/task-source-slack.md) は全タスクに thread_key を付与する（スレッド内メンションはスレッド代表 ts を共有し、トップレベルメンションは自身の ts が根 = 新規会話）。相関インデックスはプラグインではなく **orchestrator-core の状態 DB** が持つ（再起動を跨いで保証するため）。タスク ID（`{channel}:{ts}`）は追いメンションごとに一意なので、既存の `(source, source_task_id)` dedup は無改修で新規タスクとして ingest する。

## resume 判定（core / dispatch_one）

`dispatch_one` はリトライ再利用の直前で次を **すべて** 満たすときだけ resume する:

1. タスクが thread_key を持つ
2. 同一 workflow・同一 thread_key の**先行**タスクが存在する（自分は除外。E-09: workflow も一致条件に含め別 workflow の同名スレッド誤紐付けを防止）
3. その先行タスクの最新セッションに**空でない** `claude_session_id` が確立している（フックの SessionStart 由来、#138）
4. エージェントが `resume_session` capability を宣言している

満たすと `task/dispatch(resume_session_id)` に先行セッション ID を渡す。worktree は通常フローで新規作成される（**破棄済みなら再作成 = セッションだけ使い回す**）。新タスクには新しい `sessions` 行と新 `job_id` が発番され、`claude_session_id` は resume 依頼値を仮置きしてフックの SessionStart で実値と突き合わせる。先行タスクの行・状態は変更しない（監査ログ上「別タスクだが会話は継続」がそのまま残る）。

## E-09（誤スレッド送信防止）

返信の宛先は常に**新タスク自身の** `source_task_id`（`{channel}:{ts}`）である。シグナル → タスクの相関は `job_id`（task_id を内包）起点で解決され、`claude_session_id` から宛先を推測する経路は存在しない。したがってセッションを共有していても、返信が先行タスクのスレッドへ誤配送されることは構造的に起こらない。
