---
type: Decision
title: ADR-0001 OKFによるドキュメント管理の採用
description: リポジトリ内ドキュメントをOKF準拠のKnowledge Bundleとして管理する決定。採用時点の準拠バージョンはv0.1で、v0.2への追従はADR-0022で決めた。
tags: [documentation, okf]
generated: { by: human:tomoya-k31, at: 2026-07-29T00:00:00Z }
status: stable
sources:
  - id: okf-spec
    resource: https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md
    title: OKF SPEC
---

# Status

Accepted — 2026-07-11

# Context

ソースコードとドキュメントを同一リポジトリで管理するにあたり、人間とAIエージェントの双方が読み書きできる、ツール非依存のフォーマットが必要だった。

# Decision

`docs/` を [OKF](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md) 準拠の Knowledge Bundle とする。

- 全 concept に frontmatter + `type` を必須とする
- 全ディレクトリに `index.md` を置き progressive disclosure を担保する
- `log.md` はバンドルルートのみに置く
- 準拠チェックは `scripts/okf-lint.sh` で CI / Claude hooks の両方から実行する

本 ADR を書いた時点の準拠バージョンは v0.1 だった。バージョン追従そのものは本 ADR の
対象外とし、v0.2 への移行は [ADR-0022](/decisions/adr-0022-okf-v02-migration.md) で個別に決めている。

# Consequences

- ドキュメントの追加・変更は同一PRで index/log の更新を伴う（lint で強制）
- type 語彙は [CLAUDE.md](/CLAUDE.md) の表を正とし、新設時はそこに追記する
