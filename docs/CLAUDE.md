---
type: Guide
title: docs バンドル運用ルール（エージェント向け）
description: このOKFバンドルの書き方・更新タイミング・index/logの維持ルール。docs配下を触る前に必ず読むこと。
tags: [meta, okf, rules]
---

# このファイルについて

`docs/` は [OKF (Open Knowledge Format) v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md) に準拠した Knowledge Bundle である。
エージェントが `docs/` 配下のファイルを作成・更新する場合、本ファイルのルールに **必ず** 従うこと。

読み方（progressive disclosure）:

1. まず [`/index.md`](/index.md) を読み、どのディレクトリに何があるか把握する
2. 関係するディレクトリの `index.md` を読む
3. 必要な concept ファイルだけを開く

いきなり `docs/` を全走査しないこと。

ディレクトリ横断で条件検索したい場合（「type が Decision の全部」「status が deprecated な全部」等）は、
index.md を1つずつ辿る代わりに `okf-search` スキル（`scripts/okf-search.sh`、frontmatter をクエリキーに絞り込む）
を使ってよい。絞り込んだ少数のファイルだけを読む点は progressive disclosure と同じ。

# OKF ルール（要約）

正本は [SPEC.md](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)。本バンドルで守る規則は以下。

## 適合条件（MUST）

1. `index.md` / `log.md` 以外のすべての `.md` は、先頭に YAML frontmatter ブロック（`---` で囲む）を持つ
2. frontmatter は空でない `type` フィールドを必ず含む
3. frontmatter は空でない `description` フィールドを必ず含む（index の転記元）
4. frontmatter は YAML として正しくパースできる（下の「引用符が要るケース」を参照）
5. `index.md` / `log.md` は予約ファイル名。concept をこの名前で作らない

## frontmatter テンプレート

```markdown
---
type: <必須。下の type 語彙から選ぶ>
title: <表示名>
description: <1文の要約。index 生成に使うため必ず書く>
resource: <対象資産の正準URI。GitHubパス・GCPコンソールURL等。抽象概念なら省略>
tags: [tag1, tag2]
timestamp: <ISO 8601。意味のある変更をした日時>
status: <拡張キー: draft | active | deprecated | superseded>
owner: <拡張キー: 担当チーム>
---
```

### 引用符が要るケース

frontmatter は **1 行 1 キーの平坦なマッピング**に限る（ネスト・複数行スカラー・ブロックシーケンスは使わない）。
値が次のいずれかを含むときは、**引用符で囲む**:

| 値に含まれるもの | 引用符なしだとどうなるか |
|---|---|
| 半角スペース + `#`（例: `マーカーは #196 で入った`） | `#` 以降が YAML コメントとして捨てられ、`description` が途中で切れる |
| コロン + 半角スペース（例: `（bin: totsuka）`） | YAML がマッピングとして解釈しようとしてパースに失敗する |
| 先頭が `#` `&` `*` `!` `%` `@` などの指示文字 | YAML の構文要素と衝突する |

`#` を全角の `＃` に置き換えるといった回避はしない — 引用符で囲む。
いずれも `bash scripts/okf-lint.sh docs` の `fm-yaml` が検出する。

## クロスリンク

- バンドルルート相対で書く: `[顧客テーブル](/data/customers.md)`（`/` は `docs/` を指す）
- 相対リンク（`./other.md`）は同一ディレクトリ内のみ許容
- リンク先が未執筆でもよい（SPEC §5.3）が、CI が警告を出すので意図的な場合のみ残す

## 本文の見出し規約

該当する場合はこの見出し名を使う: `# Schema`（テーブル・データ構造）、`# Examples`（使用例）、`# Citations`（外部出典。文末に番号付きで列挙）。

# ディレクトリと推奨 type

| ディレクトリ | 管理する情報 | 推奨 type |
|---|---|---|
| `/architecture/` | システム構成、コンテキスト図、依存関係、非機能要件 | `Architecture`, `Diagram` |
| `/decisions/` | ADR。1決定=1ファイル、`adr-NNNN-<slug>.md` 連番 | `Decision` |
| `/components/` | パッケージ/サービス単位の責務・公開IF・依存先 | `Component`, `Service`, `Library`, `Tool` |
| `/apis/` | エンドポイントの意味・利用文脈・認証。スキーマ本体は `resource` で参照 | `API Endpoint`, `Webhook`, `Event` |
| `/data/` | テーブルの意味づけ、ER関係、データライフサイクル | `Table`, `Data Model`, `Queue` |
| `/infrastructure/` | GCP構成、環境、IaCモジュール、Secret方針 | `GCP Resource`, `Environment`, `IaC Module` |
| `/operations/` | 障害対応 Playbook、デプロイ手順、アラート別トリアージ | `Playbook`, `Runbook`, `Alert` |
| `/development/` | 環境構築、規約、ブランチ戦略、レビュー規約 | `Guide`, `Convention` |
| `/quality/` | テスト戦略、E2Eシナリオ、既知の不具合パターン | `Test Strategy`, `Known Issue` |
| `/security/` | 脅威モデル、認可設計、脆弱性対応方針 | `Threat Model`, `Policy` |
| `/product/` | 機能仕様、ユースケース、背景 | `Feature`, `Spec`, `Use Case` |
| `/releases/` | リリースノート、互換性、マイグレーション手順 | `Release`, `Migration` |
| `/glossary/` | ドメイン用語・社内略語。1用語=1ファイル | `Term`, `Concept` |
| `/references/` | 外部資料の要約ミラー（Citations の参照先） | `Reference` |

