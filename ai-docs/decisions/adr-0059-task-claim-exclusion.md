---
type: Decision
title: ADR-0059 多人数 poll の二重着手は Issue self-assign の claim と AssignedEvent 先着裁定で防ぎ、スイムレーン差し戻しで再実行する
description: "複数メンバーが同じ GitHub Project を poll する構成の二重着手対策。dispatch 直前にコアが task/claim（protocol 0.6.1、capability 宣言制）でソースプラグインへ占有を要求し、github プラグインは Issue への self-assign と AssignedEvent の createdAt 先着で裁定する。敗北は新終端状態 Skipped、claim 黙殺（Forbidden）は書き戻し無しの Fail。on_start を新設し勝者確定後に Status を動かす。人間がカードをトリガー列へ差し戻したときの再実行は status セルの updatedAt を message_key に刻んで #242 の会話再開に乗せる。Status LWW 単独・git ref CAS・単一ディスパッチャ・ハッシュ裁定は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/556
tags: [decision, github, task-source, protocol, dispatch, concurrency, adr]
generated: { by: claude-code/opus-5, at: 2026-08-26T15:00:00+09:00 }
verified:
  - { by: human:tomoya-k31, at: 2026-08-26T12:15:00Z }
sources:
  - { resource: "https://docs.github.com/en/rest/issues/assignees" }
  - { resource: "https://github.com/tomoya-k31/totsuka/issues/556#issuecomment-5409966837" }
status: stable
owner: tomoya-k31
---

# Status

stable。設計・Phase 0 実測（2026-08-25）・実装の全 6 PR・**実機検収（2026-08-26）**まで完了。検収では claim が `on_start` に 3 秒先行すること、lane 差し戻しが会話の 2 通目として記録されて reopen すること、reopen 後の claim が pre-read fast-path で書き込みゼロの Won になることを実データで確認した。

