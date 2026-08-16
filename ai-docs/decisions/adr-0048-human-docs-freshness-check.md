---
type: Decision
title: ADR-0048 人間向け docs/ は編集的に生成し、鮮度だけを CI で検査する
description: "docs/ を ai-docs/ から生成する仕組みの決定。変換は編集的（frontmatter 除去に留まらず内部 issue 番号・判断過程を落とす）なので決定的な再生成検査は書けず、代わりに生成ページへソースの content hash を埋めて CI が古さだけを検出する。hash だけを書き換える近道を作らないため、検査スクリプトは docs/ を一切書き換えない。"
resource: https://github.com/tomoya-k31/totsuka/issues/458
tags: [decision, docs, ci, tooling, adr]
generated: { by: claude-code/fable-5, at: 2026-08-16T19:20:53+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-458
    resource: https://github.com/tomoya-k31/totsuka/issues/458
    title: "docs 分離: AI 用 OKF バンドルを ai-docs/ へ移設し、人間用 /docs を AI 生成 + 鮮度検査で新設する"
  - id: adr-0047
    resource: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0047-ai-docs-human-docs-split.md
    title: "ADR-0047 AI 用 OKF バンドルを ai-docs/ へ分離し、人間用 docs/ を生成物にする"
---

# Status

stable（[#458](https://github.com/tomoya-k31/totsuka/issues/458)）。[ADR-0047](/decisions/adr-0047-ai-docs-human-docs-split.md) が決めた分離の、生成側の実装を定める。

# Context

ADR-0047 は `docs/` を `ai-docs/` からの生成物と決めたが、**どうやって生成し、どうやってズレを防ぐか**は決めていなかった。

難しいのは、この変換が機械的ではないことである。frontmatter を剥がすだけでは足りず、内部 issue 番号・ADR 参照・判断過程の記録・実測ログを落とし、残ったものを利用者の語彙で書き直す必要がある。つまり**生成は編集作業**で、「もう一度生成すれば必ず同じ出力になる」という決定的な検査は書けない。

一方で検査を置かないと、生成スキルの実行を忘れた PR から黙ってズレる。このリポジトリでは**検査の無い手動同期が既に 2 度壊れている**（[ADR-0031](/decisions/adr-0031-docs-ledger-conflicts.md) の元帳、[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md) の撤回漏れ）。元帳が機能しているのは `--check` を CI に置けたからである。

# Decision

保証を 2 つに割る。

| 何を | 誰が |
|---|---|
| 内容の正しさ | `human-docs` スキル + PR レビュー（人間） |
| **古さの検出** | `scripts/docs-freshness.sh`（CI） |

生成ページの先頭付近に、ソースの content hash を持つマーカーを 1 行入れる:

```text
<!-- generated-from: ai-docs/development/config-reference.md sha256:<64hex> -->
```

HTML コメントなので描画されない。`docs-freshness.sh` はこれを読み、ソースの現在の hash と照合して、食い違えば `stale` として落とす。

## 検査スクリプトは docs/ を書き換えない

`--marker <source>` はマーカー 1 行を **標準出力に印字するだけ**で、`docs/` には一切触れない。これは意図的で、**「hash だけ更新して内容は古いまま残す」という、検査を黙って無効化する近道を作らないため**である。

書き換える `--update` を用意すると、落ちた CI を通す最短経路がそれになる。生成物の内容が古いまま検査だけが緑になる状態は、検査が無い状態より悪い — 「同期されている」という誤った信号を出すからである。

## この検査が保証しないこと

**保証するのは「古くない」ことだけで、内容が正しいことは保証しない。** hash が一致していても、生成物が中身の薄い要約でも、誤訳でも、検査は通る。そこは PR レビューが引き受ける。ADR-0031 の `log-sync` とは強度が違う点を、スキルと dev-flow の両方に明記した。

## マーカーを持たないページは明示的に列挙する

`docs/` 配下でマーカーを持たないページは、スクリプト内の `EXEMPT`（現状 `index.md` / `index.ja.md` のみ）に列挙したものだけを許す。列挙外のマーカー無しページはエラーにする。**そうしないと「マーカーを付け忘れたページ」が検査をすり抜けて永久に古いままになる**からで、これは黙って正しく見える壊れ方である。

## CI への置き方

`okf-lint.yml` の **`lint` ジョブ内のステップ**として足す。このジョブ名はブランチ保護の必須チェックのコンテキストなので、ここに入れれば ruleset を変えずに必須化される。新しいジョブを足すと、required checks に登録するまで検査が任意になる。

# Alternatives considered

- **決定的スクリプトで生成する**（セクションマーカー + frontmatter 除去） — CI で完全に検査できるが、「内部情報の除去と簡潔な書き直し」は編集作業であり、機械変換では目的の品質に届かない。不採用。
- **鮮度検査を置かず生成スキルだけに任せる** — スキルの実行を忘れた PR から黙ってズレる。ADR-0047 で却下した「手動同期」と強度が変わらない。不採用。
- **`--update` で hash を一括更新できるようにする** — 便利だが、それが CI を通す最短経路になる。上記のとおり不採用。
- **生成物の内容そのものを検査する**（要約の網羅性チェック等） — 編集的変換に対する自動検査は、それ自体が生成と同じ難しさを持つ。人間のレビューに委ねる。不採用。
- **新しい CI ジョブとして足す** — ruleset の required checks に登録するまで任意チェックになる。既存の必須ジョブ内のステップにするほうが確実。不採用。

# Consequences

- `ai-docs/` の 5 つの生成元を触った PR は、`docs/` の対応ページも同じ PR で作り直す義務を負う。義務は `human-docs` スキル・`okf-docs` スキル・dev-flow の 3 箇所に書いてある。
- 生成元でない `ai-docs/` のファイルを触っても検査は落ちない。5 ペアの対応表はスキルが持つ。
- `docs/` は OKF バンドルではないので `okf-lint` を掛けない。PostToolUse フックのパス判定も `ai-docs/` のみに絞ってある。
- `rumdl` は `docs/` も検査する。生成ページは言語スイッチャとマーカーが本文より前に来るので、`.rumdl.toml` に `MD041` の per-file ignore を足した（README と同じ理由）。
- `orchestrator-spec` だけが英日で別ソースを持ち、他の 4 本は日本語 1 本から両言語を生成する。後者では 2 つの生成ページに**同じソースの同じ hash** が入るので、ソースを触ると 2 件同時に落ちる。
