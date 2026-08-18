---
type: Decision
title: ADR-0010 worktree 掃除の3段化（判定→pane→worktree）と session/release の追加
description: 正常完了時に herdr pane を閉じる経路が無く pane が単調増加する問題（#210）に対し、cleanup を「判定→pane 解放→worktree 削除」の3段に分割し、protocol 0.2.1 で session/release RPC（pane_control 相乗り・expect_cwd による同一性検証・degrade-open）を追加、保持プリセット keep_7d/keep_28d と worktree sweep 間隔の分離（60s・config 非露出）を併せて導入する決定。
tags: [worktree, cleanup, pane, protocol, herdr, retention, architecture]
generated: { by: human:tomoya-k31, at: 2026-07-22T00:00:00Z }
status: stable
sources:
  - id: ref-1
    resource: https://github.com/tomoya-k31/totsuka/issues/210
    title: "Issue #210 — 本文（要件）と詳細設計コメント（実機観測・確定仕様）"
  - id: ref-2
    resource: /decisions/adr-0005-click-to-focus.md
    title: "ADR-0005 通知 click-to-focus — `pane_control` 相乗りの先行判断"
  - id: ref-3
    resource: /references/herdr-socket-api.md
    title: "herdr Socket API リファレンス — `pane.get` の `cwd`/`label`"
---

# Status

