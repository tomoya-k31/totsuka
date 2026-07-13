---
type: Term
title: Task Source（タスクソース）
description: タスクの供給元（GitHub / Notion 等）。task_source プラグインが tasks/fetch・task/update_status・result/publish を実装して接続する。
tags: [glossary, plugin]
timestamp: 2026-07-13T04:30:00Z
status: active
owner: tomoya-k31
---

# Task Source（タスクソース）

タスクの供給元となる外部システム。`task_source` kind のプラグイン（例: [task-source-github](/components/task-source-github.md)、[task-source-notion](/components/task-source-notion.md)）が、トリガー条件付きの `tasks/fetch`、完了時のステータス書き戻し `task/update_status`（F-84）、成果物の書き戻し `result/publish`（F-07）を実装する。config.toml の `[plugins.{name}]` インスタンス名がワークフローの `source` と対応する。
