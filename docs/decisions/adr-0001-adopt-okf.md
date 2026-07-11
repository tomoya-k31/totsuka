---
type: Decision
title: ADR-0001 OKFによるドキュメント管理の採用
description: リポジトリ内ドキュメントをOKF v0.1準拠のKnowledge Bundleとして管理する決定。
tags: [documentation, okf]
timestamp: 2026-07-11T00:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-11

# Context

ソースコードとドキュメントを同一リポジトリで管理するにあたり、人間とAIエージェントの双方が読み書きできる、ツール非依存のフォーマットが必要だった。

# Decision

`docs/` を [OKF v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md) 準拠の Knowledge Bundle とする。

- 全 concept に frontmatter + `type` を必須とする
- 全ディレクトリに `index.md` を置き progressive disclosure を担保する
- `log.md` はバンドルルートのみに置く
- 準拠チェックは `scripts/okf-lint.sh` で CI / Claude hooks の両方から実行する

# Consequences

- ドキュメントの追加・変更は同一PRで index/log の更新を伴う（lint で強制）
- type 語彙は [CLAUDE.md](/CLAUDE.md) の表を正とし、新設時はそこに追記する

# Citations

[1] [OKF SPEC v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md)
