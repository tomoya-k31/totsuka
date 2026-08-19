---
type: Decision
title: ADR-0052 プロトコルの宣言は「誰かが読んでいる」ことを機械検証する（protocol 0.5.0）
description: "Capabilities のフィールドと error_code の定数が実際に消費されていることを arch-lint で検査し、それに引っかかった 5 件を削除する決定。resume_session は hook_completion へ置き換え、diagnostics_snapshot は実 RPC をゲートするため独立のまま残す。ワイヤは壊れないが型は壊れるので protocol を 0.5.0 へ。"
resource: https://github.com/tomoya-k31/totsuka/issues/496
tags: [decision, plugin, protocol, fitness-function, arch-lint, adr]
generated: { by: claude-code/opus-5, at: 2026-08-20T00:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: issue-496
    resource: https://github.com/tomoya-k31/totsuka/issues/496
    title: "refactor(protocol)!: 到達不能な宣言 5 本を削除し、宣言が読まれていることを arch-lint で機械検証する"
  - id: adr-0011
    resource: /decisions/adr-0011-arch-fitness-function.md
    title: "ADR-0011 ワークスペース依存境界の Fitness Function"
  - id: adr-0034
    resource: /decisions/adr-0034-protocol-0-4-0-removals.md
    title: "ADR-0034 protocol 0.4.0 の削除（design_preview / hook）"
---

# Status

stable。実装は #496（本 ADR と同一 PR）。**実機検収済み**（2026-08-20）— F-54 の境界が両方向で効いた。`<0.5` を宣言したままの installed プラグインは protocol 0.5.0 に拒否され（`it supports ">=0.2.3, <0.5" but the orchestrator is 0.5.0`）、`<0.6` へ再インストール後は`launches and accepts its config` になった。`hook_completion` 経由の dispatch も実タスクで通っている。

# Context

`plugin-protocol` は契約であって実装ではない。そこに宣言があるのに誰も読んでいないなら、それは**プラグイン作者に「効く」と信じさせて何もしない鍵**である。

grep で数えたところ、そういう宣言が **5 件**あった。

| 宣言 | 本体コードからの参照 | なぜ死んでいたか |
|---|---|---|
| `Capabilities::plan_mode` | 0 | plan / implement の分岐は `ExecutionMode` と workflow profile が担っており、この宣言は経路に存在しない |
| `Capabilities::task_submit` | 0 | 0.2.0 で `tasks/fetch` が消えた時点で情報量ゼロ。起動できる task_source は例外なく push 型なので、`true` 以外を取り得ない |
| `Capabilities::resume_session` | 0（単独では） | dispatch の resume 判定は**プラグインではなくツール側の** `tool_caps.resume` を見る。唯一の役割は `hook_capable()` の OR 項 |
| `error_code::PROTOCOL_VERSION_MISMATCH` | 0 | 互換検査は spawn の**前**にホスト側で終わる。プラグインがこれを返す機会は原理的に無い |
| `error_code::CAPABILITY_UNSUPPORTED` | 0 | ホストは宣言された capability しか呼ばない。到達不能 |

**これは再発である。** `Capabilities::design_preview` はまったく同じ形で、誰にも読まれないまま 1 世代残った（#356 → #411 で削除、[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）。0.2.3 で「次の破壊的バンプで削除」と宣言した `TaskDispatchParams.hook` も、実際に消えるまで丸 1 世代かかった。

**そして最悪なのは、ドキュメントが嘘を教えていたこと。** `plugin-dev-guide` は `plan_mode` と `task_submit` を意味のある宣言として例示し、`task_submit` は「必須」とまで書いていた。プラグイン作者はこれを読んで宣言し、何も起きない。

# Decision Drivers

* このリポジトリは「宣言だけの猶予」が守られないことを**既に 2 回実証している**。3 回目を人間の注意力で防ぐ計画には根拠がない
* 依存境界には既に fitness function がある（[ADR-0011](/decisions/adr-0011-arch-fitness-function.md)）。同じ形の検査を、同じスクリプトに足せる
* ワイヤを壊さずに削除できる（`Capabilities` に `deny_unknown_fields` が無い）

# Options Considered

1. **何もしない。** 5 件はそのまま残り、`plugin-dev-guide` は嘘を教え続ける
2. **削除だけする。** カウンタをリセットするだけで、次の 1 本が生えるのを止めない
3. **検査だけ入れて削除しない。** 5 件すべてを免除リストに載せることになり、免除リストの意味が最初から失われる
4. **検査を入れ、それに引っかかった 5 件を削除する（採用）**

# Decision

選択肢 4。**削除は結果であって主眼ではない。**

## 1. `declaration-consumed` 検査（`scripts/arch-lint.sh`）

`Capabilities` の各フィールドと `error_code` の各定数について、消費者が 1 件も無ければ **エラーで落とす**。既存の `plugin-bin-name` 検査と同じ形（宣言的許可リスト + フェイルクローズ）で、CI の `clippy / rustfmt` ジョブ内で走る。

探索範囲が 2 種類で違うのは、**宣言の向きが違うため**である。

* **`Capabilities` のフィールド**は「プラグインが宣言し、Orchestrator が読む」。プラグイン側は値を**立てる**だけで消費者ではないので、`orchestrator-core` / `orchestrator-cli` の `src` だけを見る。ここを広げると、herdr が `plan_mode: true` と書いているだけで「消費されている」ことになってしまい、検査が自分の目的を打ち消す
* **`error_code` の定数**は両側が発行し照合するので、`plugin-protocol` 自身を除くすべてを見る

どちらも `mock_plugin` と `tests/` は消費者に数えない。**テストダブルが立てているだけの宣言は、本番では誰も読んでいないのと同じ**である。フィールドは**アクセス**（`.field`）だけを数え、初期化（`field: true`）は宣言なので数えない。コメント行も数えない — 「`.plan_mode` を読む」と書いた doc comment があるだけで、読まれていないフィールドが生きて見えてしまう。

## 2. 免除リストが検査の主眼である

`DECLARATION_EXEMPT` に `<name>=<理由>` を書けば通る。**このリストの存在こそが目的**で、放置された宣言と、意図した「実装より先の宣言」とを区別できるようにするためにある。理由なしで足さない。

## 3. `resume_session` は削除ではなく `hook_completion` への置換

旧設計は専用フラグを持たず、`resume_session || diagnostics_snapshot` という **de-facto のシグナル**をフック対応の判定に使っていた（`hook_capable()`）。どちらのフラグも「hook」とは言っていないので、プラグイン作者は文書化されていない規約を知らないと opt-in できず、逆に「セッション再開はできるがフックは話さない」プラグインには**それを言う手段が無かった**。

**`diagnostics_snapshot` は統合しない。** これは `diagnostics/snapshot` という実在の RPC をゲートしており、`hook_completion` に畳むと「フックで完了を報告する agent は必ずスナップショットも返せる」という**新しい要求を暗黙に課す**ことになる。herdr は両方宣言しているので今は通るが、将来の agent プラグインが理由なく縛られる。

## 4. 退役した番号は再利用しない

`-32001` / `-32002` は削除するが、番号は空けたまま残す。到達不能だったとはいえ、外部プラグインが定義を見て条件分岐を書いている可能性を否定できない。

# Consequences

## 良くなること

* 「宣言したのに効かない」が **CI で止まる**。人間のレビューが見落としても止まる
* `plugin-dev-guide` が教える内容と、Orchestrator が実際に読むものが一致した
* `hook_completion` は名前が意味と一致しているので、プラグイン作者は規約を知らなくても opt-in できる

## 悪くなること・注意点

* **`<0.5` を上限とする manifest はすべて起動拒否される**（F-54 の設計どおり）。0.2.0 / 0.3.0 / 0.4.0 で 3 回実績のある手順で、バンドルプラグインは同一 PR で `<0.6` へ上げた
* **ワイヤは壊れない。** `Capabilities` に `deny_unknown_fields` が無いので、古い manifest の `plan_mode = true` は読み飛ばされる。壊れるのは**フィールドを読むコード**で、これは型の破壊であり、だからバージョンが動いた
* **検査は「読んでいるか」しか見ない。** 読んだ結果が正しいかは見ないし、フィールドを 1 度読んで捨てているコードも通る。ここは PR レビューが引き受ける
* **検査はレシーバを見ないので、同名の別物を消費と数えうる。** 実際、当初は `self.diagnostics_snapshot(record)` という**メソッド呼び出し**（`run/hooks.rs`）が capability フィールドの読みとして数えられており、本物の読みを消しても検査が通る状態だった。メソッド呼び出しは「`(` が続かないこと」で除外したが、**同名の「フィールド」を持つ別の型は今も区別できない**。現に `ToolCapabilities` は `plan_mode` というフィールドを持つ（今は初期化されるだけで読まれていないので衝突していない）。レシーバ側で絞る案は採らなかった — `caps` / `capabilities()` / `m.capabilities` と呼び名が一定せず、許可した綴りの外にある読みを取りこぼすほうが危ないため。**新しい capability の名前を決めるときは、`orchestrator-core` / `orchestrator-cli` に同名フィールドが無いか確かめること**

# 検証

検査は**削除より先に**書き、5 件を正しく検出することを確認してから削除した（`plan_mode` / `resume_session` / `task_submit` / `PROTOCOL_VERSION_MISMATCH` / `CAPABILITY_UNSUPPORTED` — 監査で grep により特定した集合と完全一致）。削除後は 0 error。

新規に死んだ宣言を足しても捕まることも実測した（`sparkle_mode` という読み手の無いフィールドを追加 → 1 error）。**5 件を消せることだけを確認して終わると、検査が「今の状態」にしか効かない可能性を残す。**

**境界が効いていることも別に確かめた**: `pane` という名前のフィールドを足すと、`.pane_control` という消費がすでに存在していても正しく「読まれていない」と報告される。マッチが緩ければここを取りこぼし、検査は**通っているのに何も守っていない**状態になる。この境界は `\b`（GNU/BSD の拡張で POSIX ではない）ではなく明示的な文字クラスで書いてある — 今日はどちらの grep でも動くが、Fitness Function が拡張に寄りかかるべきではない。仮に壊れても**フェイルクローズ**する（何もマッチしなくなり、全宣言が「読まれていない」として赤くなる）。
