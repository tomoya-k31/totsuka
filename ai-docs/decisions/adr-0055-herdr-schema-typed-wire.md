---
type: Decision
title: ADR-0055 herdr Socket API を下限版の schema から生成した型で受け、互換を CI で機械検査する
description: "herdr のレスポンスを serde 型で受け、互換を CI の schema 差分で機械検査する決定。型は下限版（0.7.5）のスライス済み schema から 1 組だけ生成し、版ごとの分岐は作らない。protocol 整数は互換の信号として使わず version の semver 判定へ置き換える。実行時は寛容（追加を無視）・CI は厳格（削除と required 追加で落とす）。最新版から生成する案・未知メソッドを試す案・実行時に schema を読む案は却下。"
tags: [decision, herdr, socket-api, schema, codegen, compatibility, ci, adr]
generated: { by: claude-code/opus-5, at: 2026-08-23T00:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0032
    resource: /decisions/adr-0032-herdr-protocol-17.md
    title: "ADR-0032 herdr protocol 17 へ追随する"
  - id: herdr-socket-api
    resource: /references/herdr-socket-api.md
    title: "herdr Socket API"
  - id: agent-ide-herdr
    resource: /components/agent-ide-herdr.md
    title: "agent-ide-herdr プラグイン"
---

# Status

stable。実装は #520 §1（下限の semver 化）と #518（スライス・型生成・CI 検査）。
型の消費は #519、日次 cron と上限側の通知は #520 §2〜§3。

# Context

totsuka を使うユーザごとに herdr のバージョンが違う。**新しい herdr が出たときに
totsuka が起動しなくなるのは設計上あってはならず、逆に古すぎる herdr には
バージョンアップを促したい。** つまり必要なのは「幅を持ったサポート」であって、
1 点への追随ではない。

herdr の stable は**約 2 週間ごと**に出る（0.7.4: 2026-07-15 → 0.7.5: 07-21 →
0.8.0: 08-03 → 0.8.2: 08-19）。追随を人手の観察に頼ると、この間隔では必ず落とす。

## 静かに壊れる側にしか倒れない結合

totsuka は herdr に **Unix ソケット上の NDJSON Socket API 一本**で結合しており、
呼んでいるのは 22 メソッド。問題は受け側で、**herdr のレスポンスに対応する
serde 構造体が 1 つも無かった**。すべて `serde_json::Value` を
`.get("...").and_then(Value::as_str)` で手掘りしている。

その結果、未知フィールドが自動で無視される（前方互換は無料）代わりに、裏返しで:

- herdr がフィールドを**消しても改名しても totsuka は落ちない**。`None` から
  既定値へ落ちて進む（`agent_status` 欠落 → `"unknown"`、未知バリアント →
  `_ => previous`、未知イベント → `_ => ExitSignal::Ignore`）
- ハードエラーになるのは `workspace.create` の `workspace_id` 欠落と
  `root_pane` 欠落の 2 つだけ

テストダブル `FakeHerdr` は herdr の応答を**手書き**しているので、実 herdr と
乖離してもテストは緑のまま通る。

## `protocol` 整数は守りたいものを追跡していない

移行前のガードは `MIN_HERDR_PROTOCOL = 17`。`ping` の `protocol` が 17 未満なら
起動を拒否していた。**この `protocol` は herdr のバイナリ client↔server wire
形式の版**（herdr repo の `src/protocol/wire.rs`）で、totsuka が使う NDJSON
Socket API の版ではない。5 版を実測すると、**両方向に外れている**:

| 遷移 | protocol | totsuka が使う NDJSON API の実変化 |
|---|---|---|
| 0.7.2 → 0.7.4 | 16 → **16** | **`custom_status` 削除**（`PaneInfo` / `AgentInfo`）+ メソッド 5 追加 |
| 0.7.4 → 0.7.5 | 16 → 17 | **`agent.send` 削除** → `agent.prompt` / `agent.wait` 他 5 追加 |
| 0.7.5 → 0.8.0 | 17 → 19 | メソッド +1（`workspace.move_block`）のみ |
| 0.8.0 → 0.8.2 | 19 → 20 | メソッド +1（`pane.input.set`）のみ |

- **上がっても壊れていない**: 17 → 20 の 3 回の bump で、22 メソッドの request
  形状の変更 0 件・result 型の削除 0 件・`required` の追加 0 件
- **上がらずに壊れた**: `custom_status` の削除は protocol 16 → 16 で起きた

スキーマ自身の版 `schema_version` も 5 版を通して `1` のままで、これも信号に
ならない（スキーマの*形式*の版であって内容の版ではない）。

つまり、**下限を課したい対象を追跡していない数値では下限を表現できない**。

# Decision

**実行時は寛容（追加を無視）、CI は厳格（削除・`required` 追加で落とす）。**

| # | 決定 | 内容 |
|---|---|---|
| D-1 | 下限の表現 | **`ping` の `version` を semver で判定**（`>= 0.7.5`）。`protocol` は捨てる |
| D-2 | 上限 | **設けない。** 新しい herdr を拒否しない |
| D-3 | 変化の検知 | 22 メソッドに**スライスした schema** を repo にコミットし、CI で突き合わせる |
| D-4 | 型の出所 | スライスから**自動生成**。生成元は**下限版**（0.7.5） |
| D-5 | 版ごとの分岐 | **作らない**（下限生成が古い版での動作を保証する） |
| D-6 | 未知 enum | 読む側の全 enum に **`#[serde(other)]`**。送る側には付けない |
| D-7 | 生成物 | **コミット + 再生成スクリプト + CI で drift 検査** |
| D-8 | method → result | schema に無いので **`methods.json` が一次情報**（手書き） |

## D-1 なぜ `version` か、そして何を通すか

`protocol` では「これ以降が必要」を表現できない（上表）。`version` なら
`>= 0.7.5` と書ける — 0.7.5 は `agent.prompt` が入ったリリースそのものだからである。

通す側の判断は 3 つ:

| `ping` の `version` | 判定 | 理由 |
|---|---|---|
| 欠落 | **通す** | 0.7.1 以降必ずあるので、無いのは未知の形。推測で起動拒否すると障害になる |
| semver としてパース不能 | **通す** | 同上。未知の形であって「古い」証拠ではない |
| 下限の prerelease（`0.7.5-rc.1`） | **通す** | semver 順では下限未満だが、下限のリリースそのもの |

## D-1 補足 — このガードは粗い網である

**`version` と `protocol` はどちらも単独では不完全。** preview ビルドは
**基底 stable の `version` を名乗る**（herdr master の `Cargo.toml` は直近タグと
同じ版）ので、preview 同士は `version` で区別できない。逆に `protocol` は
preview ごとに動くが、stable 間で動かないことがある。

したがってガードは「はるかに古い」だけを捕まえる粗い網でよく、**互換の実判定は
コミット済み schema の CI 差分が持つ**。

## D-4 なぜ「下限版から生成する」だけで古い版が動くのか

型は下限版の schema から生成した **1 組だけ**。古い版 = 生成元そのものなので
定義上読める。新しい版は追加しかしない（それを D-3 の CI 差分が保証する）ので、
未知フィールド無視 + `#[serde(other)]` で同じ型が読める。

危険なのは**逆向き**（最新版から生成して古い版で動かす）で、新しく `required` に
なったフィールドを古い版が送らずデシリアライズが落ちる。

## D-3 互換検査は向きで条件が逆になる

`required` の追加を一律に落とす設計は**誤り**だった。読む側と送る側で、互換を
壊す変化が逆になる:

| 側 | 落とす | 通す |
|---|---|---|
| result（読む） | メソッド削除 / result タグ削除 / 生成した型に載っているプロパティの削除 / **`required` から外れる**（新しい版が省略しうる → 下限生成の型が落ちる）/ enum バリアント削除 | **`required` の追加**（保証が強まる）/ プロパティ追加 / enum バリアント追加 |
| request（送る） | **`required` の追加**（totsuka が送らない params を要求される）/ 送る enum バリアントの削除 / 生成した型に載っている params プロパティの削除 | — |

request `$defs` に `additionalProperties: false` は **104 件中 0 件**なので、
totsuka が送る余分なキーは無視される。それでも params プロパティの削除を落とすのは、
「送っているつもりのものが届かない」が黙って縮退する側だからである。

## D-8 なぜ対応表が手書きなのか

herdr の schema は **method と result を結び付けていない。**
`success_response.result` は `type` の const で判別する 57 分岐の `oneOf`
（`ResponseResult`）で、`request` 側の `method` からは辿れない。したがって
「どのメソッドがどの result を返すか」は機械的に取り出せず、
`plugins/agent-ide-herdr/schemas/methods.json` が一次情報になる。

**`result` が `null` のメソッドは、totsuka が応答を読んでいない。** その場合は
封筒の型も作らず、互換検査もメソッドの存在と params の形しか見ない。読んで
いない result の型を主張しても、裏の取りようがない主張が 1 つ増えるだけである。

# Consequences

- 型化が変えるのは「**どこで気づくか**」だけで、「気づいた後どうするか」は
  呼び出しごとの既存の degrade 方針をそのまま保つ
- **result 封筒は `type` タグを検査しない。** タグの改名を報せるのはマージ前の
  schema 差分であって、実行時の失敗ではない（D-3 が厳格である以上、実行時まで
  厳格にすると生存できる変化で落ちるようになる）
- `deny_unknown_fields` は付けない。前方互換はこの結合の無料の利点である
- 生成器は**教えていない JSON Schema 構文でフェイルクローズ**する。推測して
  生成すると、生成物が黙って間違う
- **rustfmt は生成の必須依存**。実測で整形の有無が生成結果を変える
  （1 フィールドの enum バリアントが 1 行に畳まれる）ので、無いと生成物が環境で
  揺れ、drift 検査がその揺れを差分として報告する
- 生成が失敗しても生成物を壊さない（一時ファイルへ書いてから差し替える）
- **PR の CI は最新版を取りに行かない。** herdr がリリースされた瞬間に無関係な
  PR が全部赤くなり、ネットワーク障害でも落ちるため。追随は日次 cron の別レーン

## 却下した案

| 案 | 却下の理由 |
|---|---|
| **protocol 番号でコードパスを分岐** | protocol は API 互換を追跡していない。`custom_status` の削除は bump 無しで起きたので分岐では捕まらない。実際に破壊が起きた `agent.send` → `agent.prompt` のとき totsuka がやったのも分岐ではなく「下限を上げて呼び替える」だった（ADR-0032） |
| **未知メソッドを呼んでみて fallback** | herdr に「method not found」専用エラーが無く、`invalid_request` はパラメータ不正と同じコード。**自分の params バグを「古い herdr」と誤認して黙って縮退する** |
| **エラー文のメソッド名列挙をパース** | 未知メソッド時のエラー文に全メソッド名が出るのは事実だが、非公式で散文の形に依存する |
| **`herdr api schema --json` を実行時に読む** | 248KB のパースが起動経路に乗り、ソケットでなくバイナリ実行に依存する。そもそもクライアント同梱の schema でサーバのものではない |
| **最新版の schema から生成** | 新しく `required` になったフィールドを古い版が送らず、**古い herdr で totsuka が落ちる**。「幅を持ったサポート」の放棄 |
| **最新から生成して全フィールドを `Option` 化** | 壊れ方が今と同じ「静かに `None`」に戻り、型を入れた意味が消える |
| **下限を毎回最新へ追随** | 型は常に 1 組で正確だが、古い herdr のユーザは起動拒否。約 2 週間ごとに totsuka のリリースが必須になる |
| **CI が毎回 GitHub から最新 schema を取得** | herdr のリリース直後に無関係な PR が全部赤くなる。ネットワーク障害でも落ちる |
| **`build.rs` で生成** | 生成物が `OUT_DIR` に隠れて **herdr の変化がレビューできない**。fixture の差分レビュー＝互換性レビュー、という既存の運用と噛み合わない |
| **`deny_unknown_fields` を付ける** | 前方互換が今の無料の利点であり、それを捨てることになる |
| **result 封筒の `type` タグを実行時に検査** | 生存できる変化（タグ改名 + 中身は同じ）で落ちるようになる。同じ変化を schema 差分がマージ前に落とすので、実行時の厳格さは重複したうえで有害 |

## この設計がカバーしないもの

**schema に載らない暗黙契約**（#521）。metadata token 値の 80 文字上限
（herdr は黙って切る）、pane id が `w1:p1` 形式であること、herdr 内部の 5 秒下限、
`workspace.create` の `env` が root pane に適用されること、そして
**`pane.split` の shell pane が env を継承しない**というセキュリティ前提。
最後の 1 つだけ性質が違い、**壊れても動き続けたまま秘密が漏れる**。
