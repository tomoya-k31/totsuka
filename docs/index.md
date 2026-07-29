---
okf_version: "0.2"
---

# Knowledge Bundle Index

このバンドルの目次。各ディレクトリの `index.md` から個別の concept に辿れる。
執筆・更新ルールは [CLAUDE.md](./CLAUDE.md) を参照。

# 設計・意思決定

* [architecture/](architecture/) - システム構成・依存関係・非機能要件
* [decisions/](decisions/) - ADR（Architecture Decision Records）
* [product/](product/) - 機能仕様・ユースケース

# 実装

* [components/](components/) - パッケージ/サービス単位の責務と公開インターフェース
* [apis/](apis/) - APIエンドポイント・イベント・Webhookの意味と利用文脈
* [data/](data/) - テーブル・データモデル・キューの定義と設計意図

# 基盤・運用

* [infrastructure/](infrastructure/) - GCP構成・環境・IaCモジュール
* [operations/](operations/) - 障害対応Playbook・デプロイ手順・アラート対応
* [security/](security/) - 脅威モデル・認可設計・脆弱性対応方針
* [releases/](releases/) - リリースノート・マイグレーション手順

# 開発・知識

* [development/](development/) - 環境構築・規約・ブランチ戦略
* [quality/](quality/) - テスト戦略・既知の不具合パターン
* [glossary/](glossary/) - ドメイン用語・社内略語
* [references/](references/) - 外部資料の要約ミラー
