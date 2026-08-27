---
type: Term
title: 要対応（Attention）
description: 人間が動かさない限り永久に進まない非終端タスクの集合。pending / waiting_input / verifying / escalated / queued+wait_reason の 5 状態からなり、メニューバーのバッジ（F-109）が数える対象。終端状態を含めないのは、含めると数字が単調増加して 0 に戻らなくなるため。
tags: [glossary, attention, menu, status, task-state]
generated: { by: claude-code/opus-5, at: 2026-08-28T05:40:00+09:00 }
status: stable
owner: tomoya-k31
---

# 要対応（Attention）

**人間が動かさない限り、永久に進まない**[タスク](/glossary/task.md)の集合。「人間の側の義務」として名付けた語で、タスクの状態名ではない。

| 状態 | 何を待っているか |
|---|---|
| `pending` | リポジトリ選択の人間確認（F-14） |
| `waiting_input` | エージェントの質問への回答（F-35） |
| `verifying` | `totsuka task verify --pass/--fail` による人間検収（#131 D-01） |
| `escalated` | pane での詰まりの解消（UNKNOWN 連続 / タイムアウト / 相関異常、F-103） |
| `queued` かつ `wait_reason` あり | 記録された停止理由の解消（#407。現状の唯一の kind は `blocked_agent_tools`） |

`queued` は **`wait_reason` の有無で二分される**。理由が記録されていないものは単に順番待ちで、放っておけば自分で始まるので要対応ではない。

# 何が入らないか

**終端状態（`done` / `failed` / `cancelled` / `skipped`）は含めない。** これは重要度の判断ではなく、数え方の帰結である —— `StateDb::list_tasks` は絞り込みも上限も無く過去の全タスクを返すので、終端を数えるとバッジの数字が**単調増加して二度と 0 に戻らない**。「押せば減る」性質を失った数字は読まれなくなる。失敗の確認は `totsuka status` と通知（F-90）の担当。

`dispatched` / `running` / `publishing` も入らない。エージェントが進めており、人間に打つ手が無い。メニューのドロップダウンでは「稼働中」の別節に出るが、バッジの数字には入らない。

# `waiting_input` との違い

**`waiting_input` は要対応の部分集合であって、同義ではない。** 集合の側を「人間待ち」と呼ばなかったのはこの衝突を避けるためで、「人間待ちは 3 件ですが、うち `waiting_input` は 1 件」という文が毎回「どっちの待ち？」を読み手に問い直させることになる。

# なぜこの集合に意味があるのか

`waiting_input` と `escalated` は**スロットを解放する**（F-45）。したがって同時実行数だけを見ていると「枠が空いている＝順調」に見えるが、実際には人間待ちで止まっている。要対応は、その見え方の穴をふさぐために数える集合である。

定義の実体は `crates/orchestrator-cli/src/menu_cmd.rs` の `classify` 一箇所にあり、[`TaskState`](/glossary/task.md) を網羅する `match` で書かれている —— 状態が増えたときに黙って「対象外」に倒れず、コンパイルが通らなくなるようにするため。

関連: [メニューバー表示の ADR](/decisions/adr-0065-menubar-status.md)、[click-to-focus](/glossary/click-to-focus.md)（要対応の行をクリックした先）、[Notifier](/glossary/notifier.md)（同じ事象を一過性に届ける側）。
