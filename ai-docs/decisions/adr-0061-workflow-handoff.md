---
type: Decision
title: ADR-0061 列パイプラインの段間は、会話を配送元のワークフローへ引き渡して繋ぐ
description: "同一 issue を列で受け渡す 2 段ワークフロー（design→implement）が動くようにするための設計。terminal な会話に別ワークフローの配送が届いたら workflow / mode / source_payload を 1 トランザクションで付け替えて Reopen する。実行中の会話は台帳に書かずに見送る。全自動ループを解禁する副作用に対しては、列を節点とするグラフの閉路検出を validate に入れる。会話行を段ごとに分ける案・実行中の乗り換え・opt-in フラグは不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/565
tags: [decision, core, workflow, ingest, conversation, pipeline, adr]
generated: { by: claude-code/opus-5, at: 2026-08-26T16:00:00+09:00 }
verified:
  - { by: human:tomoya-k31, at: 2026-08-26T12:15:00Z }
status: stable
owner: tomoya-k31
---

# Status

stable。実装・**実機検収（2026-08-26）**まで完了。

[ADR-0059](/decisions/adr-0059-task-claim-exclusion.md) §5 の「段間 handoff は別 issue」を**解消する**決定であり、同 ADR が入れた「別ワークフロー配送は破棄」を**置き換える**。[ADR-0015](/decisions/adr-0015-conversation-task-identity.md) の会話同一性（1 会話 = 1 行）は**不変** — 変わるのは、その行が属するワークフローが固定ではなくなること。

# Context

Spec §4.9 の設定例そのもの（design 列 → implement 列の 2 段パイプライン）が、**一度も動いていなかった**。

機序は 3 つの既存仕様の合成である:

1. 会話は `UNIQUE(source, source_task_id)` で issue につき 1 行
2. 行の `workflow` は作成時に決まり、`ON CONFLICT DO NOTHING` が以後**更新しない**
3. design 完了 → `on_success` でカードが implement 列へ → implement ワークフローが**同じ issue node id** で `task/submit` する

結果、2 段目の配送は「既知の会話・別ワークフロー」となり、#556 以前は message_key の dedup で黙って捨てられ、#556 以降は明示的に破棄されていた（破棄しないと「旧ワークフローでの誤 reopen」になるため、当時の判断としては正しい）。

#556 が「トリガー列への入場はリクエストである」を確立した以上、**どのワークフローの列に入ったかまで尊重する**のが整合的な帰結になる。

# Decision

## 1. terminal な会話は、配送元のワークフローへ引き渡す

ingest が「既存行 × 別ワークフロー」を見たときの分岐:

| 行の状態 | メッセージ | 挙動 |
|---|---|---|
| terminal | 新規 | **引き渡す**（append + Reopen + 3 列更新を 1 トランザクション） |
| 非 terminal | 新規 | **見送る。台帳にも書かない**（warn） |
| — | 重複 | `Duplicate`（変化なし） |

`Cancelled` / `Skipped` からも引き渡す。既存の Reopen 教義（**新しい**メッセージは新しい指示）と、claim が付け替え後の dispatch で再裁定することによる。

## 2. 動かす列は 3 つで、3 つとも load-bearing

| 列 | なぜ必要か |
|---|---|
| `workflow` | 下流は毎回 `workflows_by_name(record.workflow)` で設定を引くので、これ 1 つで段の設定一式（dispatch 先・状態書き戻し・verification・tool）が切り替わる |
| `mode` | **dispatch は workflow ではなくこの列から `ExecutionMode` を引く**。置き去りにすると implement 段が plan の制約下で走る。worktree 掃除の経路選択と plan 副作用検査もこの列を読む |
| `source_payload` | `task_from_record` がここから配送タスクを組み直す。置き去りにすると次の段が**前の段の指示文**で走る |

据え置くもの: `title`（会話の件名）／`repo`（worktree の同一性がこれに乗る）／`id`・`source_task_id`（会話の同一性そのもの）。

## 3. 原子性が本体

