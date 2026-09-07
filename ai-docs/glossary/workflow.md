---
type: Term
title: Workflow（ワークフロー）
description: projects × trigger × mode × agent × output の名前付き束ね（F-80）。タスクは定義順の first-match で最大1つのワークフローに割り当てられる（F-81）。タスクソースは名指した projects の所有者として導出される。mode / output / verification は profile の 4 原型でまとめて指定することもできる。
tags: [glossary, domain]
generated: { by: claude-code/opus-5, at: 2026-09-07T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Workflow（ワークフロー）

「**どの domain の**・どんな条件のタスクを・どのモードで・どのエージェントに実行させ・結果をどう出力するか」を束ねる名前付き定義（F-80、config.toml の `[[workflows]]`）。domain は `projects` で名指す [Project](/glossary/project.md) の配列で、[タスクソース](/glossary/task-source.md)はその所有者として導出される（#626、[ADR-0069](/decisions/adr-0069-workflow-projects.md)。それまでは `source` でプラグインを直接名指し、そのプラグインの全 domain が対象だった）。トリガーの意味はソースプラグインが解釈し、Orchestrator は status / labels を防御的に再判定する。マッチは定義順の first-match（F-81）。mode は `plan`（設計のみ。ペインが git を実行できないので push も PR も起きない F-82）と `implement`、output は `source` / `none`（F-83。`pull_request` は push・PR 作成がエージェントの責務になった時点で廃止 → [ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）。`on_success` / `on_failure` でソース側ステータスの書き戻しを指定する（F-84）。

mode / output / verification は個別に書くほか、**原型（profile）**でまとめて指定できる（#394、[ADR-0033](/decisions/adr-0033-workflow-profile.md)）。`answer` / `triage` / `design` / `implement` の 4 値で、噛み合う組み合わせに名前を付けたもの。2 値の mode では「worktree は read-only だが外部（GitHub / Notion）へは書く」形（triage / design）が表現できないのが導入理由で、解決テーブルは設定ではなく Rust に固定されている。
