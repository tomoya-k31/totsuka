---
type: Guide
title: docs バンドルの読み方・書き方
description: このディレクトリはOKF準拠のKnowledge Bundle。人間向けの利用ガイド。
tags: [meta, okf]
---

# docs/ — Knowledge Bundle

このディレクトリは [Open Knowledge Format (OKF) v0.1](https://raw.githubusercontent.com/GoogleCloudPlatform/knowledge-catalog/refs/heads/main/okf/SPEC.md) に準拠した Knowledge Bundle です。人間とAIエージェントの両方が読み書きすることを前提にしています。

## 読み始める場所

- 目次: [index.md](./index.md) — 何がどこにあるか
- 更新履歴: [log.md](./log.md) — 最近何が変わったか
- 執筆ルール: [CLAUDE.md](./CLAUDE.md) — frontmatter・type語彙・更新タイミングの正本（人間もこれに従う）

## 最低限のルール（3つだけ）

1. `index.md` / `log.md` 以外の `.md` には、必ず YAML frontmatter と `type` を書く
2. ファイルを足したり消したりしたら、そのディレクトリの `index.md` と ルートの `log.md` を同じコミットで更新する
3. 他ドキュメントへのリンクは `[title](/data/example.md)` のようにバンドルルート相対で書く

詳細ルール・ディレクトリごとの役割・type 一覧・「いつ書くか」の定義はすべて [CLAUDE.md](./CLAUDE.md) にあります。

## 検証

```bash
bash scripts/okf-lint.sh docs
```

CI（GitHub Actions）でも同じチェックが走ります。ローカルで通してから push してください。
