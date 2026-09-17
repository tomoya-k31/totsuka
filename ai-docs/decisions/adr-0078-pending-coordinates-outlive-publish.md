---
type: Decision
title: ADR-0078 Slack の pending 座標は result/publish で消費しない
description: "1 会話が複数回 dispatch されうる（#242 で終端が可逆になった）のに、Slack プラグインが result/publish を会話の終端とみなして返信先の座標を消費していたため、追いメンションで再起動された run の返信が必ず失われていた実機バグへの決定。座標は peek のみとし take_pending を削除して、有界化は FIFO 上限 1024 だけに委ねる。message_key 単位のキー・reopen 通知の新設・座標の永続化を採らない理由と、残る no pending 原因が 3 つに減ることを記録する。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/approval.rs
tags: [decision, adr, slack, approval, conversation, lifecycle]
generated: { by: claude-code/opus-5, at: 2026-09-17T20:30:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0015
    resource: /decisions/adr-0015-conversation-task-identity.md
    title: "タスクの同一性を 1 会話へ変えた決定 — ADR-0015"
  - id: live-state-db
    resource: "実機の state.db（task 27、2026-09-17）の tasks / task_messages / events 各行"
    title: "実機ログと台帳 — 原因特定の一次証拠"
---

# Status

**採択（stable）。** [ADR-0015](/decisions/adr-0015-conversation-task-identity.md) 決定 3（「`Done` は未処理メッセージが無い」= 終端は可逆）を、Slack プラグイン側の pending-mention index へ伝播させる。承認フロー（[ADR-0003](/decisions/adr-0003-slack-reply-assistant.md)・[ADR-0074](/decisions/adr-0074-single-draft-surface.md)）と direct 投稿（[ADR-0057](/decisions/adr-0057-per-workflow-publish-and-cleanup.md)）の挙動そのものは変えない。

# Context

## 実機で出た症状

エージェントは答えを書き終えているのに、Slack には**何も出ずに終わる**。

```text
ERROR orchestrator_core::run::finalize: task failed: result/publish failed:
  plugin `slack` method `result/publish` failed (-32603):
  task C…:….656719 has no pending Slack coordinates
  (plugin restarted since the mention?) → the reply cannot be placed;
  re-trigger from a fresh mention  kind=publish task_id=27
```

**プラグインは再起動していない。** エラー文言の推測は外れている。実機の `state.db` に残っていた task 27 の台帳とイベント列が原因を確定させた。

| 時刻 | 何が起きたか |
|---|---|
| 05:50:22 | 1 通目のメンション → `queued`、座標を index に格納 |
| 05:50:45 | `dispatched` |
| 06:21:47 | `escalated`（エージェント側のログイン切れで 1800 秒無信号） |
| 06:31:51 | **2 通目のメンション**。実行中なので ingest は台帳に積むだけ（`Appended`）。プラグイン側は同じキーの座標を 2 通目のもので上書き |
| 11:06:05 | 1 通目の run が `result/publish` → **`take_pending` が座標を消費** → `done` |
| 11:06:06 | `done` → `queued`（`{"cause":"messages_arrived_while_working","kind":"reopen"}`） |
| 11:06:28 | 2 通目の run が `dispatched` |
| 11:07:18 | 2 通目の run が `result/publish` → **座標が無い** → `failed` |

## 前提が崩れていた

`approval.rs` にはこう書いてあった。

```rust
// Publish is the task's terminal step: consume the pending entry.
let Some(pending) = state.take_pending(task_id) else {
```

このコメントは **#242 以前は正しかった**。1 メンション = 1 タスクだった頃、publish は確かに終端だった。#242 で `Task.id` が会話（スレッド）の識別子になり、`Done` が可逆になった時点で前提は崩れている —— **1 会話は run のたびに `result/publish` を 1 回ずつ起こす**。ところが pending index のキーは会話 1 つにつき 1 エントリなので、最初の publish がそれを食べてしまえば、以降の run は構造的に必ず返信先を失う。

再現条件は「**エージェントが作業している最中に、同じスレッドへもう一度メンションする**」だけである。今回 5 時間ぶん滞留したのはログイン切れによる escalate のせいだが、それは条件ではなく増幅要因にすぎない。

失われるのは返信だけで、答えの本文は `events` の `publish_artifact` に残り、worktree も publish 失敗時の規約どおり保持される。**静かに消えるのではなく、拾い直せる形で失敗していた**のは救いだが、利用者から見れば「返事が来ない」である。

# Decision

## 1. 座標は消費しない（peek only）

`publish_draft` / `publish_direct` はいずれも `pending()` で覗くだけにし、`take_pending` は**削除する**（呼び出し側が無くなったため残せば dead code になる）。

publish の成否で分岐しない。成功しても失敗しても、エントリはそこに残る。

## 2. 有界化は FIFO 上限だけに委ねる

`PENDING_CAP = 1024` が唯一の境界になる。index の意味は「**返信待ちの会話**」から「**直近 1024 会話の返信先**」へ変わる。上限に当たったときの警告文（押し出されたエントリの返信はもう置けない）は従来どおり出る。

## 3. エラー文言は据え置く

「plugin restarted since the mention?」は今回の原因ではなかったが、**修正後は残る原因の筆頭になる**（下の帰結を参照）。当たっていない推測を書き換えるのではなく、当たる状況だけを残すことで正しくする。

# 不採用

- **pending を `message_key` 単位にする。** 返信先は常に「そのスレッドの最新の問い手」であるべきで（#242 の意図）、配送単位に戻すと 1 通目の答えが古い座標に着く。加えて 1 回の run が台帳の複数メッセージをまとめて受け取るため、run と message の対応はオーケストレータの台帳にしかなく、プラグインには対応付ける手段が無い。
- **publish で消費し、reopen のときに再インストールする。** プラグインは reopen を知らない —— protocol に「会話が再開した」を伝える口が無い。口を新設するのは、消費をやめるだけで済む問題に対して大きすぎる。
- **座標の永続化。** プラグイン再起動で座標が消えるのは事実（drafts.json と違い index はメモリのみ）だが、今回の原因ではなく、直交する別の問題である。必要になったら別途決める。

# 帰結

- **残る "no pending" の原因は 3 つだけになる**: プラグインの再起動、FIFO 上限による押し出し（1024 会話ぶん）、自分の配送のロールバック（`discard_pending_delivery`）。いずれも文言どおり「再メンションで再開する」が正しい対処になる。
- index は最大 1024 会話ぶん常駐し続ける。1 エントリは短い文字列数本なので、上限に張り付いても常駐量は問題にならない。
- **古い会話へ誤爆することはない。** キーが会話そのものなので、残ったエントリが指す先は常にその会話である。
- **1 通目の答えが 2 通目の送信者宛に出る**という派生の挙動は、この決定では変えない。`insert_pending` が同じキーを上書きする以上、1 通目の run が publish する時点で index にあるのは 2 通目の座標である。同一スレッド内なので投稿先は正しく、変わるのは冒頭のメンション先だけで、#242 の「最新の問い手に返す」という意図とも整合する。
- Discord プラグイン（`task-source-discord`）は**同じ欠陥を持たない**。あちらは「1 投稿 = 1 タスク、forever」（[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md)）でタスクが会話にならないため、requeue による 2 回目の run が存在しない。`take_pending` はそのまま残す。
