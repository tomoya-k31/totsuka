---
type: Decision
title: ADR-0062 status は trigger と on_* で同じ綴りにし、status_map を廃止する
description: "同じ状態列を指す 2 つのキーが別名（trigger.project_status / on_*.set_status）で、しかも片方だけが status_map の写像を通っていた問題への決定。両側を status に統一し、写像表を廃止して全キーがボードの option 名を生で指すようにする。あわせて trigger 内の status を core 予約キーとして正式化する。式言語・両側 project_status・on_* のスカラー化・in_progress_statuses などの改名は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/575
tags: [decision, config, workflow, trigger, status, breaking, adr]
generated: { by: claude-code/opus-5, at: 2026-08-27T03:00:00+09:00 }
verified:
  - { by: human:tomoya-k31, at: 2026-08-26T20:11:00Z }
status: stable
owner: tomoya-k31
---

# Status

stable。実装・**実機検収（2026-08-27）**まで完了。

検収は「旧綴りが無言にならないこと」と「新綴りが実際に書き戻すこと」の両方を見た。実運用と同形の e2e 設定（`[[workflows]]` 5 本、うち github 2 本）を旧綴りのまま `totsuka config validate --offline` に掛けると、`on_*` 側の 3 箇所を**すべて**名指しして exit 1 で止まる。`on_*` だけ新綴りへ直して `totsuka run` を起動すると、今度は github プラグインが `initialize` を `-32003` で落とし、`project_status` を名指しして有効キー 3 つ（`assignee` / `label` / `status`）を列挙する —— **プロセス全体が起動しない**（`launch_plugins` の `?` が伝播する）。完全移行後は `on_start = { status = "In Progress" }` と `on_success = { status = "Done" }` の**両方**が実際に ProjectsV2 のカードを動かした。

[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md) の「Orchestrator は `trigger` の中身を一切解釈しない」を**この 1 点について改訂する**。ADR-0058 は全体としては有効で、`deprecated` にはしない。

# Context

`[[workflows]].trigger.project_status` と `on_*.set_status` は**同じ列**（github なら `[github].status_field` の SingleSelect、既定 `Status`）を指していた。名前が違うだけなら読みにくいだけだが、**値の解釈経路まで違っていた**。

列の値を名指すキーは 4 つあり、写像を通るのは 1 つだけだった:

| キー | 向き | 値の経路 |
|---|---|---|
| `trigger.project_status` | 読む | 生 |
| `in_progress_statuses` | 読む | 生 |
| `[[projects]].triage_status` | 書く（指示文経由） | 生 |
| `on_*.set_status` | 書く | **`status_map` 経由** |

結果、同じ文字列が方向によって違う意味になった:

```toml
[github]
status_map = { done = "🚧 最終レビュー" }

on_success = { set_status = "done" }      # → 「🚧 最終レビュー」が書かれる
trigger = { project_status = "done" }     # → 翻訳されない。存在しない列を探して永久に一致しない
```

書き込みでは通る文字列が、読み取りでは黙って何も起こさない。エラーも警告も出ない。

## `status_map` は用を成していなかった

説明は「オーケストレータ側のステータス名 → Project のオプション名」だが、**書き戻される値の出どころは設定の `set_status` 文字列ただ 1 つ**で、core はそれを一切加工せずプラグインへ渡していた（`run/finalize.rs` の `write_back_status` が唯一の呼び出し元）。「オーケストレータ側のステータス名」という独立した語彙は存在しない。

つまり **運用者が書いた文字列を、運用者が書いた表で、運用者が書いた別の文字列へ変換する**二重間接だった。既定は空（恒等）で、実運用の config でも使われていなかった。

間接層としても**半分しか効かない**。ボードの列名を変えたら `trigger` も `in_progress_statuses` も `triage_status` も結局書き換えるので、「1 箇所で吸収できる」という擁護が成立しない。

## 閉路検査に盲点を作っていた

`column_cycles`（#565 の無限ループ検査）の doc コメント自身が書いていた:

> a plugin-side `status_map` that aliases two names onto one column is out of its sight

上の例で `on_success = { set_status = "done" }` と `trigger = { project_status = "🚧 最終レビュー" }` を並べると、検査は `"done"` を見て**閉路なしと判定する**のに、実際の書き込みは 🚧 最終レビュー に着地して本当にループする。毎周エージェントが起動し、実際にトークンを消費する。

## notion では閉路検査そのものが効いていなかった

notion の trigger は `status` を第一名、`project_status` を別名として受けていた。しかし core の `trigger_column` は `project_status` しか読まない。**`trigger = { status = "..." }` と書いた notion のワークフローは、閉路を作っても `config validate` を素通りしていた。**

# Decision

## 1. 綴りを両側 `status` に統一する

