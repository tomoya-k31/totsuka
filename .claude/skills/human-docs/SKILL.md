---
name: human-docs
description: 人間向け docs/ を ai-docs/ の対応ソースから生成・更新するときに使う。トリガー: ai-docs/development/config-reference.md・plugin-dev-guide.md、ai-docs/operations/setup-playbook.md・operations-guide.md、ai-docs/product/orchestrator-spec(.ja).md のいずれかを編集したとき（この 5 本は docs/ の生成元なので、触ったら必ず生成物も更新する）。「docs を更新」「利用者向けドキュメントを直して」「鮮度検査が落ちた」「generated-from が stale」等の依頼にも使う。README からリンクされるページを書き換えるときは常に該当する。
allowed-tools: Read, Write, Edit, Bash, Grep
---

# human-docs

`docs/` は **`ai-docs/` からの生成物**（[ADR-0047](/decisions/adr-0047-ai-docs-human-docs-split.md)）。
このスキルは、その生成をどうやるかと、何を落とすかを定義する。

`ai-docs/` は OKF バンドルでエージェント向け。`docs/` は実利用ユーザ向けで、
OKF ではない（frontmatter も元帳も index 掲載義務も無い）。**`docs/` に
`okf-lint` を掛けない。**

## 生成マップ

| 生成物 | ソース |
|---|---|
| `docs/config-reference.md` / `.ja.md` | `ai-docs/development/config-reference.md` |
| `docs/plugin-dev-guide.md` / `.ja.md` | `ai-docs/development/plugin-dev-guide.md` |
| `docs/setup-playbook.md` / `.ja.md` | `ai-docs/operations/setup-playbook.md` |
| `docs/operations-guide.md` / `.ja.md` | `ai-docs/operations/operations-guide.md` |
| `docs/orchestrator-spec.md` | `ai-docs/product/orchestrator-spec.md` |
| `docs/orchestrator-spec.ja.md` | `ai-docs/product/orchestrator-spec.ja.md` |

`orchestrator-spec` **だけ**が英日で別ソースを持つ。残り 4 本のソースは日本語 1 本で、
そこから `.md`（英語）と `.ja.md`（日本語）の両方を作る。したがって両者には
**同じソースの同じ hash** が入る。

`docs/index.md` / `docs/index.ja.md` は手書き。生成対象ではない
（`scripts/docs-freshness.sh` の `EXEMPT` に入っている）。

## 手順

1. **ソースを読む**（生成マップの該当行）。
2. **落とすものを落として書き直す**（次節）。frontmatter を剥がすだけでは足りない。
3. **マーカーを入れる**。言語スイッチャの直後に 1 行:

   ```bash
   bash scripts/docs-freshness.sh --marker ai-docs/development/config-reference.md
   ```

   出力をそのまま貼る。**hash を手で書かない。**
4. **両言語を揃える**。`.md` と `.ja.md` は同じ構成・同じ節見出しにする
   （→ [documentation-i18n](../../rules/documentation-i18n.md) の言語スイッチャ規約に従う）。
5. **検査する**:

   ```bash
   bash scripts/docs-freshness.sh   # 0 error になるまで
   rumdl check .                    # docs/ も対象
   ```

## 何を落とすか

ここが本スキルの本体。**読者はこのツールを使う人であって、開発した人ではない。**
リポジトリの事情を知らない読者にとって意味の無い情報は完全に除去する。

落とすもの:

- **issue / PR / ADR 番号**（`#458`、`PR #460`、`ADR-0031`）。本文中の言及も、
  リンクも落とす。「なぜこの仕様になったか」を辿りたい読者は居ない前提でよい —
  必要なら `ai-docs/` を読む人（＝保守する人）向けの情報である
- **判断過程・不採用案**（「〜も検討したが不採用」「〜という経緯で」）。
  決まった結果だけを書く
- **実測ログ・検収記録**（「実機で確認した」「PR #352 で通した」）
- **frontmatter 一式**、`generated` / `verified` / `stale_after` / `sources`
- **バンドル内リンク**（`/decisions/adr-0031-…`）。生成物から `ai-docs/` へ
  リンクしない — 読者に見せる先ではない。同じ内容が `docs/` 側にあるなら
  そちらへ、無ければリンクごと落として本文に畳む
- **内部コンポーネント名で書かれた説明**（`orchestrator-core` の `dispatch_one` が…）。
  ユーザから見た振る舞いに言い換える

残すもの:

- 設定キー・CLI コマンド・ファイルパス・環境変数（**識別子は訳さない**）
- 実際に貼って動く例
- エラーメッセージと、その対処
- 外部ドキュメントへのリンク

**短くすること自体は目的ではない。** 目的は「使うために要る情報だけにする」こと。
落とした結果ソースの 3 割になることも、表がそのまま残って 8 割になることもある。

## 相互参照を書く

分離した 2 つのツリーが互いを知らないと、読者も保守する人も迷子になる。

- `docs/` の各ページの末尾に、対応する `ai-docs/` のソースを 1 行で示す
  （「詳細な設計判断は …」の形。**リンクにはしない** — 読者を OKF 側へ
  送り込まないため）
- `ai-docs/` 側のソース冒頭に、人間向け生成物があることを 1 行で示す

## 鮮度検査との関係

CI（`okf-lint.yml` の `lint` ジョブ）が `scripts/docs-freshness.sh` を回す。
検査しているのは**「ソースが変わったのに生成物が追随していない」ことだけ**で、
**内容が正しいかは検査していない**。そこは PR レビューが引き受ける。

だから「検査が通った = 生成物が正しい」ではない。逆に、
**hash だけ差し替えて内容を古いまま残す**と検査は黙って通る。これは検査を
無効化する行為なので、やらない。`--marker` が `docs/` を書き換えない設計に
なっているのはこのためである。

## 禁止事項

- `docs/` に frontmatter を書かない（OKF ではない）
- `docs/` を `ai-docs/` の index / log に載せない
- `docs/` から `ai-docs/` へリンクしない（末尾の 1 行の言及は可）
- 内容を直さずにマーカーの hash だけ更新しない
- 片方の言語だけ更新しない
