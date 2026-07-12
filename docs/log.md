# Bundle Update Log

## 2026-07-12
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

