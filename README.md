# Totsuka

AIを使用した開発フロー自動化ツール

## 概要

Totsuka は、開発フローの半自動化、タスク指示の自動検知、およびそれらを AI Agent にオーケストレーション・割り振りするツールを提供します。

Socket API を経由して herdr と連携し、AI Agent を操作・制御することで、タスク実行の自動化を実現します。

## ステータス

- 言語・ディレクトリ構成: 検討中


## ドキュメント

このリポジトリに関する知識（設計・意思決定・運用手順・用語）はすべて
[`docs/`](./docs/) で管理しています。`docs/` は
[Open Knowledge Format (OKF) v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)
準拠の Knowledge Bundle です。

- 目次: [docs/index.md](./docs/index.md)
- 更新履歴: [docs/log.md](./docs/log.md)
- 執筆ルール（人間・エージェント共通）: [docs/CLAUDE.md](./docs/CLAUDE.md)

ドキュメントを追加・変更する PR は CI（`okf-lint`）で
frontmatter・index 掲載・ログ形式が検証されます。ローカルでは
`bash scripts/okf-lint.sh docs` で事前確認できます。
