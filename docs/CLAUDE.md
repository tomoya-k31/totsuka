---
type: Guide
title: docs バンドル運用ルール（エージェント向け）
description: このOKFバンドルの書き方・更新タイミング・index/logの維持ルール。docs配下を触る前に必ず読むこと。
tags: [meta, okf, rules]
status: stable
generated: { by: human:tomoya-k31, at: 2026-07-29T00:00:00Z }
---

# このファイルについて

`docs/` は [OKF (Open Knowledge Format) v0.2](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md) に準拠した Knowledge Bundle である。
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
2. frontmatter は空でない `type` フィールドを必ず含む（SPEC が要求する唯一の必須キー）
3. frontmatter は空でない `description` フィールドを必ず含む（index の転記元。本バンドルの追加規約）
4. frontmatter は YAML として正しくパースできる（下の「書ける構造」「引用符が要るケース」を参照）
5. `index.md` / `log.md` は予約ファイル名。concept をこの名前で作らない

## frontmatter テンプレート

```markdown
---
type: <必須。下の type 語彙から選ぶ>
title: <表示名>
description: <1文の要約。index 生成に使うため必ず書く>
resource: <対象資産の正準URI。GitHubパス・GCPコンソールURL等。抽象概念なら省略>
tags: [tag1, tag2]
status: <draft | stable | deprecated。省略時は stable 扱い>
generated: { by: <actor>, at: <ISO 8601。意味のある変更をした日時> }
verified: { by: <actor>, at: <ISO 8601> }   # 検証した事実があるときだけ
stale_after: <YYYY-MM-DD。陳腐化しやすい concept にだけ>
owner: <拡張キー: 担当チーム>
sources:                                    # 出典があるときだけ
  - id: <脚注ラベルに使う安定キー>
    resource: <URL / バンドル相対パス / 母集団の記述>
    title: <出典の表示名>
---
```

`type` 以外はすべて任意（SPEC §4.1・§11）。ただし `description` は本バンドルの lint が必須にしている。

## 書ける構造（YAML 部分集合）

v0.2 で `sources` などのネストが必要になったため、本バンドルは次の部分集合に限定する。
逸脱は `bash scripts/okf-lint.sh docs` の `fm-yaml` が検出する。

- インデントは **0 / 2 / 4 スペースのみ**。タブは使えない
- ブロックシーケンス項目は `  - `（インデント 2）で始める
- シーケンス項目のマッピング継続行はインデント 4
- 複数行スカラー（`|` / `>`）とアンカー・エイリアス（`&` / `*`）は使わない
- 1 行に収まるものはフロー表記でよい（`generated: { by: ..., at: ... }`、`tags: [a, b]`）

```yaml
# ブロックシーケンス（sources / parameters）
sources:
  - id: herdr-socket-api
    resource: https://herdr.dev/docs/socket-api/
    title: herdr — Socket API

# ネストマッピング（executor / attester / usage_window）
executor:
  resource: /references/skills/run-on-bq.md
  receipt: [job_id, executed_sql, result]
```

### 引用符が要るケース

値が次のいずれかを含むときは、**引用符で囲む**。ネストした値にも同じ規則が効く:

| 値に含まれるもの | 引用符なしだとどうなるか |
|---|---|
| 半角スペース + `#`（例: `マーカーは #196 で入った`） | `#` 以降が YAML コメントとして捨てられ、値が途中で切れる |
| コロン + 半角スペース（例: `（bin: totsuka）`） | YAML がマッピングとして解釈しようとしてパースに失敗する |
| 先頭が `#` `&` `*` `!` `%` `@` `` ` `` `,` `\|` `>` の指示文字 | YAML の構文要素と衝突する（`&` `!` はアンカー/タグと解釈され、値が **null になって黙って消える**） |
| 先頭が `-` または `?` で、**その直後が半角スペース** | ブロックシーケンス項目 / 明示キーと解釈されパースに失敗する（`-1` や `?foo` のように直後がスペースでなければ問題ない） |

`#` を全角の `＃` に置き換えるといった回避はしない — 引用符で囲む。

# provenance / trust / lifecycle（v0.2 の中核）

「どこから来たか」「どれだけ信じてよいか」「まだ現役か」を frontmatter から答えられるようにするためのファミリ。
**すべて任意**で、無いこと自体が意味を持つ（未検証の concept は検証済みと区別されるが、拒否はされない）。

## actor 記法（§7）

`generated.by` / `verified[].by` は次のいずれかで書く。`human:` 接頭辞が信頼段階の判定キーになるので、
人間が書いた・人間が確認した内容には必ず `human:` を使う。

| 形式 | 用途 | 例 |
|---|---|---|
| `human:<id>` | 人間 | `human:tomoya-k31` |
| `process:<id>` | 自動プロセス | `process:ci-nightly` |
| `<producer>/<version>` | エージェント・ツール | `claude-code/opus-5` |

## `generated` — 誰がいつ書いたか（§5.2）

v0.1 の `timestamp` は **`generated.at` に置き換わった**（SPEC §13.1 の破壊的変更）。
`by` は必須。意味のある内容変更をしたら `at` を更新する。