新しい type が必要な場合は自由に定義してよい（OKF は type を中央登録しない）が、まず上の語彙で表現できないか検討し、新設したらこの表に追記すること。

# ドキュメントを残すタイミング（トリガー定義）

以下のイベントが発生する作業を行ったら、**同一 PR 内で** 対応するドキュメントを作成・更新する。コードだけの PR にしない。

| トリガーとなるイベント | 作成/更新するもの | 期限 |
|---|---|---|
| 技術選定・アーキテクチャ上の意思決定（ライブラリ採用、方式変更、トレードオフ判断） | `/decisions/adr-NNNN-*.md` を新規作成 | 実装 PR と同時 |
| 新しいパッケージ・サービス・モジュールの追加 | `/components/` に concept 新規作成 | 同一 PR |
| 既存コンポーネントの責務・公開IFの変更 | 該当 `/components/*.md` を更新、`timestamp` 更新 | 同一 PR |
| API エンドポイント・イベント・Webhook の追加/破壊的変更 | `/apis/` を作成/更新 | 同一 PR |
| DB スキーマ・キュー・データモデルの変更 | `/data/` を作成/更新（`# Schema` 見出し） | 同一 PR |
| インフラ・環境・IaC の変更 | `/infrastructure/` を作成/更新 | 同一 PR |
| インシデント発生・対応 | `/operations/` に Playbook/Postmortem | 収束後 2 営業日以内 |
| 新しいアラート・監視の追加 | `/operations/` にトリアージ手順 | 同一 PR |
| リリース（バージョンタグ付与） | `/releases/` に Release ノート | リリース時 |
| 新しいドメイン用語・略語の導入 | `/glossary/` に 1 ファイル | 初出の PR |
| ドキュメントの前提が崩れた（廃止・置換） | 該当ファイルの `status: deprecated` 化と後継へのリンク。削除はしない | 気づいた PR |

判断に迷ったら「3ヶ月後の自分/別のエージェントがこの判断・構造を再構築できるか」を基準にする。できないなら書く。

# index.md のルール

- **すべてのディレクトリ**に `index.md` を置く（progressive disclosure の要）
- concept ファイルまたはサブディレクトリを追加・改名・削除したら、**同じコミットで**そのディレクトリの `index.md` を更新する
- エントリ形式: `* [Title](file.md) - frontmatter の description をそのまま転記`
  - **全文をそのまま**転記する（要約・省略・追記をしない）。lint の `index-desc` が
    frontmatter との一致を検査するので、description を書き換えたら index も同じコミットで直す
  - description を引用符で囲んでいる場合、index には**引用符を外した中身**を転記する
- ルート `/index.md` のみ frontmatter（`okf_version` 宣言）を持つ。他の index.md に frontmatter を書かない
- `README.md` / `CLAUDE.md` は index への掲載対象外（linter も除外している）

# log.md のルール

- `log.md` はルート（`/log.md`）にのみ置く。ディレクトリ単位の log は作らない
- 以下の場合に追記する（新しい日付が上）:
  - concept の新規作成・廃止（`**Creation**` / `**Deprecation**`）
  - 既存 concept の意味的な更新（`**Update**`。typo 修正は不要）
- 日付見出しは `## YYYY-MM-DD` 固定。同日ならエントリを追記する
- エントリには対象 concept へのバンドルルート相対リンクを含める

# 検証（CI / hooks）

ドキュメントを変更したら必ず lint を通すこと:

```bash
bash scripts/okf-lint.sh docs          # 下記チェックをエラーとして報告
bash scripts/okf-lint.sh docs --strict # 加えてリンク切れもエラー化
```

| チェック | 内容 |
|---|---|
| `frontmatter` | frontmatter ブロックがある |
| `fm-yaml` | frontmatter が YAML として壊れていない（引用符・`: `・` #`・重複キー・タブ・インデント） |
| `type` | 空でない `type` がある |
| `description` | 空でない `description` がある |
| `index-fm` | ルート以外の `index.md` に frontmatter が無い |
| `index-exists` | concept を含むディレクトリに `index.md` がある |
| `index-listed` | 各 concept / サブディレクトリが `index.md` から張られている |
| `index-desc` | `index.md` の転記が frontmatter の `description` と一致する |
| `log-format` | `log.md` の日付見出しが `## YYYY-MM-DD` |

PostToolUse hook（`.claude/settings.json`）により、docs 配下の編集後に自動で lint が走る。エラーが返った場合は必ず修正してから作業を完了すること。
