---
type: Term
title: Workflow（ワークフロー）
description: source × trigger × mode × agent × output の名前付き束ね（F-80）。タスクは定義順の first-match で最大1つのワークフローに割り当てられる（F-81）。
tags: [glossary, domain]
generated: { by: human:tomoya-k31, at: 2026-07-13T04:30:00Z }
status: stable
owner: tomoya-k31
---

# Workflow（ワークフロー）

「どのソースの・どんな条件のタスクを・どのモードで・どのエージェントに実行させ・結果をどう出力するか」を束ねる名前付き定義（F-80、config.toml の `[[workflows]]`）。トリガーの意味はソースプラグインが解釈し、Orchestrator は status / labels を防御的に再判定する。マッチは定義順の first-match（F-81）。mode は `plan`（設計のみ。ペインが git を実行できないので push も PR も起きない F-82）と `implement`、output は `source` / `none`（F-83。`pull_request` は push・PR 作成がエージェントの責務になった時点で廃止 → [ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）。`on_success` / `on_failure` でソース側ステータスの書き戻しを指定する（F-84）。