```yaml
generated: { by: human:tomoya-k31, at: 2026-07-29T10:00:00Z }
```

## `verified` — 誰がいつ裏取りしたか（§5.2 / §5.3）

`generated` とは独立。**書いた人と確認した人は別**という前提のキー。本バンドルでの運用ルール:

- **実機で動かして確認が取れたときに書く。** 実装して PR を通しただけでは書かない
  （このリポジトリでは「実機検収」を区別して扱ってきた。それが済んだ事実をここに残す）
- CI や定期ジョブが継続的に担保している事実は `process:<id>` で書く
- 複数回の検証は配列で並べる。「どれだけ最近か」は最新の `at` で判断する
- 内容を書き換えても `verified` は自動的には消えない。**古い検証が残っていると誤誘導になる**ので、
  意味のある変更をしたら `verified` を消すか、新しい検証を追記する

```yaml
verified:
  - { by: human:tomoya-k31, at: 2026-07-29T12:00:00Z }
  - { by: process:ci-nightly, at: 2026-07-30T02:00:00Z }
```

信頼段階（§5.3）は保存せず `verified` から導出する。`scripts/okf-search.sh --trust-tier` で絞り込める:

| 段階 | 条件 |
|---|---|
| `unverified` | `verified` が無い |
| `machine-confirmed` | `verified` が `human:` 以外の actor だけ |
| `human-reviewed` | `verified` に `human:<id>` がある |

## `status` — ライフサイクル（§5.4）

**v0.2 で語彙が固定された。`draft` / `stable` / `deprecated` の 3 値以外は書かない**（省略時は `stable` 扱い）。

| 値 | 意味 |
|---|---|
| `draft` | 未レビュー。書きかけでもよい |
| `stable` | 既定。読んでよい |
| `deprecated` | リンクと履歴のために残すが、もう現役ではない。後継へのリンクを本文に書く |

ADR も同じ 3 値を使う（v0.1 まで使っていた `accepted` は v0.2 に無いため `stable` に寄せた）。
ADR 固有のライフサイクル（提案中・却下・置換）は本文の `# Status` 見出しに書く。

## `stale_after` — 賞味期限（§5.5）

相対 TTL ではなく **絶対日付**（`YYYY-MM-DD`）。`today >= stale_after` で stale。運用ルール:

- `/references/`（外部ドキュメントのミラー）には **必ず書く**。外部の仕様は予告なく変わり、
  ミラーが黙って古くなるのが一番危ない。目安は最終確認日から 6 ヶ月
- 外部 API・CLI の挙動に依存する concept（`/apis/`・外部連携の `/components/`）にも推奨
- 内部の設計判断（`/decisions/`）や用語（`/glossary/`）には基本的に不要 — 古くなるのではなく
  `deprecated` になる性質のものなので `status` で表す
- 期限切れは lint が `[W] stale` で警告する。放置せず、中身を確認して期限を延ばすか内容を直す

## `sources` — 出典（§5.1）

v0.1 の本文 `# Citations` リストは **`sources` に置き換わった**（SPEC §13.1 の破壊的変更）。
本文に出典リストを作らない。

- `resource` は各エントリで **必須**。URL・バンドル相対パスのほか、
  たどれない母集団の記述（例: `全 PR のレビューコメント`）でもよい
- `id` は本文から参照するときの安定キー。順序が変わっても壊れないよう、番号ではなく名前を使う
- 出典の信頼シグナル（`author` / `usage_count` / `last_modified`）は分かる範囲で書く。
  **推測で埋めない** — 無いことは「不明」という情報になる

本文の特定の主張に出典を紐づけるときは、`sources[].id` をラベルにした Markdown 脚注を使う:

```markdown
`events_` テーブルは `events_YYYYMMDD` として日次シャードされる。[^ga4-schema]

[^ga4-schema]: GA4 BigQuery Export schema
```

脚注ラベルと `sources[].id` の対応は lint が `[W] footnote` で検査する。

# クロスリンク

- バンドルルート相対で書く: `[顧客テーブル](/data/customers.md)`（`/` は `docs/` を指す）
- 相対リンク（`./other.md`）は同一ディレクトリ内のみ許容
- リンク先が未執筆でもよい（SPEC §6.1）が、CI が警告を出すので意図的な場合のみ残す

# 本文の見出し規約

該当する場合はこの見出し名を使う: `# Schema`（テーブル・データ構造）、`# Examples`（使用例）、
`# Computation`（Attested Computation の計算式本体）。
`# Citations` は v0.2 で廃止 — 出典は frontmatter の `sources` に書く。

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
| `/references/` | 外部資料の要約ミラー（`sources` の参照先） | `Reference` |

新しい type が必要な場合は自由に定義してよい（OKF は type を中央登録しない）が、まず上の語彙で表現できないか検討し、新設したらこの表に追記すること。

## `Attested Computation`（§10）

「値の意味」だけでなく「その値を出す正規の計算方法」を持つ concept 型。数値を出す計算式を
サンクション化し、エージェントが勝手に書き換えた式で計算していないことを消費側が確認できるようにする。

