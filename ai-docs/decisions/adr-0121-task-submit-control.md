---
type: Decision
title: ADR-0121 外からのタスク投入 — totsuka task submit と POST /task/submit
description: 実行中の totsuka に外からタスクを 1 件投入する口（CLI totsuka task submit → 制御 UDS の POST /task/submit）を足し、routine のエージェントが PR ごとに呼んで 1 PR = 1 タスクの fan-out をできるようにする決定。タスクは指定した workflow の source のものとして既存の task/submit 経路で取り込み、予約 source もプロトコル変更も作らず、冪等性は呼び出し側が渡す id と series に任せ、投入先の workflow は制限しない。
resource: https://github.com/tomoya-k31/totsuka/issues/940
tags: [decision, control, uds, cli, routine, task-source, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-22T01:06:30+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-940
    resource: https://github.com/tomoya-k31/totsuka/issues/940
    title: "Issue #940 — totsuka task submit（設計の問答を含む）"
---

# Status

stable（[#940](https://github.com/tomoya-k31/totsuka/issues/940)）。

# Context

[routine](/decisions/adr-0117-routines.md) は 1 回の実行で 1 タスクしか作らない。「ラベルの付いた PR を定期的に探し、PR ごとに Codex でレビューする」のような fan-out をしたくても、routine の中のエージェントが PR ごとに totsuka のタスクを作る手段が無かった。`totsuka task` は list / show / cancel / retry / export / verify だけで、外からタスクを入れる口が無い。

PR ごとのタスクを作れる既存の仕組みは task-source-github のラベル trigger だけで、これは ProjectsV2 ボードに載っている PR しか拾わない（[task-source-github](/components/task-source-github.md)）。ボードに紐づかないリポジトリでは使えない。

# Decision

1. **汎用の投入口にする。** PR レビュー専用の API や routine の子タスク専用の API にはしない。入れるのは「タスク 1 件」で、PR の列挙やレビュー手順は呼び出し側（routine のプロンプトとスキル `skills/totsuka-submit/`）が持つ。
2. **入口は CLI `totsuka task submit`、中身は制御 UDS の `POST /task/submit`**（[agent-events](/apis/agent-events.md)。`/routine/run` と同じソケット・Bearer トークン・request-response の往復）。body は `task/submit` の params そのもの（`{"workflow", "task"}`）。CLI の引数は必須が `--workflow` / `--id` / `--title` / `--body`（または `--body-file <path|->`）/ `--repo`、任意が `--branch`（`branch_hint`）/ `--url` / `--series`。
3. **source は workflow から決まる。** 本体は指名された workflow の source（`projects` から解決）で `task.source` を上書きし、既存の `on_task_submit` → `ingest_task` に流す。行・`on_*` の書き戻し・`(source, id)` の重複検査はすべてそのソースプラグインのものになる。予約 source は作らない（ADR-0117 決定 2 の「ソースはすべてプラグイン」を守る）し、プロトコルも変えない。
4. **リポジトリは名前で指定し、知らない名前は submit の時点で拒否する。** `task/submit` では `repo_hint` は手がかりにすぎず、解決できなければリポジトリ選択（LLM）に回る。呼び出し側が明示した名前が黙って別のリポジトリに化けないよう、ここだけ検査を足した。
5. **冪等性は呼び出し側が渡す id と series に任せる。** `--id` は必須で、同じ id は `duplicate`（行 id 付き）。PR なら `pr:<repo>#<番号>@<head SHA>` と組めば、同じ head は二重に走らず、push されれば新しいタスクになる。`--series <repo>#<番号>` は protocol 0.7.8 の `busy` に乗り、同じ PR のレビューが終わるまで新しい head を積まない。
6. **結果は 4 値で返す**（`accepted` / `duplicate` / `busy` / `rejected`、`--json` 対応）。fan-out では同じタスクを毎回投げるので duplicate と busy が普通に起きる。そのため非ゼロ終了にするのは rejected と通信エラーだけにした。HTTP ステータスは認証・形式・サイズ・Engine 不在に限り、断りはすべて `200` の status で返す。
7. **投入先の workflow は制限しない。** opt-in のフラグや routine ソース限定も検討したが、採らなかった（利用者の判断）。リスクは Consequences に書く。

# Consequences

- **外部 id を持つソースの workflow に入れると壊れうる。** github・notion・slack などの workflow に投入すると、id はそのソースの native id ではないので、`on_*` の書き戻し・claim・返信先の解決が失敗するか、意図しない対象に当たりうる。書き戻しを何もしない routine のソースに属する workflow に入れるのが想定の使い方。
- **エージェント経由の投入を防がない。** pane には制御 UDS のパスとトークン（`TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN`）が入っているので、信頼できないテキスト（PR 本文など）を読んだエージェントが、任意の workflow にタスクを投入できる。cancel / retry / routine run も前から同じ口で叩けたが、新しいタスクを作れるのはこれが初めてである。
- 投入されたタスクは、どのソースのタスクとも同じく `totsuka task show` とメニューバーで追える。
- `--branch` を付けると、mode が implement の workflow（`unattended` など）では worktree が PR のブランチ上にでき、push できる状態になる。「push しない」はプロンプトで守らせるしかない。
- Codex でレビューを投稿させるには通信が要る。私たちの理解では workspace-write の sandbox は既定でネットワークを塞ぐので、レビュー用の `[tools.<name>] kind = "codex"` の `mode_args` にだけ `-c sandbox_workspace_write.network_access=true` を足す運用を想定している（**未実測**。実機で確かめたら `verified` を足すこと）。