```toml
[[workflows]]
name       = "github-implement"
source     = "github"
trigger    = { status = "🤖 実装・受入検証" }
profile    = "implement"
on_start   = { status = "🚧 実装中" }
on_success = { status = "🚧 最終レビュー" }
```

- **`project_` を落とす**。GitHub Projects の語であり、notion にも slack にも出ていた。`reaction` は Slack の語なので core の語彙に持ち込まない、という #554 の判断と整合する。
- **`set_` を落とす**。`on_*` を「代入の集合」と読む。`trigger` と語彙が完全に対称になり、将来 label や assignee の書き戻しを足すときに `set_label` のような二重の語彙が要らない。非代入的な動作（`result/publish`）は**すでに `on_*` の外**（`output` キー）に住んでおり、`on_*` は F-84 の状態遷移専用の表である。
- **notion の `project_status` 別名は廃止**する。

## 2. `status_map` を廃止する（github / notion 両方）

列の値を名指す全キーが「ボードの option 名を生で書く」に揃う。閉路検査の盲点も同時に消える。

## 3. `status` を core 予約キーとして正式化する

> `status` は **`trigger` テーブルに住む core 所有のキー**である。意味（ソース側の状態列の値）と型は core が持ち、閉路検査の列グラフに使う。**受理するかは各ソースが決める** —— 状態列を持たない slack は未知キーとして拒否してよい。

core がこのキーを読むのは `column_cycles` の入口を取る 1 用途だけで、**文字列の比較しかしない**（doc コメントいわく "Lexical only"）。ボードへの問い合わせも解決もしないので、`config validate --offline` でも動く。

## 4. スキーマ `version` は上げない

#554 と同じ線を採る。**#574 の未知キー検査が入っているので、旧綴りは 3 つとも硬いエラーになる**:

| 破壊 | #574 なし | #574 あり |
|---|---|---|
| `trigger.project_status` → `status` | 黙って条件なし = 全件マッチ | `initialize` の硬い失敗 |
| `on_*.set_status` → `status` | 黙って書き戻し停止 | 起動時エラー |
| `status_map` 削除 | — | `deny_unknown_fields` で硬いエラー |

`version` を上げると「移行方式」と「`version` 省略時の既定」の 2 決定（config-reference のバージョニング方針）を先に片づける義務が付いてくる。それに見合う対価が無い。

# Consequences

- **既存 config は起動しない。** 旧綴りは全部エラーになり、メッセージが有効キーを列挙する。移行シムは書かない（#554 と同じ理由 —— 維持する相手がいない）。
- **notion のワークフローが閉路検査の対象になった。** 回帰テスト `a_notion_column_cycle_is_caught_by_the_same_walk` が固定している。
- **論理名でボードの列名を吸収する手段が無くなった。** ボードの列名を変えたら、その列を名指す設定を全部書き換える。`status_map` があってもどのみち 3/4 のキーは書き換えていたので、失うのは「1/4 だけ吸収できる」という半端な性質である。
- **`status` を予約したので、将来のソース（jira 等）もこの名前を使う義務を負う。** 見返りに、新しいソースでも閉路検査が自動で効く。
- 検査は `#574` と対になっている。`trigger` はプラグインが、`on_*` は core が、それぞれ未知キーを拒否する。

# Alternatives considered

- **`status_map` を残して 4 キー全部に適用する**: 論理名で全部書けて列名変更を 1 箇所で吸収できるが、設定を読むたびに写像表を引くことになる。今それで困っている運用者がいない。
- **両側 `project_status` に揃える**: GitHub Projects の語を core 所有の `on_*` にまで広げることになり、notion / slack にもその名前が出る。
- **`on_*` をスカラーにする**（`on_success = "🚧 最終レビュー"`）: F-84 の表は状態列専用なので最も短いが、将来の拡張余地を捨てる（拡張時に再び破壊的変更が要る）。
- **式言語**（`status:todo && assignees in (...)`）: パーサ・検証・エラーメッセージの維持コストに見合わない。キーの AND で足りる。
- **core が trigger を読むのをやめ、プラグインに監視列を申告させる**: 所有は澄むが、閉路検査がプラグイン往復の後ろへ移り `config validate --offline` を失う。
- **`in_progress_statuses` / `triage_status` / `status_field` も改名する**: いずれも既に生で意味も明確。壊れる面が増えるだけ。

# 関連

- [ADR-0058 設定の所有境界](/decisions/adr-0058-config-ownership-boundary.md) —— この ADR が 1 点を改訂する
- [ADR-0061 列パイプラインの段間 handoff](/decisions/adr-0061-workflow-handoff.md) —— 閉路検査を入れた決定
- [ADR-0059 claim による二重着手防止](/decisions/adr-0059-task-claim-exclusion.md)