本バンドルには現状この type の concept は無い（サンクション化すべき計算式がまだ無い）。
lint は書式だけ受け付ける状態にしてあるので、必要になったら SPEC §10 に従って書けばよい。
その際 `runtime` は必須。計算式は本文の `# Computation` 見出しの下にフェンスで書くか、
`computation` キーでファイルを指す。

# ドキュメントを残すタイミング（トリガー定義）

以下のイベントが発生する作業を行ったら、**同一 PR 内で** 対応するドキュメントを作成・更新する。コードだけの PR にしない。

| トリガーとなるイベント | 作成/更新するもの | 期限 |
|---|---|---|
| 技術選定・アーキテクチャ上の意思決定（ライブラリ採用、方式変更、トレードオフ判断） | `/decisions/adr-NNNN-*.md` を新規作成 | 実装 PR と同時 |
| 新しいパッケージ・サービス・モジュールの追加 | `/components/` に concept 新規作成 | 同一 PR |
| 既存コンポーネントの責務・公開IFの変更 | 該当 `/components/*.md` を更新、`generated.at` 更新 | 同一 PR |
| API エンドポイント・イベント・Webhook の追加/破壊的変更 | `/apis/` を作成/更新 | 同一 PR |
| DB スキーマ・キュー・データモデルの変更 | `/data/` を作成/更新（`# Schema` 見出し） | 同一 PR |
| インフラ・環境・IaC の変更 | `/infrastructure/` を作成/更新 | 同一 PR |
| インシデント発生・対応 | `/operations/` に Playbook/Postmortem | 収束後 2 営業日以内 |
| 新しいアラート・監視の追加 | `/operations/` にトリアージ手順 | 同一 PR |
| リリース（バージョンタグ付与） | `/releases/` に Release ノート | リリース時 |
| 新しいドメイン用語・略語の導入 | `/glossary/` に 1 ファイル | 初出の PR |
| 実機で動作を確認できた | 該当 concept に `verified` を追記 | 確認した PR |
| ドキュメントの前提が崩れた（廃止・置換） | 該当ファイルの `status: deprecated` 化と後継へのリンク。削除はしない | 気づいた PR |

判断に迷ったら「3ヶ月後の自分/別のエージェントがこの判断・構造を再構築できるか」を基準にする。できないなら書く。

# index.md のルール

- **すべてのディレクトリ**に `index.md` を置く（progressive disclosure の要）
- concept ファイルまたはサブディレクトリを追加・改名・削除したら、**同じコミットで**そのディレクトリの `index.md` を更新する
- エントリ形式: `* [Title](file.md) - frontmatter の description をそのまま転記`
  - **全文をそのまま**転記する（要約・省略・追記をしない）。lint の `index-desc` が
    frontmatter との一致を検査するので、description を書き換えたら index も同じコミットで直す
  - description を引用符で囲んでいる場合、index には**引用符を外した中身**を転記する
- ルート `/index.md` のみ frontmatter（`okf_version: "0.2"` 宣言）を持つ。他の index.md に frontmatter を書かない
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

構造チェック:

| チェック | 内容 |
|---|---|
| `frontmatter` | frontmatter ブロックがある |
| `fm-yaml` | frontmatter が上の部分集合として壊れていない（インデント・引用符・`: `・` #`・重複キー・タブ） |
| `type` | 空でない `type` がある |
| `description` | 空でない `description` がある |
| `index-fm` | ルート以外の `index.md` に frontmatter が無い |
| `index-exists` | concept を含むディレクトリに `index.md` がある |
| `index-listed` | 各 concept / サブディレクトリが `index.md` から張られている |
| `index-desc` | `index.md` の転記が frontmatter の `description` と一致する |
| `log-format` | `log.md` の日付見出しが `## YYYY-MM-DD` |

v0.2 ファミリのチェック:

| チェック | 内容 |
|---|---|
| `status` | `draft` / `stable` / `deprecated` のいずれか |
| `actor` | `generated.by` / `verified[].by` が actor 記法 |
| `generated` | `generated` があるとき `by` がある |
| `datetime` | `generated.at` / `verified[].at` が ISO 8601、`stale_after` 等が `YYYY-MM-DD` |
| `sources` | `sources` の各エントリに `resource` がある |
| `computation` | `type: Attested Computation` に `runtime` がある |
| `legacy` | v0.1 の `timestamp` / 本文 `# Citations` が残っていない |
| `[W] stale` | `stale_after` を過ぎていない |
| `[W] footnote` | 本文の脚注ラベルに対応する `sources[].id` がある |
| `[W] okf-version` | ルート `index.md` の `okf_version` が `"0.2"` |

PostToolUse hook（`.claude/settings.json`）により、docs 配下の編集後に自動で lint が走る。エラーが返った場合は必ず修正してから作業を完了すること。

v0.1 形式で書かれたファイルを機械的に v0.2 へ寄せる変換スクリプトがある（冪等・`--dry-run` 付き）:

```bash
bash scripts/okf-migrate-v02.sh docs --dry-run
```
