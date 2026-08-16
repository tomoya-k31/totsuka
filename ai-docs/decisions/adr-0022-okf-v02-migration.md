---
type: Decision
title: ADR-0022 docs バンドルを OKF v0.2 へ移行する
description: "ai-docs/ の準拠バージョンを OKF v0.1 から v0.2 へ上げる決定。破壊的変更2件（timestamp→generated.at、本文 # Citations→frontmatter sources）をスクリプトで一括変換し、status 語彙を draft/stable/deprecated へ寄せ、okf-lint の YAML 部分集合をネスト対応へ広げる。verified と stale_after は運用ルールだけ定義して既存ファイルへの一括付与はしない。"
tags: [documentation, okf, migration, tooling, lint]
resource: https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md
status: stable
generated: { by: human:tomoya-k31, at: 2026-07-29T00:00:00Z }
owner: tomoya-k31
sources:
  - id: okf-spec-v02
    resource: https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md
    title: OKF SPEC v0.2
    last_modified: 2026-07-29
  - id: adr-0001
    resource: /decisions/adr-0001-adopt-okf.md
    title: ADR-0001 OKFによるドキュメント管理の採用
  - id: okf-search-component
    resource: /components/okf-search.md
    title: okf-search コンポーネント
---

# Status

Accepted — 2026-07-29

# Context

OKF v0.2 が公開された。[ADR-0001](/decisions/adr-0001-adopt-okf.md) で採用した v0.1 に対し、
v0.2 は「エージェントが継続的に書き換える知識コーパス」を前提に、provenance / trust /
lifecycle / attestation を frontmatter の第一級市民にしている（SPEC §1）。

本バンドルに効く差分は 3 種類ある。

**破壊的変更が 2 件**（SPEC §13.1）:

- `timestamp` が `generated: { by, at }` に置き換わった。`by`（actor）が必須になった
- 本文の `# Citations` リストが frontmatter の `sources` に置き換わった

**拡張キーだった `status` が標準キーになった**（SPEC §5.4）。語彙が `draft` / `stable` /
`deprecated` の 3 値に固定され、本バンドルが使っていた `active`（48 件）と
`accepted`（21 件、ADR 全部）は語彙外になった。

**任意ファミリの追加**: `sources` の信頼シグナル、`verified`、`stale_after`、
`Attested Computation` 型と計算キー群、actor 記法、本文見出し `# Computation`。

移行しないという選択肢もあった。v0.2 の消費者は legacy な `timestamp` と `# Citations` を
読んでもよいとされている（§13.1）ので、放置しても「壊れる」わけではない。しかし
`ai-docs/` は本リポジトリの唯一の知識ソースであり、その価値は機械可読な一貫性にある。
2 系統の書式が混在すれば lint も検索も両方を相手にし続けることになり、
「フォーマットに合わせる」コストは時間とともに増えるだけなので、一括で寄せる。

# Decision

`ai-docs/` を OKF v0.2 準拠とする。ルートの `/index.md` は `okf_version: "0.2"` を宣言する。

## 1. frontmatter の一括変換はスクリプトで行う

`scripts/okf-migrate-v02.sh` を新設し、機械的に決まる変換だけを担わせる（冪等・`--dry-run` 付き）。
手で 74 ファイルを触ると必ず取りこぼすため、変換内容そのものをレビュー可能なコードとして残す。

| 変換 | 件数 |
|---|---|
| `timestamp: X` → `generated: { by: human:tomoya-k31, at: X }` | 72 |
| `status: active` / `accepted` → `stable` | 69 |
| 本文 `# Citations` → frontmatter `sources` | 19 |
| `okf_version` を `"0.2"` へ | 1 |

## 2. `generated.by` は `human:tomoya-k31` で一律に埋める

過去のファイル単位の作者は復元できない。actor は必須なので何かを書く必要がある。

`claude-code/<version>` にしなかったのは、実際には複数モデル・複数セッションで書かれており
`<producer>/<version>` の version が正確に書けないため。`process:okf-migration` にしなかったのは、
それが「移行スクリプトが埋めた」という変換の事実を指すだけで、内容の出自を何も語らないため。
`human:tomoya-k31` は「最終的にレビューしてコミットした責任者」という、実際に成り立っている事実を指す。

**限界を明記しておく**: これは「この人が直接書いた」という主張ではない。§5.3 の信頼段階は
`generated` ではなく `verified` から導出されるので、`generated.by` が `human:` でも
信頼段階は `unverified` のままであり、この一律値が信頼を水増しすることはない。

## 3. `status` は SPEC の 3 値に寄せ、ADR ライフサイクルは本文へ移す

`active` → `stable`、`accepted` → `stable`。

ADR の `accepted` / `superseded` は ADR 界隈では標準的な語彙で、残す案もあった。v0.2 も未知の値の
拒否を禁じている（§11）ので違反ではない。それでも寄せたのは、`status` の意味が
「バンドル全体のライフサイクル」と「ADR 固有のライフサイクル」の 2 系統に割れると、
`--status` での横断検索が type ごとに違う意味を返すようになり、
機械可読性という移行の目的そのものを損なうから。

