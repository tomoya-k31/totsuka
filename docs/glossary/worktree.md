---
type: Term
title: worktree（ワークツリー）
description: タスク専用の git 作業ディレクトリ。「1 task = 1 repo = 1 worktree = 1 branch」の正規化単位で、完了後は掃除ポリシー（immediate / retention_days / manual）が適用される。
tags: [glossary, git]
timestamp: 2026-07-13T04:30:00Z
status: active
owner: tomoya-k31
---

# worktree（ワークツリー）

`git worktree` で作られるタスク専用の作業ディレクトリ（F-20〜25）。「1 task = 1 repo = 1 worktree = 1 branch」を不変条件とし、ブランチ名（既定 `agent/{source}-{task_id}`）と配置先はテンプレートで決まる。タスク完了・キャンセル後は掃除ポリシー（`immediate` / `{ retention_days = N }` / `manual`、F-23/85）が適用されるが、未コミット変更があるものは決して削除しない。どのタスクにも属さないものは孤児（orphan）として `totsuka doctor` が検出・掃除を提案する（F-24）。
