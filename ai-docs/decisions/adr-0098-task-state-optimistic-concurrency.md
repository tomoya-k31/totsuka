---
type: Decision
title: ADR-0098 タスクの状態遷移を行バージョンによる楽観的並行制御にし、競合をタスク単位で隔離する
description: 状態 DB の書き込み元が 2 つあるため、Engine が読んでから書くまでの間に外部で状態が動くと、不正遷移で run 全体が止まるか、古い判断が合法な遷移として黙って通る。tasks.state_version を足し、遷移の API を「読んだ版数（TaskRef）」でしか呼べないようにして、版数が違えば Conflict を返す。Engine は Conflict をタスク単位の境界（隔壁）で受け止めて run を続ける。版数が一致したうえでの不正遷移は Engine のバグとして debug では落とし、release では隔離する。
resource: https://github.com/tomoya-k31/totsuka/issues/763
tags: [decision, state, sqlite, concurrency, engine, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-24T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-763
    resource: https://github.com/tomoya-k31/totsuka/issues/763
    title: "Issue #763 と詳細設計コメント"
---

# Status

stable（[#763](https://github.com/tomoya-k31/totsuka/issues/763)）。2 段で入れる。

- PR 1: 振る舞いを変えない準備。`tasks.state_version`（v9）、`TaskRef`、全呼び出し元の条件付き API への置き換え、`StateError::Conflict`、本 ADR
- PR 2: Engine の隔壁と再現テスト

[ADR-0094](/decisions/adr-0094-task-control-endpoints.md) は書き込み元を Engine に寄せる決定だが、DB への直接書き込みをフォールバックとして残している。本 ADR はそのフォールバックがあっても Engine が止まらないための防御である。

# Context

状態 DB の書き込み元は 2 つある。`totsuka run` の Engine と、`totsuka task cancel` / `retry` の DB 直接書き込み（`run` が応答しないときのフォールバック）である。Engine はタスクを読み、claim・ステータスの書き戻し・エージェント呼び出しで数秒 await してから遷移を書く。その間に外部で状態が動くと、次の 2 通りのどちらかになる。

1. **不正遷移で run が止まる。** 例: `Cancelled` に `Fail` を当てる。`StateError::Transition` は `EngineError::Db` に変換され、`Engine::run` から返ってループが終わる
2. **合法な遷移として黙って通る。** 例: cancel → retry で `Queued` に戻ったタスクに、古い試行の `Fail` を当てると `Queued → Failed` が成立する

調べて分かった事実:

- 旧来の `apply_event(id, event)` は、Engine が何を前提に書いたかを受け取らない。そのため Engine のバグと外部競合が同じ `InvalidTransition` になり、区別できない
- 状態の値だけで比べると ABA を見落とす。例: cancel → retry → 再 dispatch で、Engine が読んだときと同じ `Dispatched` に戻る
- `Engine::run` の最上位で受け止めても直らない。`dispatch_ready` はその回に dispatch する予定のスロットを先にまとめて確保し、スロットの持ち主（`slot_holders`）は 1 件ずつ記録する。途中で抜けると、計画済みで未 dispatch のタスクのスロットが漏れる

# Decision

1. **楽観的並行制御にする。** `tasks.state_version`（v9、`NOT NULL DEFAULT 0`）を足し、状態を書く唯一の関数（`apply_event_tx`）の中で遷移のたびに 1 増やす。ノートのように遷移でない書き込みでは増やさない。版数の比較は遷移の判定より**前**に行い、違えば `StateError::Conflict(TransitionConflict { id, expected, actual, actual_state, event })` を返して何も書かない
   - 遷移のトランザクションは `BEGIN IMMEDIATE` で書き込みロックを先に取る。deferred のままだと、版数を古いスナップショットで読み、別の接続が間にコミットしたときの書き込みへの昇格が `SQLITE_BUSY_SNAPSHOT` で拒否される（busy handler は再試行しない）。競合が Conflict ではなく致命的な DB エラーとして表に出てしまうため
2. **API は「読んだ参照」でしか呼べない。** `apply_event` / `retry_task` は id ではなく `TaskRef` を取る。`TaskRef` は id と版数の組で、`TaskRecord::task_ref()`、`StateDb::task_ref(id)`、または前の遷移の戻り値から得る
   - 成功すると更新済みの `TaskRef` を返す。同じタスクに続けて書くときはそれを使う
   - `TaskRef` は `Clone` / `Copy` でない。遷移が参照を消費するので、古い参照の使い回しはコンパイルが通らない
   - id と版数を取り違える、あるいは別のタスクの版数を渡す誤りも型で防ぐ
3. **条件なしの入口は残さない。** 呼び出し元はすべて版数を渡す: Engine、`task_control`（Engine 経由と CLI の直接書き込みの両方）、`task verify`、起動時の recovery、テスト。同じトランザクション内で読んで書く内部経路（取り込み時の reopen）だけは版数を持たない
4. **CLI 側の競合は、案内つきの拒否にする。** cancel / retry は既存の `lost_race`（`200 + ok:false` 相当の拒否）で、`task verify` は `→ totsuka task show <id>` を添えたエラーで返す
5. **Engine は Conflict をタスク単位の境界で受け止める**（PR 2）。`EngineError` に DB 障害とは別のバリアントとして `Conflict(TransitionConflict)` を足し（`From<StateError>` が振り分ける）、`?` で境界まで運ぶ。境界はタスクを 1 件ずつ処理する箇所で、隔壁は `Engine::isolate_task` 1 つである。当てているのは次の箇所: `dispatch_ready`（dispatch 1 件ごと）、`select_repos`、`requeue_conversations_with_unsent_messages`、`on_signal`（hook シグナルと spool の再生の両方が通る）、`sweep_signal_timeouts`、エージェント状態の通知（`on_event` の `State`）、読み取り専用違反の掃除、プラグインのクラッシュでのフェイル
   - 受け止めたら warn を 1 行出し、そのタスクのメモリ上の状態（スロット、セッションの経路、出力バッファ）を捨てて処理を続ける。以降は DB を正として扱う
   - 通知・ソースへの書き戻し・pane を閉じる処理はしない
   - DB 障害は今までどおり致命的
6. **版数が一致したうえでの不正遷移は Engine のバグとして扱う**（PR 2）。debug ビルドでは落とし、release ビルドでは error ログを出してタスク単位で隔離する

# 代替案と不採用理由

| 案 | 不採用理由 |
|---|---|
| 境界で `InvalidTransition` を受け止めるだけ（API は変えない） | Engine のバグと外部競合を区別できない。合法な遷移として黙って通る競合も残る |
| 状態の値で compare-and-set する | ABA を見落とす（cancel → retry → 再 dispatch） |
| 監査イベントの最新 id を版数に使う | ノート行でも id が進むので、偽の Conflict が出る |
| id と版数を別々の引数にする（生の整数や newtype） | 取り違えや、別のタスクの版数を渡す誤りを型で防げない |
| 条件なしの版を CLI・recovery・テスト用に残す | 書き込みの入口が 2 つになり、新しいコードが条件なしの版を選べてしまう |
| `Engine::run` の最上位で受け止める | 計画済みのスロットが漏れる。task_id が分からないので後始末もできない |
| Conflict をエラーでなく戻り値の enum で返す | 22 か所の呼び出しすべてに分岐が生える |
| 単一の書き込み元（Engine）に寄せる | ADR-0094 がフォールバックとして直接書き込みを残しているので、防御は別に要る |

# Consequences

- `TaskRecord` に `state_version` が増えた。v9 のマイグレーションは `totsuka run` が適用する（`open_no_migrate` を使う CLI は、それまでは古いスキーマとして拒否する。従来どおりの挙動）
- `apply_event` の戻り値は `(TaskState, TaskRef)`、`retry_task` の戻り値は `(TaskState, TaskRef, usize)` になった
- 遷移を書く関数（`fail_dispatch` / `finalize_success` / `fail_publish`）は、記録を書き込む参照を `record` とは別に受け取る。途中で遷移を適用した呼び出し元が、古い `record` の版数で書かないようにするため
- PR 1 の時点では Engine の中で Conflict はまだ致命的だった（以前なら黙って通っていた「古い判断の合法な遷移」も止まる）。PR 2 の隔壁で、run を止めるのは DB 障害と debug ビルドでの Engine のバグだけになった
- dispatch 失敗後の自動 retry（#492）の「requeue できなければ普通の失敗として扱う」逃げ道は、Conflict だけは握りつぶさず隔壁へ渡す。外部で状態が動いたタスクを失敗として通知しないため
- 起動時の recovery の中での Conflict は隔壁を通さない（常駐中の競合の窓ではないため）
- 回帰テストはモックプラグインの `gates`（指定メソッドの応答を、指定ファイルが現れるまで止める）で、sleep に頼らず競合を再現する
