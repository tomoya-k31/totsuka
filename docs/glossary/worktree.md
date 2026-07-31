---
type: Term
title: worktree（ワークツリー）
description: タスク専用の git 作業ディレクトリ。「1 task = 1 repo = 1 worktree = 1 branch」の正規化単位で、完了後は掃除ポリシー（immediate / retention_days / keep_7d / keep_28d / manual）が「判定 → pane 解放 → 削除」の3段で適用される。
tags: [glossary, git]
generated: { by: human:tomoya-k31, at: 2026-07-22T13:00:00Z }
status: stable
owner: tomoya-k31
---

# worktree（ワークツリー）

`git worktree` で作られるタスク専用の作業ディレクトリ（F-20〜25）。「1 task = 1 repo = 1 worktree = 1 branch」を不変条件とし、ブランチ名（既定 `agent/{source}-{task_id}`）と配置先はテンプレートで決まる。**配置先のディレクトリ名はブランチ名からは作らない** — 既定は `{source}-{task_id}` を git ref 規則で正規化した `{worktree_name}`（Slack なら `slack-C0ABCDEF12-1720000000.123456`）で、worktree を作る時点でブランチ名がまだ確定していない場合にも使えるようにするため。タスク完了・キャンセル後は掃除ポリシー（`immediate` / `{ retention_days = N }` / その糖衣 `keep_7d`・`keep_28d` / `manual`、F-23/85）が適用されるが、未コミット変更があるものは決して削除しない。どのタスクにも属さないものは孤児（orphan）として `totsuka doctor` が検出・掃除を提案する（F-24）。

掃除は「**判定**（decide: `Remove` / `Retain` / `Dirty`）→ **pane 解放**（`Remove` のときだけ `session/release` で herdr pane を閉じる）→ **削除**（remove: dirty を再チェックしてから `git worktree remove`）」の3段で実行される（#210、[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）。pane の寿命はこの掃除ポリシーに連動し、`Retain` / `Dirty` の worktree は pane も保持される（人間の導線）。
