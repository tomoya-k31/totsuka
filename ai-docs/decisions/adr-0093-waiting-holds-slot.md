---
type: Decision
title: ADR-0093 人間待ち（waiting_input / escalated）のタスクもスロットを保持する
description: F-45 は waiting_input と escalated でスロットを解放していたため、入力待ちで止まるタスクが多い運用では max_concurrency が上限として働かず、キューのタスクが次々に中途半端に進んで、人間がすべてに同時に答える羽目になっていた。人間待ちでもスロットを保持し、再開は保持したスロットのまま行う決定。設定での切り替えは設けず、既定の挙動を変える。
tags: [decision, scheduler, concurrency, waiting-input, escalated, adr]
generated: { by: claude-code/opus-5, at: 2026-09-23T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[F-45](/product/orchestrator-spec.md) の「待機状態はスロットを解放する」を置き換える。[ADR-0043](/decisions/adr-0043-human-approved-completion.md)・[ADR-0050](/decisions/adr-0050-question-tool-asking.md)・[ADR-0065](/decisions/adr-0065-menubar-status.md) が前提として書いた「スロット解放」は、この ADR 以降は成り立たない（park・掃引対象外・通知の部分はそのまま）。

# Context

F-45 は `dispatched → running → verifying → publishing` だけをスロットに数え、`waiting_input` と `escalated` では解放していた。理由は「待ちによる実質デッドロックの防止」だった。

実運用（gh-spec ワークフロー、`max_concurrency = 5`、orca）では、設計を詰める段階のタスクのほとんどが途中で人間に質問して `waiting_input` に入る。入ったとたんに枠が空くので、スケジューラはキューの次のタスクを起動し、それもまた質問で止まる。結果として:

- `max_concurrency = 5` なのに、orca 上では 8 つのタスクが同時に開いていた（dispatched 4 ＋ 入力待ち 3 ＋ 失敗 1）
- キューのタスクがすべて中途半端に進み、人間はすべての詳細設計に同時に答えなければならなくなった

上限は「人間が同時に面倒を見られる数」を決めるためのものなのに、人間の手が要る状態を数えていなかった。

# Decision

1. **`counts_toward_slot` に `WaitingInput` と `Escalated` を加える。** dispatch されてから終わるまで、すべての状態がスロットを保持する。`pending`（リポジトリ確認待ち）は dispatch 前なので従来どおり保持しない
2. **入力待ち・エスカレーションへの遷移でスロットを解放しない。** 解放していた 3 か所（`run::events` の `WaitInput`、`run::hooks` の `escalate` と escalated → waiting_input）から `release_slot` を外した
3. **再開は、保持しているスロットのまま行う。** `ResumeInput` での再取得は、スロットを持っていないタスク（再起動の境目など）にだけ行う。二重に取得すると、他のタスクの枠を食う
4. **再起動時の再構築（`recovery::active_slot_claims`）も同じ集合を使う。** `counts_toward_slot` を共有しているので、別の変更は要らない
5. **one-shot の `run` の終了判定（`Engine::settled`）は、人間待ちを「落ち着いた」とみなし続ける。** これまで `counts_toward_slot` を流用していたので、そのままだと入力待ちのタスクがある限り one-shot の `run` が終わらなくなる。判定から `WaitingInput` / `Escalated` を明示的に除く
6. **設定での切り替えは設けない。** 既定の挙動そのものを変える（利用者の判断）

# 代替案と不採用理由

- **設定キーで選べるようにし、既定は従来どおりにする** — 他の運用を変えずに済むが、上限が人間待ちを数えないことはこの運用に限った問題ではない。利用者の判断で既定を変えた
- **`waiting_input` だけを数え、`escalated` は解放のままにする** — どちらも「人間待ちで中途半端に止まっている」点は同じ。分けると、escalated → waiting_input の遷移でスロットを取り直す処理が要る
- **デッドロック対策として解放を残す（F-45 の原案）** — 待っているタスクが必要としているのは人間の返答であって、スロットではない。再開は保持したスロットで行うので、スロット待ちで止まる経路は無い。枠がすべて人間待ちで埋まれば新しいタスクは始まらないが、それはこの ADR が求めている挙動そのもの

# Consequences

- 入力待ちが `max_concurrency` 個たまると、人間が答えるか `totsuka task cancel` するまで、新しいタスクは起動しない
- Slack の対話のように「答えを待つ会話」が多い運用では、以前よりキューが進みにくくなる。枠を広げたければ `max_concurrency` を上げる
- `totsuka status` で見える「実行中」と「スロットの使用数」が一致するようになる。メニューバーの要対応表示（[attention](/glossary/attention.md)）は、枠だけでは見えない人間待ちを数えるものとして引き続き意味がある
- 検証: `tests/run_loop.rs` の `a_task_waiting_for_input_keeps_its_slot`。枠 1 で最初のタスクが入力待ちに入った後、2 つ目が `queued` のままであることを確かめる。入力待ちでの解放を戻すと、2 つ目が `dispatched` になって落ちる
