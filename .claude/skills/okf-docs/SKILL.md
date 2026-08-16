---
name: okf-docs
description: ai-docs/ 配下（OKF Knowledge Bundle）へのドキュメント作成・更新を行うときに必ず使う。トリガー: ADR作成、設計判断の記録、コンポーネント/API/スキーマ/インフラの追加・変更、Playbook作成、リリースノート、用語追加、「ドキュメントを書いて」「ADRにして」「docsを更新」等の依頼。コード変更のPRでドキュメント更新が必要かの判断にも使う。
---

# OKF Docs Skill

`ai-docs/` は OKF v0.2 準拠の Knowledge Bundle。詳細ルールの正本は `ai-docs/CLAUDE.md`。
このスキルは「いつ・どこに・何を・どの順で」書くかの手順を定義する。

## 手順（必ずこの順で）

1. **ルールを読む**: `ai-docs/CLAUDE.md` を読む（既にコンテキストにあれば省略可）
2. **配置先を決める**: `ai-docs/CLAUDE.md` のディレクトリ表から選ぶ。迷ったら
   - 「なぜそうしたか」→ `decisions/`（ADR）
   - 「何がどう動くか」→ `components/` or `architecture/`
   - 「どう対応するか」→ `operations/`
3. **既存の重複を確認**: 配置先ディレクトリの `index.md` を読み、既存 concept の更新で済むなら新規作成しない
4. **concept を作成/更新**: frontmatter テンプレート（`ai-docs/CLAUDE.md` 参照）に従う
   - `type` は必須。語彙表から選び、新設したら `ai-docs/CLAUDE.md` の表に追記
   - `description` は必ず1文で書く（index 転記に使うため）
   - `generated: { by: <actor>, at: <ISO 8601> }` の `at` を現在時刻に更新する
     （v0.2 で旧 `timestamp` を置き換えたキー。`by` は actor 記法 = `human:<id>` /
     `process:<id>` / `<producer>/<version>`）
   - `status` は `draft` / `stable` / `deprecated` の 3 値のみ（省略時 `stable`）
   - 出典があれば本文ではなく frontmatter の `sources` に書く（`# Citations` は v0.2 で廃止）
   - 実機で確認が取れた事実があれば `verified: { by: human:<id>, at: ... }` を足す
   - 他 concept への言及はバンドルルート相対リンク `[title](/dir/file.md)` にする
5. **ログ断片を書く**: `ai-docs/log.d/YYYY-MM-DD-<slug>.md` を**新規作成**し、
   `* **Creation|Update|Deprecation**: 説明と [リンク](/dir/file.md)` を書く。
   - `ai-docs/log.md` は生成物なので**直接編集しない**（全 PR が同じ行に書き込む構造をやめ、
     並行 PR の決定論的コンフリクトを消すため。[ADR-0031](/decisions/adr-0031-docs-ledger-conflicts.md)）
   - `<slug>` は**必須**で、同日の別 PR とファイル名が衝突しないよう
     issue 番号や題材などその変更に固有の語にする（`356-pane-layout` 等）
   - 断片に `## YYYY-MM-DD` 見出しは書かない（日付はファイル名から取られる）
6. **生成物を作り直す**: 新規作成・改名・削除をした場合は index も併せて正規化する。
   `index.md` の一覧行と `log.md` は**手で書かない**:

   ```bash
   bash scripts/okf-log-build.sh    # ai-docs/log.md を断片から生成
   bash scripts/okf-index-build.sh  # 各 index.md のマーカー区間を正規化
   ```

   並び順と表示タイトルは `index.md` 側の既存の値が保存されるので、
   新しい concept を望みの位置に置きたいときは生成後に行を移動してよい。
7. **lint を実行**: `bash scripts/okf-lint.sh ai-docs` を実行し、エラーがゼロになるまで修正する
   （`log-sync` / `index-sync` が落ちたら手順 6 を実行し忘れている）

## コード変更時のドキュメント要否判定

コードを変更する作業では、完了前に `ai-docs/CLAUDE.md` の
「ドキュメントを残すタイミング」表と照合し、該当するイベントがあれば
**同じ作業内で** 手順 1〜7 を実行する。該当例:

- 新しい依存ライブラリの採用・アーキテクチャ変更 → ADR
- 新パッケージ/サービス → components
- API/イベントの追加・破壊的変更 → apis
- マイグレーション・スキーマ変更 → data
- IaC/環境変更 → infrastructure

該当しない場合（リファクタ、typo修正、テスト追加のみ等）はドキュメント不要。

## 禁止事項

- `index.md` / `log.md` という名前で concept を作らない（予約ファイル名）
- **`ai-docs/log.md` を直接編集しない**（生成物。書くのは `ai-docs/log.d/` の断片）
- **`index.md` のマーカー区間を手で書かない**（`* [Title](file.md) - …` の行は
  `okf-index-build.sh` の管轄。前後の散文とサブディレクトリ行は手で書いてよい）
- `ai-docs/log.d/` を concept 置き場にしない（frontmatter も index 掲載も無い材料ディレクトリ）
- ルート以外の `index.md` に frontmatter を書かない
- 廃止された concept を削除しない（`status: deprecated` にして後継へリンク）
- `ai-docs/` 全体を無差別に読み込まない（index.md からの progressive disclosure で辿る）
- 検証していない事実に `verified` を書かない（実機で確認した事実だけを記録する）
- 出典の信頼シグナル（`author` / `usage_count` / `last_modified`）を推測で埋めない
