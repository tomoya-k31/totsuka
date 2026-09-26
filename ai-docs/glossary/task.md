---
type: Term
title: Task（タスク）
description: タスクソース由来の作業単位。共通スキーマ（plugin-protocol の Task 型）に正規化され、状態DBの1行として9状態のステートマシン（F-71）を遷移する。
tags: [glossary, domain]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T23:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Task（タスク）

GitHub の Issue や PR、Notion ページなど、タスクソースが提供する作業1件（GitHub Project 上の PR から取り込まれたものは [PR タスク](/glossary/pr-task.md)）。task_source プラグインが共通スキーマ（[plugin-protocol](/components/plugin-protocol.md) の `Task` 型: id / source / title / body / repo_hint / labels / priority / status / url / assignee）へ正規化し、Orchestrator が [state.db](/data/state-db.md) に冪等に取り込む（F-73）。取り込み後は queued → dispatched → running → publishing → done などの9状態を遷移する（F-71）。CLI では `totsuka task list / show / cancel / retry` で操作する。

## 2 つの id

タスクには id が 2 つある。取り違えると別のタスクを操作するので、コードでは型を分けている（#765）。

| id | 何か | どこで使うか | Rust の型 |
|---|---|---|---|
| 行 id（`tasks.id`） | totsuka が取り込み時に振る連番 | `totsuka task show <id>`・`status` の ID 列・`JobId`（`job-<task>-<session>`）・`{task_number}` | `domain::TaskId` |
| ソース側の id（`source_task_id`） | タスクソースが持つ id（Issue 番号、Notion のページ id、Slack のスレッドキー） | pane label・plugin protocol の `Task.id` | `domain::SourceTaskId` |
