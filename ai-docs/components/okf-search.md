---
type: Tool
title: okf-search
description: docs/ の frontmatter（type/status/owner/resource/tags と OKF v0.2 の generated/verified/sources/stale_after）でconceptを絞り込むCLIスクリプトと、絞り込み結果をAIが読んで抽出するokf-searchスキル。
resource: /scripts/okf-search.sh
tags: [okf, search, tooling]
generated: { by: human:tomoya-k31, at: 2026-07-29T00:00:00Z }
status: stable
---

# 責務

`docs/` バンドル内の concept ファイルを、本文ではなく frontmatter のフィールド（`type` / `status` / `owner` / `resource` / `tags` と OKF v0.2 の `generated` / `verified` / `sources` / `stale_after`）をクエリキーとして絞り込む。本文の全文検索は行わない — 絞り込んだファイル一覧を呼び出し元（`okf-search` スキル / Claude）に渡し、実際の内容抽出・要約は AI 側が行う。

frontmatter は OKF バンドル運用における「クエリ・フィルタ・インデックス対象の小さなフィールド集合」であり、本ツールはその役割を活かす検索インターフェースにあたる。`ai-docs/CLAUDE.md` の progressive disclosure（index.md を辿る読み方）を置き換えるものではなく、ディレクトリ横断的な条件検索を補うもの。

# 公開インターフェース

```text
scripts/okf-search.sh [bundleDir=docs] [フィルタ...] [出力オプション]
```

- フィルタ（すべて AND）: `--type` `--status` `--owner` `--resource` `--resource-like` `--tag`（繰り返し可） `--field KEY=VALUE`（繰り返し可） `--after` `--before` `--generated-by` `--trust-tier` `--stale` `--source-like`
- 出力: 既定は `path / type / status / generated.at / trust / title — description` の表。`--paths-only` でパスのみ。`--list-values FIELD` で distinct 値と件数の一覧（`trust` / `generated.by` / `sources.resource` の擬似フィールドも指定可）。
- `--after` / `--before` は v0.2 の `generated.at` を見る。`generated` を持たず旧 `timestamp` だけの concept は
  timestamp にフォールバックする（SPEC §13.1 が認める後方互換）。
- `--trust-tier` は frontmatter に保存された値ではなく `verified` から導出する（SPEC §5.3）。
  信頼段階は主観的で陳腐化するため OKF は値を保存せず、シグナルだけを持つ設計になっている。
- ネストした frontmatter（`sources` のブロックシーケンス、`generated` のフローマッピング）を読むため、
  字下げ行を直前のトップレベルキーの続きとして解釈する。
- 依存: bash 3.2+, POSIX awk/grep/sed のみ（`scripts/okf-lint.sh` と同方針。追加の外部依存なし）。
- 値の引用符は YAML の構文であって値の一部ではないので、**外してから比較・表示する**。`description` は
  ` #` や `: ` を含む場合に引用が必須（`ai-docs/CLAUDE.md`）で、外さないと表示に `"` が混ざり、
  `--field KEY=VALUE` の完全一致も引用符の有無で外れる（#304）。

# Examples

```bash
bash scripts/okf-search.sh --type Decision
bash scripts/okf-search.sh --status deprecated --paths-only
bash scripts/okf-search.sh --tag okf --after 2026-01-01
bash scripts/okf-search.sh --trust-tier unverified --type Reference
bash scripts/okf-search.sh --stale
bash scripts/okf-search.sh --list-values trust
```

# 依存先

- `ai-docs/CLAUDE.md` の frontmatter テンプレート・type 語彙表に準拠する
- `.claude/skills/okf-search/SKILL.md` から呼び出される（クエリ→フィルタ翻訳、絞り込み、該当ファイルのみ読んでAI抽出、という手順を定義）