Accepted — 2026-07-22（[#210](https://github.com/tomoya-k31/totsuka/issues/210)）。
doctor による孤児 pane 検出は [#211](https://github.com/tomoya-k31/totsuka/issues/211) に分離（本 ADR の `session/release` を解放手段として再利用するため、本件マージ後に着手）。

# Context

protocol 0.2.0 の Slack 実機検証で、完了したタスクの herdr pane が残り続けることが判明した。`pane.close` に至る経路は `cancel()`（`totsuka task cancel`）と `abandon()`（dispatch 失敗、workspace ごと close）だけで、**正常完了（done）時に pane を閉じる経路が存在しない**。worktree は `plan_cleanup = "immediate"` に従って消えるのに pane の寿命は掃除ポリシーと連動しておらず、検証を繰り返すと pane が単調増加する。

さらに従来の `cleanup()` は「dirty 判定 → worktree 削除」を単一メソッドで行い pane は関与しない。ここに素朴に「pane を先に閉じる」を足すと、`DirtySkipped`（未コミット変更で削除を見送る F-23）のときに **worktree だけ残って人間が確認するための pane が失われる**最悪の状態になるため、判定の分離が必要だった。

前提として調査で確定した事実:

- `{ retention_days = N }` は既に任意日数を受け付ける（`#[serde(untagged)]`）。プリセットは純粋な糖衣にできる。
- [会話継続](/glossary/conversation-continuity.md)は worktree を再利用しない（`claude --resume` はセッションだけ引き継ぐ）ため、worktree 保持期間・pane close は会話継続に影響しない。
- プロトコルにはキャンセルなしで pane を閉じるメソッドが無い（`task/cancel` は ctrl+c 前置 + 状態機械が終端タスクへの Cancel を拒否）。
- herdr の `pane.get` は `cwd`（= worktree パス）と `label` を返すことを実機（herdr 0.7.4 / socket protocol 16）で確認済み — 同一性検証は herdr 本体無変更で実現できる。

# Decision

## 1. cleanup を「判定 → pane 解放 → worktree 削除」の3段に分割する

`WorktreeManager::cleanup()` を `decide_cleanup()`（dirty チェック + policy 判定 → `Remove | Retain | Dirty`）と `remove()`（削除の実行）に分割し、既存 `cleanup()` は両者を呼ぶ薄いラッパとして残す。Engine の `cleanup_worktree` は判定が **`Remove` のときだけ** pane を閉じ（`session/release`）、その後 worktree を削除する。`Retain` / `Dirty` では pane を保持し、人間の導線（F-23）を守る。

- **TOCTOU 対策**: `remove()` は冒頭で dirty を**再チェック**し、判定と実行の間に dirty 化していれば削除せず `DirtySkipped` を返す。pane は既に閉じた後だが、**データ損失（不可逆）> pane 喪失（軽微）**の優先順位で削除を中止し、次の sweep が再試行する。
- **release の一回性**: Engine が `released_panes` を持ち、release RPC が**正常応答した時点で**（`released` の真偽によらず）記録する。**キーは `sessions.id`（#486 で `task_id` から変更）** — 1 つのタスクは retry や追いメッセージで複数の pane を持ちうるので、task 単位のメモは「新しい pane ができたら手で無効化する」規約を要求し、その違反が無言（新 pane が二度と解放されない = 本 ADR が塞いだはずの漏れ）だった。セッション行は dispatch のたびに増えるので、このキーは自動的に無効化される。**一回性を適用するかは呼び出し側が `ReleaseMode` で選ぶ**: 掃除は `Once`（下記）、再 dispatch は `Always`（[#481](https://github.com/tomoya-k31/totsuka/issues/481)） — 後者はこれから新しい pane を開くための前提条件であり、`released: false` は「既に消えていた」と「同一性拒否で閉じなかった」の両方を意味するので、メモは pane が閉じた証拠にならない。削除が失敗し続ける worktree に対して sweep のたびに release を再送しない。transport エラー時は記録せず warn のみ（release 失敗は削除をブロックしない — pane 孤児化は #211 の doctor が受け持つ）。

## 2. protocol 0.2.1 で `session/release` を追加し、capability は `pane_control` を再利用する

`SessionReleaseParams { session_id, expect_cwd?, expect_label? }` → `SessionReleaseResult { released }`（`false` は「既に消えている / 同一性不一致で見送った」でいずれも正常）。additive change のため 0.2.0 → **0.2.1**。全同梱プラグインの manifest は上限 `<0.3` のため manifest 変更なしで受理される。

capability を新設せず **`pane_control` に相乗り**する（[ADR-0005](/decisions/adr-0005-click-to-focus.md) の `session/focus` と同じ判断）。トレードオフ: 専用 flag（例 `pane_release`）なら「focus はできるが release はできない」プラグインを表現できるが、focus も release も「この pane を制御する」ことに変わりなく、release に対応するプラグインはどのみち実装更新が要る。宣言しないプラグイン（orca / mock 既定）は単に呼ばれず、orchestrator は pane 解放をスキップして worktree だけ削除する。

## 3. 同一性検証は `expect_cwd` を主キーとし、判定規則は「不一致は拒否・比較不能は degrade-open」

herdr の pane id（`w34:p2`）は**位置ベース**で、遅延削除（7日/28日）では元の pane が閉じられ別 pane が同じ id を取る窓が開く。そこで orchestrator は DB が正本でタスク毎に一意な **worktree パスを `expect_cwd`** として送り、herdr プラグインが live pane の `cwd` と突き合わせる。

- **判定規則**: 比較可能なペア（期待値と実値が両方存在）が1つでも不一致 → 閉じずに `released: false` + warn。期待フィールドが**すべて取得できない** → 閉じる（degrade-open）+ debug ログ。
- **degrade-open の根拠**: pane_id 再利用は herdr が同 id を再発番して初めて起きる稀な事故だが、degrade-closed にすると（cwd が null を返す構成で）**全タスクで確実に pane が漏れる**。稀な事故より確実な劣化を避ける。
- **`expect_label` は送らない**（型上の拡張点として予約）。label の書式 `"totsuka {task_id}"` は herdr プラグインの実装詳細であり、orchestrator がこの書式をハードコードして期待値を組み立てるのは層違反になる。`expect_cwd` だけで同一性は成立する。
- pane が既に存在しない場合（cancel 済みタスク等）は `released: false` を返し、**workspace も閉じない**（同一性未検証のまま閉じない。cancel の「盲目クローズ」と違い release は完了から日単位で後に走り得る）。

## 4. 保持プリセット `keep_7d` / `keep_28d` を config 層の糖衣として追加し、既定値は変えない

`CleanupPolicyName` に `#[serde(rename = "keep_7d")] Keep7d` / `#[serde(rename = "keep_28d")] Keep28d` を追加（`rename_all = "snake_case"` は `keep7d` になってしまうため明示 rename 必須）。`settings_from_config` で `RetentionDays(7)` / `RetentionDays(28)` へ変換するだけで、`CleanupPolicy` と下流は何も知らない。命名は `keep_` が動作を示し、`7d`/`28d` は正確（`week`/`month` は 28日 ≠ 1ヶ月で曖昧）。

**既定値は変更しない**（implement → `manual`、plan → `immediate`）。`manual` 既定では pane が自動で閉じずタスクごとに増えるが、コミット済み未 push の作業のレビュー面として pane を残すのは妥当であり、既定変更はデータ損失リスクを伴う。[設定リファレンス](/development/config-reference.md)に「既定では pane が溜まる」旨と `keep_7d` の推奨を明記する。

## 5. worktree sweep の間隔を `SETTLE_TICK` から分離する（60s・config 非露出）

sweep は `Retained` / `DirtySkipped` の worktree 1件につき `git status --porcelain` のプロセス起動を伴い、従来は 200ms tick ごと（毎秒5回）に走っていた。`keep_7d`/`keep_28d` は**意図的に長期保持**する運用なのでこの負荷が常態化する。sweep だけを `worktree_sweep_interval`（既定 60 秒、`EngineSettings` のみでユーザー設定には露出しない）で間引く。起動直後の初回 cycle は必ず実行（起動時回収は従来どおり即時）。**正常完了パス（done 直後の cleanup）は間引き対象外**で pane close は即時のまま — 60s 粒度になるのは retention 失効と DirtySkipped 再試行のみで、日単位の判定に 60s の遅れは無意味な差。テストは `Duration::ZERO` で毎 cycle 実行に戻せる。

# Consequences

- 正常完了時、削除すると決まった worktree の pane が閉じられ、pane の寿命が worktree の掃除ポリシーに連動する。`DirtySkipped` / `Retained` / `manual` では pane が保持され、人間の導線が残る。
- ~~Cancelled タスクの sweep では `cancel()` が既に pane を閉じているため release は `released: false` を返す~~ **この前提は誤りだった（[#481](https://github.com/tomoya-k31/totsuka/issues/481)、2026-08-18 訂正）**。`totsuka task cancel` は CLI プロセスでプラグインホストを持たないので、`cancel()`（`task/cancel` RPC）は**呼ばれない**。cancel されたタスクの pane を実際に閉じるのは本 ADR の `release_pane` であり、掃引がそこへ到達するまで pane は生きている。その窓の中で `task retry` すると生きた pane の上へ dispatch し、agent 名が衝突して retry が即 failed になっていた。修正は `dispatch_one` が前回セッションの pane を dispatch 前に `session/release` することで、同一性ガード（`expect_cwd`）も冪等性も本 ADR の設計をそのまま使う。なお pane 消失時に何も閉じず `released: false` を返す挙動自体は正しく、herdr プラグインの fake transport テストで固定されている。
- **決定 1 の「`Retain` / `Dirty` では pane を保持する」には例外が 1 つある（#481、2026-08-18）**: そのタスクを**もう一度 dispatch すると決めた**とき、`dispatch_one` は保持中の pane も `session/release` で閉じる。「レビュー面として残す」という判断はタスクが終わったまま放置される局面のもので、同じ worktree に新しいエージェントを入れると決めた瞬間に前提が変わる — 残せば古い pane は同じ作業ディレクトリを指す幽霊になり、しかも task id から agent 名を導くプラグイン（herdr）では新しい dispatch 自体が名前衝突で拒否される。既定 `manual` の implement タスクはまさにこの状態（`Retain` のまま pane が生き続ける）なので、例外を置かないと既定構成で retry が直らない。
- orca は `pane_control` 非宣言のため release は呼ばれず、従来どおり worktree だけ削除される（変更なし）。
- pane を閉じても Claude セッションは消えないため、`claude --resume` による[会話継続](/glossary/conversation-continuity.md)は影響を受けない。
- release の transport 失敗や degrade 判定で pane が残るケースは残存する — 孤児 pane の検出・解放は #211（doctor）が `session/release` を再利用して担う。