決定の全文と経緯は [#556](https://github.com/tomoya-k31/totsuka/issues/556)（本文 = 決定 10 件と不採用案、コメント = 詳細設計・reopen 追補・実測結果）。この ADR はその確定部分を記録する。

# Context

F-08 は「複数人利用時の取り込み確認・制御はタスクソースプラグインの役割（厳密な排他制御までは不要）」と定め、github プラグインは**読み取り側のゲートだけ**を実装してきた: 他者 assignee の除外（`assignable_to_me`）・`in_progress_statuses` の除外・ボード紐付けの除外。書き込み側の claim は存在しないため、**未アサインのタスクを複数メンバーの totsuka が同時に拾うと二重着手する** — 同じ issue に worktree が 2 つ切られ、PR が 2 本出る。取り返しはつくが token・レビュー・コンフリクト解消が丸ごと無駄になる。

前提の整理で 2 つの誤解が解けた:

1. **「完全な排他制御はできない」は誤り。** GitHub には第 2 書き込み者を落とす真の CAS がある（`POST /git/refs` の ref 重複、Contents API の sha 無し作成）。無いのは **ProjectsV2 の上**だけ — `UpdateProjectV2ItemFieldValueInput` は事前条件フィールドを一切持たず無条件 last-write-wins（GraphQL 内省で確認）。よって選択は「妥協するか」ではなく「どこに副作用を置くか」。
2. **claim-then-verify の窓は、順序付きログで裁定すれば「読んだ順」に依存しない。** 追加集合（assignee）には順序が無いので集合だけでは先着を決められないが、`AssignedEvent`（`actor` / `assignee` / `createdAt` / `id`）というサーバー側の全順序ログがあり、全員が同じ結論に収束できる。残る穴は読み取りの結果整合性だけ。

# Decision

## 1. claim = Issue への self-assign、裁定 = AssignedEvent の先着

`addAssigneesToAssignable` は**追加であって置換ではない**（docs 明記・実測済み）ので、レースは assignee 集合の重複として観測できる。裁定は「現 assignee ごとに最新の AssignedEvent を取り、その createdAt が最古の者が勝ち。同時刻は event node id の辞書順最小」— サーバー発行の全順序なので決定的。**actor ではなく assignee の login で判定**（トークン名義と `github_login` のズレに耐える）。敗者は自分の assignee を外して降りる（他人の assignee には触らない）。

pre-read で**既に自分が assignee なら書き込まずに Won**。この 1 規則が、人間による事前アサイン（自分＋レビュアー等の複数人アサインを含む）・過去の自分の claim・retry を吸収する — 裁定は自動 claim 同士の対称レースを破る道具であり、人間の意図に適用しない。

## 2. コアが「いつ」、プラグインが「どう」

`task/claim`（protocol 0.6.1、O→P）を新設し、コアは **dispatch 直前**（`dispatch_one` の対象解決直後・最初の副作用の前）に呼ぶ。fetch 時に claim すると trigger 一致の全件を assign してしまい、並列上限で `Queued` に滞留した分が「自分にアサイン済みだが動かない」= 他の全員から見えないまま塩漬けになる（元の問題より悪い）。capability `task_claim` を宣言したソースにだけ送り、未宣言（Slack / Notion / 旧版）は素通り = 従来挙動。結果は `won` / `lost` / `forbidden` の 3 値で、**一時障害は variant ではなく JSON-RPC error**（コアは Queued のまま次 cycle 再試行）。

## 3. 敗北は新終端状態 Skipped、黙殺は書き戻し無しの Fail

- `lost` → `TaskEvent::Skip` → **`TaskState::Skipped`**（新設・terminal）。`Failed` にしないのは `write_back_status(false)` が**他人が実行中のタスクのボード列を動かす**から。`Cancelled` にしないのは「人間がキャンセルした」という嘘が本物と区別できなくなるから。復帰は `totsuka task retry`（Skipped からの Retry / Reopen 遷移を許可）と、§5 の差し戻し。
- `forbidden`（読み戻しに自分が現れない = GitHub が黙って無視した。push 権限の無い assignee は **200 のまま黙殺**される）→ 書き戻しを**迂回して** Fail + 通知。通常の `fail_dispatch` を通すと、誰も保持していないタスクの列を `on_failure` が動かし、他メンバーの trigger からタスクが消える。
- assignee を外すのは**負けたときだけ**。失敗時に外すと「A が失敗→外す→B が拾う→同じ理由で失敗」がチームを巡回する（dispatch が Failed を自動再キューしない理由と同型）。

## 4. `on_start` の新設

`[[workflows]].on_start = { set_status = "…" }`（`on_success` / `on_failure` と同形の `Option`、**未設定なら何も書かない** = 既存設定の挙動は不変）。発火は claim 勝利の直後（勝者確定後なので race 無し）で、`in_progress_statuses` が初めて自動で効く第 2 の防御線になる。`on_start` を使うなら `on_failure` も設定すること（失敗時に列が「実装中」のまま残るため）。

## 5. スイムレーン差し戻しによる再実行 — edge を message_key に刻む

「人間がカードをトリガー列へ戻したら再実行してよい」を、**level（いまどの列か）ではなく edge（いつ入ったか）**で判別する。ProjectsV2 の status セルは値オブジェクトごとに `updatedAt` を持つ（実測: 列移動で進み、**同一 option への冪等再セットでは進まない**）ので、github プラグインが `message_key = "status:{列名}@{updatedAt}"` を刻めば [#242](/decisions/adr-0015-conversation-task-identity.md) の会話再開機構にそのまま乗る — コアの reopen 経路は新設ゼロ。

- 定常の毎 tick 再配送・完了直前の古い fetch スナップショットの遅延配送 → 古い updatedAt = 台帳に既在 → Duplicate。**サーバー発行タイムスタンプ同士の等値比較だけ**で race が閉じる（ローカル `finished_at` との大小比較はクロックスキューで誤るため不採用）
- 差し戻し → 新しい updatedAt → terminal 行 → Reopened → claim をやり直して再実行。assignee の付け替えで**誰が**再実行するかを人間が指定できる
- `project_status` トリガーを持つ workflow だけが対象（label-only は「列」の概念が無く、任意の列移動で誤爆するため従来どおり at-most-once）

**必須ガード**: 同一 issue は `UNIQUE(source, source_task_id)` で 1 行・行の workflow は初回から不変なので、別ワークフローからの配送をそのまま受けると「旧ワークフローでの誤 reopen」に化ける。当初は破棄で塞いだが、**[#565](https://github.com/tomoya-k31/totsuka/issues/565) が引き渡し（handoff）へ置き換えた** — terminal な会話は配送元のワークフローへ移り、実行中の会話だけが（台帳に書かずに）見送られる。**新 validate エラー**: `set_status` が自分の `trigger.project_status` と一致する workflow は拒否（on_start で列外へ出た後に書き戻しで戻ると無限 reopen ループ）。**#565 でこれは列グラフの閉路検出へ一般化された**（自己ループはその長さ 1 の場合）。

## 6. 前提と制約

- **1 login = 1 インスタンス。** assignee は login しか運べず `AssignedEvent.actor` も同一になるため、同じ login の複数 totsuka は原理的に裁定できない（非対応と明記）。
- 裁定で現 assignee のイベントが timeline に見えないときは**エラーで返して次 cycle 再挑戦**（安全側 = 降りる、だと相互不可視で両者が降り、誰にも拾われないタスクが生まれる。エラー化でデッドロックが遅延に化ける）。
- `claim_verify_delay_ms` の既定は **750ms**（実測 p95 ≈ 700ms / max 983ms）。
- アップグレード時、旧台帳の key は task.id なので「トリガー列に置き去りの完了カード」が初回 poll で 1 回 reopen する（`on_success` で列外へ出す運用なら影響ゼロ。リリースノート行き）。

# Alternatives considered

| 案 | 却下理由 |
|---|---|
| Project Status の LWW ＋ `creator` 読み戻しを claim にする | 人間から「誰が担当か」が読めない。`creator` の意味論（最後の書き手か）は単一アカウントでは実測不能のまま。ただし勝者確定後の Status 書きは §4 の `on_start` として採用 |
| git ref 作成を lock にする（`refs/totsuka/claim/<issue>`） | 窓が真にゼロになる唯一の案だが、`contents:write` の全 repo 拡大・TTL 無しでクラッシュ時に永久残留・ボードから人間に見えない。被害が可逆（二重 PR）な問題に釣り合わない |
| 人間の事前アサインを必須にする運用規約 | 「手が空いた totsuka が拾う」という価値の中核を殺す |
| 単一ディスパッチャ | race は原理的に消えるが SPOF + 配分ロジックの新規実装 |
| `hash(issue_id + login)` 最小値の勝ち | 既に走っている先行者を後発がハッシュで押しのける。「走っている方が勝ち」を別途作ることになる |
| 他者がいたら無条件で降りる | 相互に降りて誰も着手せず、2 人分の assignee が残って以後誰の poll にも拾われない死にタスクになる |
| fetch 時にプラグインが claim（protocol 変更ゼロ） | `Queued` 滞留分の過剰 claim（§2） |
| 全 task_source に claim を必須化（0.7 破壊的変更） | Slack は自分宛メンションで競合せず、常に won を返す無意味な実装を強いる |
| reopen を `finished_at` と updatedAt の大小比較で判定 | ローカル時計と GitHub 時計のスキューで静かに誤判定。採用案は等値比較のみ |
| プラグイン側 seen-set で列移動の edge 検出 | プロセス内メモリは再起動で消える。「dedup は orchestrator の仕事、プラグインに seen-set を持たない」という既存方針にも反する |

# Consequences

- protocol は 0.6.1（追加的 patch）。`task/claim` + `Capabilities.task_claim`、SDK はデフォルト実装付きで既存ハンドラ無改修。同梱 manifest は `>=0.6.0, <0.7` のまま
- github プラグインの「Issue へは何も書かない」（#398）は**撤回**される: `addAssigneesToAssignable` / `removeAssigneesFromAssignable` / timeline 読みが増え、GraphQL 操作は 4 → 7。トークン権限表も更新（実測は OAuth `gho_` のみ = 実運用と同種別。fine-grained PAT は user 所有 board を読めないため対象外）
- `TaskState` に `Skipped` が増える（DB マイグレーション不要・TEXT。**旧バイナリは `"skipped"` 行を読めない** — 0.x の downgrade 制約としてリリースノートに記載）。worktree sweep の対象に Skipped を追加（retry 中に負けた行が worktree を持ち得る）
- capability 未宣言による無言スキップは**受容した残存リスク**（発火点はプラグイン再インストール。doctor 検査は作らないと決めた）。読み取りの結果整合性の窓・クラッシュした勝者の assignee 残留も同様（issue 本文の残存リスク 11〜13）
- Phase 0 の probe は `.claude/skills/live-e2e/scripts/github-claim-probe.sh` として恒久化（トークン種別を差し替えて再測定できる）
