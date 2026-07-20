---
type: Term
title: Task Source（タスクソース）
description: タスクの供給元（GitHub / Notion 等）。task_source プラグインが task/submit（push）・task/update_status・result/publish を実装して接続する。
tags: [glossary, plugin]
timestamp: 2026-07-20T18:00:00Z
status: active
owner: tomoya-k31
---

# Task Source（タスクソース）

タスクの供給元となる外部システム。`task_source` kind のプラグイン（例: [task-source-github](/components/task-source-github.md)、[task-source-notion](/components/task-source-notion.md)）が、トリガー条件に合致するタスクを自ら push する `task/submit`（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)、プロトコル 0.2.0 以降は唯一の取り込み経路）、完了時のステータス書き戻し `task/update_status`（F-84）、成果物の書き戻し `result/publish`（F-07）を実装する。config.toml の `[plugins.{name}]` インスタンス名がワークフローの `source` と対応する。
