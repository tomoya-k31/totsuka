---
type: Decision
title: ADR-0064 notion のタスクは at-most-once のままにする
description: "notion のタスクがトリガーによらず 1 回しか実行されない件を、実装漏れではなく決定として確定させる。Notion API にプロパティ単位の更新時刻が無いため github の lane identity が移植できず、代替 3 案（ページ単位の last_edited_time / 時刻なしの鍵 / プラグインが直近ステータスを記憶）と core 側でステータス差分を鍵にする案はいずれも代償が釣り合わない。再実行を要する運用が実在してから決め直す。"
resource: https://github.com/tomoya-k31/totsuka/issues/573
tags: [decision, notion, task-source, ingest, message-key, adr]
generated: { by: claude-code/opus-5, at: 2026-08-27T06:45:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。実装変更は無く、**現状を決定として確定させる** ADR である。

[ADR-0063](/decisions/adr-0063-trigger-assignee.md) §5 が「notion の at-most-once はソース全体の性質で #573 の担当である」と委ねた問いに答える。github 側の対になる決定は [ADR-0059](/decisions/adr-0059-task-claim-exclusion.md) §5。

# Context

`NotionClient::normalize_page` は `Task.message_key` を**無条件で `None`** にしている。core はそれを `task.id` にフォールバックさせ、`UNIQUE(task_id, message_key)` が以降の再配送をすべて重複として捨てる。

結果、**notion のタスクはトリガーが何であれ 1 回しか実行されない**。ステータスを戻しても再配送は捨てられ、`totsuka task retry` も `done` を拒否する（「終わった会話は新しいメッセージで再開しろ」と案内するが、notion ソースにはその新しいメッセージを作る経路が無い）。

github は #556 で lane identity を得ている。`trigger.status` を持つワークフローの配送に `status:{列名}@{セルの updatedAt}` を刻むので、人間がカードをトリガー列へ差し戻すと**新しいメッセージ**になり、完了した会話が再開する。

**この差は「notion の実装が遅れている」ではない。** Notion API には**プロパティ単位の更新時刻が存在しない**。ページが持つのは `last_edited_time`（ページ全体）だけで、ステータスセルがいつ変わったかは取れない。

# Decision

**at-most-once のままとし、それを仕様として文書化する。** 再実行を要する運用が実在してから、その要求に合わせて決め直す。

## 却下した案

| 案 | 破れ方 |
|---|---|
| `status:{name}@{page.last_edited_time}` | ページ単位の時刻なので、**カードがトリガー列に居る間にタイトルや説明を編集しただけで再実行される**。`on_start` で列から出す運用なら窓は短いが、`on_start` は任意なので前提にできない |
| `status:{name}`（時刻なし） | 列を出て同じ列へ戻すと同じ鍵になり、再実行**されない**。at-most-once とほぼ変わらない |
| プラグインが直近ステータスを記憶し、変化時だけ新しい鍵を作る | 記憶はプロセスローカルなので**再起動で消える**。消えた後を「取りこぼす側」に倒すか「余分に走る側」に倒すかは、実運用の要求なしには決められない |
| core 側で「前回配送時と違うステータスなら新しい配送」にする | **成立しない。** `UNIQUE(task_id, message_key)` は履歴全体に効くので、`Todo → 実行中 → Todo` と戻すと 2 回目の `Todo` が衝突する —— ステータス値ごとに 1 回しか使えない鍵になり、2 度目の差し戻しで止まる |

## なぜ今決めるのか

実運用は github ボードで、notion は live-e2e にも入っていない。**代償を払う相手がいないうちに、代償のある機構を入れない。**

一方、決めずに開けたままにするのは害がある。#572 の実装で「`assignee` 単独トリガーは `status` を足せば再実行できる」という警告文を書いたが、**notion では成り立たない**ため、効かない対処を案内するところだった（レビューで捕捉し、警告をソース別に切り分けた）。`at-most-once である`と明記されていれば、この種の誤りは起きない。

# Consequences

- **notion のタスクはボード上から再実行できない。** `trigger.status` を足しても変わらない。
- **`assignee` 単独トリガーの警告は github だけに出る**（[ADR-0063](/decisions/adr-0063-trigger-assignee.md) §5）。lane identity を刻まないソースに「`status` を足せ」と言っても直らないため。
- **github 側にも限界がある。** lane identity が付くのは `trigger.status` を持つワークフローだけで、`label` 単独・`assignee` 単独のトリガーは github でも at-most-once である。「github は再実行できる」と無条件に書かないこと。
- 要求が出たら #573 を再開し、この ADR を後継で置き換える。

# 関連

- [ADR-0059 claim による二重着手防止](/decisions/adr-0059-task-claim-exclusion.md) §5 —— github の lane identity
- [ADR-0063 trigger.assignee](/decisions/adr-0063-trigger-assignee.md) §5 —— この問いをここへ委ねた決定
- [task-source-notion](/components/task-source-notion.md)