別キー（`decision_status`）を足す案も採らなかった。ADR のライフサイクルは
既に本文の `# Status` 見出しに書かれており、frontmatter に二重化すると片方だけ更新される
腐敗リスクを新たに作ることになる。**ADR のライフサイクルは本文 `# Status` を正とする。**

## 4. `# Citations` は `sources` へ移し、本文の出典リストは廃止する

1 引用行 = 1 `sources` エントリ。`resource` が取れない行（例: 実機プローブ記録）は
行文そのものを scope descriptor として `resource` に入れる（§5.1 が明示的に許容している）。
`id` は元の採番を保つ `ref-N`。

本文からの per-claim 脚注化（`[^id]`）は**今回は行っていない**。本リポジトリの本文には
`[1]` 形式の番号参照が 1 件も無く、脚注を張る先が存在しなかったため。`sources` への移設だけで
情報は落ちていない。今後の per-claim 帰属は脚注で書く（ルールは `ai-docs/CLAUDE.md`）。

1 行に複数リンクを持つ引用行が 5 箇所あり、これは 2 本目以降の URL が失われるため
スクリプトが WARN で列挙し、手でエントリを分割した。黙って捨てないことを優先した。

## 5. okf-lint の YAML 部分集合をネスト対応へ広げる

v0.1 の `fm-yaml` は「1 行 1 キーの平坦なマッピング」しか許さず、**v0.2 の `sources` を
100% エラーにする**。外部 YAML パーサ（yq / PyYAML）を導入する案は採らなかった:

- CI に新しい依存を持ち込む（`bash 3.2 + POSIX awk` だけという方針を崩す）
- そして何より、**パーサは「パースは通るが値が壊れる」事故を検出できない**。
  ` #` 以降がコメントとして捨てられた description は、パーサから見れば正常な短い文字列でしかない。
  この検出（PR #303 の再発防止）が `fm-yaml` の存在理由なので、パーサでは置き換えられない。

代わりに部分集合をインデント 0 / 2 / 4 のネストへ広げ、既存のスカラー検査を深さに関係なく効かせた。
併せて v0.2 のファミリに対する意味検査を追加した（`status` 語彙、actor 記法、`generated.by` 必須、
ISO 8601、`sources[].resource` 必須、`Attested Computation` の `runtime` 必須、
旧 `timestamp` / `# Citations` の残存検出、`stale_after` 超過の警告、脚注ラベルと `sources[].id` の整合）。

## 6. okf-search は `generated.at` を見る（legacy フォールバック付き）

`--after` / `--before` の比較対象を `generated.at` に変更し、`generated` が無い concept は
旧 `timestamp` にフォールバックする（§13.1 が認める後方互換）。
`--trust-tier` / `--stale` / `--generated-by` / `--source-like` を追加した。

信頼段階は frontmatter に**保存しない**。SPEC が値ではなくシグナルだけを持つ設計にしているのは、
信頼度が主観的で、消費者間で可搬でなく、陳腐化するため（§5.1）。検索側で毎回導出する。

## 7. `verified` / `stale_after` は運用ルールだけ定義し、一括付与はしない

どの concept が実機で検証済みかは既存ファイルから機械的に導出できない。
`verified` を一括で付ければ、それは**検証していない事実に検証済みの印を付ける**ことになり、
このファミリの存在意義を最初から壊す。よって既存 74 ファイルは全て `unverified` のまま出す。

代わりに `ai-docs/CLAUDE.md` に記入トリガーを定義した:

- `verified` は実機で動作確認が取れたときに書く。実装して PR を通しただけでは書かない
- CI や定期ジョブが担保している事実は `process:<id>` で書く
- `stale_after` は `/references/`（外部ドキュメントのミラー）に必須、目安 6 ヶ月

## 8. `Attested Computation` は書式受け入れのみ

サンクション化すべき計算式が本リポジトリに現状無いため、concept は作らない。
lint は `runtime` 必須などの書式検査を通すようにしてあるので、必要になった時点で書ける。

# Consequences

- 全 74 concept の信頼段階は `unverified` から始まる。`bash scripts/okf-search.sh --trust-tier unverified`
  が全件を返す状態であり、これは**正直な初期状態**であって不具合ではない
- frontmatter に書ける YAML が明示的に制限された（インデント 0/2/4、複数行スカラー・アンカー禁止）。
  制限は `ai-docs/CLAUDE.md` に文書化し、逸脱は lint が落とす
- `generated.by` は全件同一値のため、当面 `--generated-by` での絞り込みに識別力は無い。
  今後の更新で実際の actor を書き分けていけば意味を持ち始める
- ADR の `accepted` が frontmatter から消えた。ADR のライフサイクルを機械的に引きたい場合は
  本文 `# Status` を読む必要がある
- 外部資料ミラーの `last_modified` は埋めていない。既存の `（YYYY-MM-DD 参照）` は
  **参照日であって出典の更新日ではない**ため、流用すると誤ったシグナルになる。
  不明は不明のまま残し、次にミラーを更新するときに実測値を入れる