append・Reopen・3 列更新は 1 トランザクション。途中で落ちて「旧ワークフローのまま新メッセージだけ台帳にある」行になると、**以後の再配送は全部 dedup され、引き渡しが永久にできなくなる**。これは `append_task_message_reopening` が防いでいる座礁と同型で、反対側から到達する。

同じ理由で、**実行中の会話は台帳にも書かない**。書けば上記の座礁を能動的に作ることになる。

ただし**書かないことで回復するのは、再配送するソースに限る**。ポーラー（`plugin_sdk::poll_loop` は seen-set を持たない）は次の tick で同じ列入場を運び直すので、段が終われば引き渡しが成立する — 列パイプラインが前提にしているのはこの経路である。一方、**ack を先に返す push ソース（Slack の Socket Mode）は再配送しない**ので、実行中に届いた別ワークフローのトリガーは失われ、`warn!` だけが残る。それでも書かないほうが正しい: 書けば**全てのソース**で永久に座礁する（台帳が、ポーラーがこれから送る再配送を dedup してしまう）。

## 4. 由来は Reopen イベントの detail に刻む

`events` は `from_state` / `to_state` しか持たない（Retry と Reopen が `failed → queued` として区別できないのと同じ構造）。したがって由来は detail に置く:

```json
{ "kind": "reopen", "cause": "workflow_handoff",
  "workflow": { "from": "github-design", "to": "github-task" },
  "message_key": "status:Todo@2026-…Z" }
```

## 5. 全自動ループは閉路検出で塞ぐ

引き渡しは、**これまで破棄によって無害化されていた設定ミスを、全自動の無限実行に変える**。`A.on_success = B のトリガー列` かつ `B.on_success = A のトリガー列` と書けば、人間が 1 人も挟まらないまま A→B→A→… が永久に回り、毎周エージェントが起動して実費が出る。

`validate_workflows` を拡張し、**同一 `source` 内で列を節点・`set_status` を辺とするグラフ**（`on_start` / `on_success` / `on_failure` の 3 キー）を作って閉路をエラーにする。ADR-0059 の自己ループ検査は**その長さ 1 の場合として一般化・置換**した（同じ規約を 2 箇所に実装しない）。

