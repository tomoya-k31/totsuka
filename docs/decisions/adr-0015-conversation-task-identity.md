---
type: Decision
title: ADR-0015 タスクの同一性を「1 メッセージ」から「1 会話」へ変える
description: "Slack スレッドの 2 通目以降が必ず dispatch failed になる実機バグ（Claude Code はセッションを cwd 単位で保存する一方 totsuka は 1 タスク = 1 worktree）に対し、追いメンションを別タスクにして thread_key で相関する #140 の方式をやめ、Task.id 自体を会話（スレッド）の識別子にする決定。個々の配送は Task.message_key で識別し、メッセージは task_messages 台帳に積む。終端は可逆になる。"
tags: [conversation, identity, slack, protocol, state-db, resume]
timestamp: 2026-07-26T00:00:00+09:00
status: accepted
---

# Status

Accepted — 2026-07-26（[#242](https://github.com/tomoya-k31/totsuka/issues/242)。子 issue #254〜#265 で実装完了）

#140（エピック #131 の設計判断 D-10）の「追いメンションは新タスク + `thread_key` 相関」を **supersede** する。同方式は protocol 0.1.3〜0.2.4 に存在し、0.3.0（#264）で撤去した。

# Context

PR #240 後の実機検証で、**Slack スレッドへの 2 通目以降が必ず失敗する**ことが判明した。

```
resuming the prior task's tool session for thread continuity (#140)
  prior_task_id=15 task_id=16 tool_session_id=6e3515d5-...
ERROR dispatch failed: herdr error (agent_not_found): agent target w39:p2 not found
```

原因は 2 つの前提の衝突である。

- **Claude Code はセッションを cwd 単位で保存する**（`~/.claude/projects/<cwd をエンコードしたディレクトリ>/<id>.jsonl`）
- **totsuka は 1 タスク = 1 worktree**

#140 は「追いメンションは別タスクだが、worktree は新規作成しつつ**セッションだけ使い回す**」という設計だった（D-10）。しかし同一スレッドの 2 通目は必ず別ディレクトリになるため、`claude --resume <id>` はセッションを見つけられない。実験で確定させた:

```
$ cd <task 16 の worktree> && claude --resume 6e3515d5-...
No conversation found with session ID: 6e3515d5-f1a1-4165-8a82-9d76a46a8a33
```

`--resume` に失敗した pane は即死し、herdr が `agent_not_found` を返す。**スレッド返信アシスタントの中核が、設計の前提からして成立していなかった。**

`thread_key` を足す・resume 前に worktree を寄せる、といった弥縫策はいずれも「別タスクだが同じ会話」という状態を維持するコストを払い続ける。そもそもなぜ別タスクなのか、という点に問題があった。**GitHub の issue も Notion のページも、既に「会話」単位でタスク化されており、コメントがその中のメッセージにあたる。Slack だけが不揃いだった。**

# Decision

## 1. `Task.id` は会話、`Task.message_key` は配送

| | 変更前 | 変更後 |
|---|---|---|
| `Task.id`（= `source_task_id`） | `{channel}:{ts}` = 1 メンション | `{channel}:{reply_ts}` = **1 スレッド** |
| 個別メッセージの同一性 | `Task.id` が兼務 | **`Task.message_key`**（加算的） |
| スレッド識別子 | `Task.thread_key` | 撤去（`Task.id` が兼ねる） |

同じスレッドの 2 通目は同じ `Task.id` を持つので、worktree・ブランチ・リポジトリ・セッションが**構造的に共有される**。相関の仕組みは要らない — 同じ行だから。

`message_key` が `None` のソースは `Task.id` にフォールバックするため、**1 メッセージ = 1 タスクのソース（GitHub Issue / Notion ページ）は無改修**で従来どおり動く。

トップレベルのメンションは `thread_ts` を持たず `reply_ts == ts` なので、**1 通目の task_id・ブランチ名・worktree パスは変わらない**。既存データの移行も不要だった。

## 2. メッセージ台帳（`task_messages`、state.db v5）

1 配送 1 行。`processed_at IS NULL` の集合が「まだエージェントに渡していないメッセージ」= キューになる。`UNIQUE (task_id, message_key)` が at-least-once 配送を冪等化する。

`hook_events`(v2/v3) と同形にしたのは、**問題の形が同一**（at-least-once 配送の冪等化）で、そのパターンが既にこのリポジトリで実証済みだから。監査用に正規化済み `Task` 全文を `payload` に持ち、表示用に `author`/`body`/`url` を非正規化する — 読み取り側が JSON を走査しなくて済むようにするため（このスキーマに JSON 走査は 1 件も無く、ここが第 1 号になってはいけない）。

## 3. `Done` は「未処理メッセージが無い」— 終端は可逆になる

終端タスクへの新着メッセージは `TaskEvent::Reopen` で `Queued` へ戻す。`Retry`（同じ指示をやり直す = 人間の `task retry`）とは**別イベント**にした。意味が違うためで、`Reopen` は「新しい指示が来た」を表す。

これは「終端は不可逆」という不変条件を**意図的に手放す**判断である。影響は事前に調査済みで、遷移ガード（Escalate/Fail/Cancel は非終端のみ）・孤児 worktree sweep・スロット管理はいずれも破綻しない。

## 4. `task/lookup`（protocol 0.2.4、P→O）

会話が既知かを submit 前に問い合わせる読み取り専用 RPC。`{source, task_id}` → `{known, repo?}`。

リポジトリ解決は**新規会話だけの仕事**である。既存の会話は既にリポジトリを持っているか、あるいは今まさに人間が picker で選んでいる最中で、決着をつけるのは Orchestrator 側。ここで再解決すると、よくても LLM 呼び出しの空費、悪ければ**既に選択 UI を見ている人間の前に 2 枚目の picker を出す**ことになる。

**到達不能時は必ず従来の解決へ縮退する。** この問い合わせは Orchestrator のイベントループで処理されるため、worktree 作成等で詰まっていれば待たされる。縮退できなければ「省ける仕事を省く」ための RPC がメンション処理を止めることになる。

## 5. `SESSION_UNRESUMABLE`（protocol 0.2.4）

resume は本質的に失敗しうる（セッションファイルの消失、`~/.claude/projects` の掃除）。agent プラグインがこのコードを返すと、Orchestrator は `resume_session_id` なしで **1 回だけ**再送する。文脈は失うがタスクは進む。

契約を「セッションが使えない」という**バックエンド非依存の言葉**で定義したのが要点で、各 agent プラグインが自分のバックエンド固有エラー（herdr なら `agent_not_found`）をここへ写像する責務を負う。別マルチプレクサが増えても core は無改修で動く。

## 不採用案

| 案 | 理由 |
|---|---|
| (A) `thread_key` を維持し、resume 前に先行タスクの worktree へ cwd を寄せる | 「別タスクだが同じ会話」という状態のコストを払い続ける。worktree の所有権が曖昧になり、掃除ポリシーが 2 タスクにまたがる。そもそもなぜ別タスクなのかという問いに答えていない |
| (B) 会話インデックスをプラグイン側に持つ | 再起動を跨いだ保証が要る以上、結局は永続ストアが必要になる。orchestrator の DB に既にあるものの二重管理 |
| (C) `task/lookup` の代わりに会話一覧をプラグインへ配布する | staleness とサイズの設計が要る。無関係なスレッドで picker が出ない保証も作れない |
| (D) `message_key` の UNIQUE キーに編集時刻（`revision`）を含める | 誤字修正が高価な再実行と二重返信を招く。含めない場合の失敗は「編集しても何も起きない」で害が小さい。SQLite は制約を in-place 変更できないが、必要になればテーブル再構築で広げられる |

# Consequences

- **実機バグが構造的に消えた**。同じ会話 = 同じタスク = 同じ worktree なので、Claude Code の cwd スコープ制約と衝突しない。
- **`Done` の意味が変わった**。「永久に完了」ではなく「未処理メッセージが無い」。運用面では `totsuka task cancel` / `retry` の案内文もこれに合わせた（完了した会話の続け方は「次のメッセージを送る」であって再実行ではない）。
- **連投が 1 返信になる**。未処理メッセージを時系列で連結して 1 回 dispatch するため、pane の二重起動も起きない。
- **実行中に届いたメッセージは完了後にまとめて処理される**。動作中のエージェントに割り込むと pane と返信が二重になるため、ingest は意図的に放置し、完了後に `requeue_conversations_with_unsent_messages` が拾う。`Failed` / `Cancelled` は対象外 — 前者は同じ恒久エラーで無限ループになり、後者は人間の判断を覆すため。
- **protocol は 0.2.4（加算的）→ 0.3.0（`thread_key` 削除、破壊的）**。同梱プラグインの manifest 上限は `<0.4` へ。
- **state.db は v5（台帳）→ v6（既存タスクのバックフィル）→ v7（`thread_key` DROP）**。3 段に分けたのは意図的で、途中で止まっても壊れた状態にならないようにするため。特に v6 が無いと、既存の終端タスクが最初の再配送で reopen され再実行される（返信ソースなら二重返信）。
- **`task show` に会話履歴が出る**。1 タスク = 1 会話になった以上、そのタスクに何が届いたかが見えないと監査・デバッグができない。

# Citations

1. 実機ログと `claude --resume` の実験結果 — [#242](https://github.com/tomoya-k31/totsuka/issues/242)
2. 会話継続の用語と現行方式 — [会話継続（conversation continuity）](/glossary/conversation-continuity.md)
3. 台帳スキーマと migration — [状態DB（SQLite state.db）スキーマ](/data/state-db.md)
