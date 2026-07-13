---
type: Term
title: Task（タスク）
description: タスクソース由来の作業単位。共通スキーマ（plugin-protocol の Task 型）に正規化され、状態DBの1行として9状態のステートマシン（F-71）を遷移する。
tags: [glossary, domain]
timestamp: 2026-07-13T04:30:00Z
status: active
owner: tomoya-k31
---

# Task（タスク）

GitHub Issue や Notion ページなど、タスクソースが提供する作業1件。task_source プラグインが共通スキーマ（[plugin-protocol](/components/plugin-protocol.md) の `Task` 型: id / source / title / body / repo_hint / labels / priority / status / url / assignee）へ正規化し、Orchestrator が [state.db](/data/state-db.md) に冪等に取り込む（F-73）。取り込み後は queued → dispatched → running → publishing → done などの9状態を遷移する（F-71）。CLI では `totsuka task list / show / cancel / retry` で操作する。
