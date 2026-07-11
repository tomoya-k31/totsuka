---
name: okf-search
description: docs/ 配下（OKF Knowledge Bundle）を type/status/owner/resource/tags/timestamp などの frontmatter で横断的に絞り込んでから読むときに使う。トリガー: 「type が Decision の一覧」「status が deprecated なdocsを教えて」「特定resourceに関するドキュメントを探して」「最近更新されたdocsは？」「〇〇タグのドキュメントをまとめて」等、ディレクトリ構造を1つずつ辿るより先にメタデータで候補を絞りたい依頼。
---

# OKF Search Skill

`docs/` の concept ファイルには frontmatter（`type` / `title` / `description` / `resource` / `tags` / `timestamp` / `status` / `owner`）が付与されている（ルールは `docs/CLAUDE.md`）。このスキルは、その frontmatter を **クエリキー** として `scripts/okf-search.sh` でスクリプト側に絞り込ませ、絞り込んだ少数のファイルだけを Claude が読んで実際の抽出・要約（AI側の仕事）を行う手順を定義する。

`docs/CLAUDE.md` の progressive disclosure（index.md を辿る）を置き換えるものではない。ディレクトリが分かっている通常の読み方は従来どおり index.md チェーンを辿ること。このスキルは、ディレクトリ横断的な条件検索（「type が X の全部」「status が deprecated な全部」など）のときに使う。

## 手順（必ずこの順で）

1. **クエリをフィルタに変換する**: ユーザーの依頼を `--type` / `--status` / `--owner` / `--resource` / `--resource-like` / `--tag` / `--after` / `--before` / `--field KEY=VALUE` の組み合わせに翻訳する。全部 AND 条件。
   - 有効な値が分からない場合は先に `bash scripts/okf-search.sh --list-values <field>`（例: `type` / `status` / `owner` / `tags`）で実在する値を確認する。当てずっぽうで値を打たない。
   - `type` の語彙表は `docs/CLAUDE.md` のディレクトリ表を正とする。
2. **絞り込む**: `bash scripts/okf-search.sh [フィルタ...]` を実行する（既定 bundle は `docs`）。出力は `path / type / status / timestamp / title — description` の表。ファイル一覧だけで十分なら `--paths-only` を付ける。
3. **0件だった場合**: 全件スキャンにフォールバックしない。フィルタを意図的に緩めるか、ユーザーに「該当なし」と伝える。
4. **候補ファイルだけを読む**: 手順2で絞り込まれたファイルのみを Read し、その内容からユーザーの質問に対する実際の答え・要約・抽出を行う。`docs/` 全体を無差別に読み込まない（`docs/CLAUDE.md` の禁止事項と同じ）。
5. 本文の全文検索（frontmatter に現れない語句での検索）が必要な場合は、手順2の絞り込み結果に対してのみ `grep` 等を使う。フィルタなしで `docs/` 全体を grep しない。

## リファレンス

```
scripts/okf-search.sh [bundleDir=docs] [フィルタ...] [出力オプション]

--type VALUE / --status VALUE / --owner VALUE / --resource VALUE   完全一致
--resource-like TEXT                                                部分一致
--tag TAG                                                            繰り返し指定 or カンマ区切り、AND
--field KEY=VALUE                                                    任意キーの完全一致、繰り返し可、AND
--after TIMESTAMP / --before TIMESTAMP                               ISO 8601 文字列比較
--paths-only                                                         パスのみ出力
--list-values FIELD                                                  FIELD の distinct 値と件数
```

例:

```bash
bash scripts/okf-search.sh --type Decision
bash scripts/okf-search.sh --status deprecated --paths-only
bash scripts/okf-search.sh --tag okf --after 2026-01-01
bash scripts/okf-search.sh --field owner=platform-team
bash scripts/okf-search.sh --list-values type
```

詳細は `bash scripts/okf-search.sh --help`。
