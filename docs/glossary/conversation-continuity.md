---
type: Term
title: 会話継続（conversation continuity）
description: 1 スレッド = 1 会話を 1 タスクとして扱い、追いメンションを同じタスクへの追加メッセージとして取り込むことで worktree・ブランチ・エージェントセッションを共有する仕組み。#242 でタスク同一性そのものを会話単位に変えた。
tags: [glossary, domain, slack, hook, conversation-identity]
timestamp: 2026-07-26T00:00:00+09:00
status: active
owner: tomoya-k31
---

# 会話継続（conversation continuity）

同一 Slack スレッドへの追いメンションが、前回までの文脈を保ったまま処理される仕組み。**エピック #242 で実現方法が根本から変わった**ため、#140（エピック #131 の設計判断 D-10）の方式は下記「旧方式」に履歴として残す。

## 現行: タスク同一性が会話同一性（#242）

追いメンションは**新しいタスクではなく、同じタスクへの追加メッセージ**である。

| | 意味 | Slack での値 |
|---|---|---|
| `Task.id`（= `source_task_id`） | **1 会話**（スレッド） | `{channel}:{reply_ts}` |
| `Task.message_key` | **1 配送**（個々のメンション） | `{channel}:{ts}` |

同じスレッドの 2 通目は同じ `Task.id` を持つので、worktree・ブランチ・リポジトリ・エージェントセッションが**構造的に共有される**。相関のための仕組みは要らない — 同じ行だから。

メッセージは [state.db](/data/state-db.md) の `task_messages` 台帳に 1 配送 1 行で積まれ、`processed_at IS NULL` の集合が「まだエージェントに渡していない」キューになる。dispatch は未処理分を時系列で連結して 1 回のプロンプトにするので、連投 3 件でも **1 dispatch = 1 返信**になる。

タスクが終端に達していれば `TaskEvent::Reopen` で `Queued` へ戻る。これは `Done` の意味を「永久に完了」から**「未処理メッセージが無い」**へ変える意図的な設計判断で、「終端は不可逆」という不変条件を手放している。

新規会話でしか必要のないリポジトリ解決は `task/lookup`（P→O）で省く。既知の会話ならソースプラグインは LLM 分類も選択 UI も出さない（詳細は [task-source-slack](/components/task-source-slack.md)）。

`message_key` を持たないソース（GitHub Issue / Notion ページ）は `Task.id` にフォールバックするため、**1 メッセージ = 1 タスクのソースは無改修で従来どおり**動く。

## なぜ変えたか

Claude Code はセッションを **cwd 単位**で保存する（`~/.claude/projects/<cwd をエンコードしたディレクトリ>/<id>.jsonl`）。一方 totsuka は「1 タスク = 1 worktree」なので、旧方式では同一スレッドの 2 通目が必ず**別ディレクトリ**になり、`claude --resume` がセッションを見つけられなかった。実機で確認済み:

```
$ cd <2 通目の worktree> && claude --resume 6e3515d5-...
No conversation found with session ID: 6e3515d5-f1a1-4165-8a82-9d76a46a8a33
```

旧方式の前提「worktree は通常フローで新規作成し、セッションだけ使い回す」が claude 相手には成立せず、**スレッド返信アシスタントの中核が動いていなかった**。同じタスク = 同じ worktree にすればこの制約自体が消える。

なお resume は本質的に失敗しうる（セッションファイルの消失、`~/.claude/projects` の掃除）。agent プラグインが [`SESSION_UNRESUMABLE`](/components/agent-ide-herdr.md) を返すと Orchestrator は resume なしで 1 回だけ再送する — 文脈は失うがタスクは進む。

## 旧方式（#140、protocol 0.1.3〜0.2.4。**撤去済み**）

追いメンションを**別タスク**として ingest し、`Task.thread_key`（`{channel}:{thread_ts}`）で同一 workflow の先行タスクを検索して、そのセッション ID を `task/dispatch(resume_session_id)` に渡していた。`thread_key` 列と `find_by_thread_key` は #264（protocol 0.3.0 / state.db v7）で撤去した — `Task.id` 自体が会話を指すようになり、相関すべき「先行タスク」が存在しなくなったため。

## E-09（誤スレッド送信防止）

返信の宛先は常に**そのタスク自身の** `source_task_id` である。シグナル → タスクの相関は `job_id`（task_id を内包）起点で解決され、`tool_session_id` から宛先を推測する経路は存在しない。会話が 1 タスクになった #242 以降はこの不変条件がさらに強くなった — 「別タスクだが同じセッション」という状態がそもそも作られない。
