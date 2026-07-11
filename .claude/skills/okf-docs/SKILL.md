---
name: okf-docs
description: docs/ 配下（OKF Knowledge Bundle）へのドキュメント作成・更新を行うときに必ず使う。トリガー: ADR作成、設計判断の記録、コンポーネント/API/スキーマ/インフラの追加・変更、Playbook作成、リリースノート、用語追加、「ドキュメントを書いて」「ADRにして」「docsを更新」等の依頼。コード変更のPRでドキュメント更新が必要かの判断にも使う。
---

# OKF Docs Skill

`docs/` は OKF v0.1 準拠の Knowledge Bundle。詳細ルールの正本は `docs/CLAUDE.md`。
このスキルは「いつ・どこに・何を・どの順で」書くかの手順を定義する。

## 手順（必ずこの順で）

1. **ルールを読む**: `docs/CLAUDE.md` を読む（既にコンテキストにあれば省略可）
2. **配置先を決める**: `docs/CLAUDE.md` のディレクトリ表から選ぶ。迷ったら
   - 「なぜそうしたか」→ `decisions/`（ADR）
   - 「何がどう動くか」→ `components/` or `architecture/`
   - 「どう対応するか」→ `operations/`
3. **既存の重複を確認**: 配置先ディレクトリの `index.md` を読み、既存 concept の更新で済むなら新規作成しない
4. **concept を作成/更新**: frontmatter テンプレート（`docs/CLAUDE.md` 参照）に従う
   - `type` は必須。語彙表から選び、新設したら `docs/CLAUDE.md` の表に追記
   - `description` は必ず1文で書く（index 転記に使うため）
   - `timestamp` を現在時刻（ISO 8601）に更新
   - 他 concept への言及はバンドルルート相対リンク `[title](/dir/file.md)` にする
5. **index.md を更新**: 新規作成・改名・削除をした場合、同ディレクトリの `index.md` に
   `* [Title](file.md) - description転記` 形式で追記/修正する
6. **log.md を更新**: `docs/log.md` の先頭（今日の `## YYYY-MM-DD` 見出し。無ければ作る）に
   `* **Creation|Update|Deprecation**: 説明と [リンク](/dir/file.md)` を追記する
7. **lint を実行**: `bash scripts/okf-lint.sh docs` を実行し、エラーがゼロになるまで修正する

## コード変更時のドキュメント要否判定

コードを変更する作業では、完了前に `docs/CLAUDE.md` の
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
- ルート以外の `index.md` に frontmatter を書かない
- 廃止された concept を削除しない（`status: deprecated` にして後継へリンク）
- `docs/` 全体を無差別に読み込まない（index.md からの progressive disclosure で辿る）
