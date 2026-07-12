# components

パッケージ/サービス単位の責務・公開インターフェース・依存先。

* [okf-search](okf-search.md) - docs/ の frontmatter（type/status/owner/resource/tags/timestamp）でconceptを絞り込むCLIスクリプトと、絞り込み結果をAIが読んで抽出するokf-searchスキル。
* [orchestrator-core](orchestrator-core.md) - totsuka のコア。ヘキサゴナルアーキテクチャの domain / ports / adapters を担う。
* [orchestrator-cli](orchestrator-cli.md) - totsuka の CLI エントリポイント（bin: totsuka）。#45 時点では --version/--help のみ。
* [plugin-protocol](plugin-protocol.md) - プラグイン開発者向けに公開する型定義クレート。JSON-RPC 2.0（NDJSON）・manifest・capabilities・§11 メソッド型・Task 共通スキーマ・プロトコルバージョニングを提供する、プラグイン境界の単一の正。
* [task-source-github](task-source-github.md) - GitHub Issues / ProjectsV2 をタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。GraphQL で fetch→正規化、ProjectsV2 ステータス書き戻し、Issue コメント publish を行う。
* [task-source-notion](task-source-notion.md) - Notion データベースをタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。プロパティマッピングで任意の DB 構造を Task へ正規化し、ステータス書き戻しとページ本文への結果追記を行う。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