エラー文は閉路の実経路を名指しし、直し方（どのワークフローもトリガーにしていない列を 1 hop 挟む = 人がそこからカードを動かす）を書く。検査は**字面のみ**で、core が trigger を解釈しない建前（[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）の範囲に留まる。

**報告は「絡み合った群ごとに 1 件」で、閉路ごとではない。** 探索は一度見た列を settle するので、同じ列群を通る複数の閉路は 1 件にまとまる（実装を線形に保つための意図的な選択で、`config validate` が起動ゲートである以上どちらでも安全側に倒れる）。エラー文自身が「1 つ直したら再検証せよ」と言うので、1 件の報告が「残り 1 つ」と読まれることはない。テストがこの挙動を固定している。

## 6. 既定で有効。opt-in フラグは作らない

フラグにすると spec §4.9 の例が「フラグを知らないと動かない」ままになり、この決定の起点そのものが残る。安全弁は 5 の閉路検査と、実行中の見送りである。

# Consequences

- **ワークフローのリネームが自然に治る**: 旧名の terminal 行へ新名の配送が届いた時点で付け替わる。#556 が warn で可視化した座礁は、専用の対処を書かずに解消した
- **段間で worktree とエージェントのセッションが引き継がれる**: `repo` と `source_task_id` が不変なので worktree パスは同一で、`latest_session` 経由で同一会話が resume される。design の文脈を持ったまま implement が始まるのが既定の挙動になる
- **claim（ADR-0059）との相互作用**: 前段の assignee は成功時も残るので、次段の claim は pre-read fast-path で**同じ保持者が続投**する。人間が assignee を付け替えれば次段の担当を変えられ、外せば claim レースが決める
- キューにいる段を止めて乗り換えることはしない（cancel は人間の決定という教義を守る）。列を移しても前段は走り切り、直列化する
- `message_key` を刻まない配送（label-only トリガー等）は引き渡しも起きない。lane 差し戻しと同じ制約で、理由も同じ

## 既知の限界（受容・文書化）

- **`initial_prompt` は次の段で再提示されない。** 引き渡された会話は必ず resume で dispatch され、`initial_prompt` は「新規会話のときだけ」入る（#415 / [ADR-0038](/decisions/adr-0038-workflow-initial-prompt.md)。開始宣言を会話の途中で再入力するとスキルが再起動して文脈を壊すため）。引き渡しは新しい会話ではなく同じ会話の継続なので、この規約に**従っている**。ただし「次の段は次の段の指示で走る」（§2 の `source_payload`）から `initial_prompt` まで及ぶと読まないこと — 及ばない。
- **段ごとに `agent` / `tool` を変えると、前段のセッション id が次段のツールへ渡る。** `latest_session` はどのツールが作ったセッションかを見ないので、design を A、implement を B のツールに固定した構成では B に A の resume id が渡る。プラグインが `SESSION_UNRESUMABLE` を返せば retry で救われるが、それ以外のエラーは `fail_dispatch` になる。引き渡し以前は会話の `workflow` が不変だったので到達しなかった経路である。
- **閉路検査は絡み合った群を 1 件にまとめて報告する**（§5）。1 つ直して再検証すると次が出る。`config validate` が起動ゲートなので安全側には倒れている。

## 実機で確かめたこと（2026-08-26）

**plan モードの会話は implement 権限で resume できる。** 唯一机上で保証できなかった点で、実データで確認した: 設計段のセッション `w8V:p1|5cf2c680-…` に対し、引き渡し後の実装段は `w8W:p1|5cf2c680-…` と**会話 uuid が同一**のまま dispatch され、worktree は detached（`[-]`）からブランチ（`[feat/add-titlecase]`）へ移った — **plan では禁じられていたブランチ作成が実際にできている**ので、`mode` 列の切り替えが効いたうえで resume が成立している。2 段パイプラインは設計コメント投稿 → 引き渡し → 実装 → PR 作成まで通しで完走した。

実行中の配送を台帳に書かずに見送る判断も実地で裏が取れた: 実行中に列を移した配送は無視され、段が終端になった後の poll で同じ lane 入場が**一度だけ**刻まれて引き渡された。`Failed` からの引き渡しも観測している。

**ただし逆向き（implement → design）には未解決の問題がある。** 前段がブランチへ載せた worktree を read-only profile が引き継ぐため read-only 検査が failed にし、その診断が「エージェントが git を実行した」と**誤った原因を名指しする**（判定自体は妥当）。→ [#568](https://github.com/tomoya-k31/totsuka/issues/568)

# Alternatives considered

| 案 | 却下理由 |
|---|---|
| 破棄のまま維持し、「段ごとに issue を分けろ」と案内する | spec §4.9 の例が製品の意図。例が動かない状態を仕様として固定する方向は取らない |
| 段ごとに**別の会話行**を作る（UNIQUE キーに workflow を含める） | 会話の同一性（ADR-0015 の中核）を壊す。worktree / セッションの継承・claim の保持者続投・監査の一本線が同時に失われ、Slack 側の設計とも非対称になる |
| 実行中でも引き渡す（cancel して乗り換え） | cancel は人間の決定という教義と衝突する。列移動という間接操作に、実行中のエージェントを止める力を持たせない |
| dispatch 時に「カードの現在列」を読み直してワークフローを決める | level 参照の再導入。ADR-0059 §5 が edge 判別を選んだ理由（fetch と dispatch の間の移動で非決定になる）への逆行 |
| `source_payload` は据え置き、dispatch を「最新メッセージの payload」読みに変える | `task_from_record` の呼び出し元が複数あり、Reopen 全般の意味論変更になる。この決定の射程を超える（将来の別課題として記録） |
| 引き渡しを opt-in フラグにする | 6 のとおり |
| 閉路を warn に留める | 無限の自動エージェント起動は実費が無限に出る。fail-closed が正しい |
