# Bundle Update Log

## 2026-07-12
* **Update**: リポジトリ自動選択（#56）。[orchestrator-core](/components/orchestrator-core.md) に `repo_select`（ルール＋LLM フォールバック、README hash キャッシュ）、`ports::LlmRouter`、`adapters::llm::OpenAiRouter`（reqwest・指数バックオフ）を追加。
* **Update**: 並列実行制御（#55）。[orchestrator-core](/components/orchestrator-core.md) に `scheduler`（3 階層スロット管理・DB 再構築・優先度キュー・waiting_input のスロット解放 F-45）を追加。
* **Update**: ワークフロー定義とトリガーマッチング（#54）。[orchestrator-core](/components/orchestrator-core.md) に `domain::workflow`（Workflow/Trigger/OutcomeAction、定義順 first-match マッチング、plan×pull_request・output=source capability・trigger 重複の検証）を追加。
* **Update**: git worktree ライフサイクル管理（#53）。[orchestrator-core](/components/orchestrator-core.md) に `worktree` モジュールと `ports::GitRunner` / `adapters::git::SystemGitRunner` を追加。作成（origin/{default} を commit 解決して分岐・並列安全）・掃除（ポリシー別、未コミットは skip）・孤児検出を実装。実 git（ローカル bare を origin）で結合テスト。
* **Update**: プラグイン管理コマンド（#52）。[orchestrator-core](/components/orchestrator-core.md) に `plugins::store`（install/uninstall/list、prepare→confirm→commit・SHA-256・manifest 検証）と `config::edit`（toml_edit で enabled 書き換え・整形保持）を追加。[orchestrator-cli](/components/orchestrator-cli.md) に `plugin` サブコマンドを配線。
* **Update**: [orchestrator-core](/components/orchestrator-core.md) に `adapters::plugin_host`（#51）を追加。プラグインをサブプロセス起動し NDJSON JSON-RPC で通信（tokio）。ライフサイクル・protocol 互換チェック・リクエスト相関・タイムアウト・クラッシュ隔離・config/validate 委譲。結合テスト用の mock_plugin バイナリ同梱。
* **Update**: [plugin-protocol](/components/plugin-protocol.md)（#50）を雛形から本実装へ。JSON-RPC 2.0（NDJSON）型・§11 メソッド型・plugin.toml manifest・capabilities・Task 共通スキーマ・プロトコルバージョニングを実装。
* **Creation**: [ログ規約（JSON Lines・機密マスキング）](/development/logging-conventions.md)（#49）。`Convention` type を追加。[orchestrator-core](/components/orchestrator-core.md) に `logging` モジュール（redact/layer/rotation）を追加し、`[log]` 設定を config スキーマへ追加。
* **Creation**: [状態DB（SQLite state.db）スキーマ](/data/state-db.md)（#48）。tasks/sessions/events/schema_migrations の DDL と設計判断、タスクステートマシン（F-71）、多重起動防止（F-74）を記録。`Data Model` type を追加。[orchestrator-core](/components/orchestrator-core.md) に `domain::state` / `adapters::state_db` / `adapters::run_lock` を追記。
* **Update**: 設定ロードとシークレット参照解決（#47）。[orchestrator-core](/components/orchestrator-core.md) に `config` モジュール（schema/raw/resolve/layered/validate）を追加。`config.toml`+`plugins/{name}.toml` の二層設定パース、`${ENV}`/`keychain:` 解決、優先順位マージ、静的検証（disable 中プラグイン参照エラー含む）。
* **Update**: XDG パス解決と platform 抽象（#46）。[orchestrator-core](/components/orchestrator-core.md) に `paths` / `platform` モジュールと `SecretStore` / `ProcessProbe` / `SecretString` / `SecretRef` を追加（macOS Keychain を `platform::macos` に隔離）。
* **Creation**: Rust workspace 実装土台（#45）。[ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md) と 3 crate の component doc（[orchestrator-core](/components/orchestrator-core.md) / [orchestrator-cli](/components/orchestrator-cli.md) / [plugin-protocol](/components/plugin-protocol.md)）を作成。
* **Creation**: Orchestrator 要件定義書（Draft v0.2）を機能仕様として取り込み [totsuka — Local AI-Agent Orchestrator Requirements (v1)](/product/orchestrator-spec.md) を作成（英語 canonical + [日本語版](/product/orchestrator-spec.ja.md)）。

## 2026-07-11
* **Initialization**: OKF v0.1 準拠のバンドル構造を作成。ディレクトリ構成と [index](/index.md) を確立。
* **Creation**: 執筆ルール [CLAUDE.md](/CLAUDE.md) と利用ガイド [README.md](/README.md) を作成。
* **Creation**: 最初のADR [OKFによるドキュメント管理の採用](/decisions/adr-0001-adopt-okf.md) を作成。
* **Creation**: frontmatter 横断検索ツール [okf-search](/components/okf-search.md) を作成。`Tool` type を新設。
# Log

