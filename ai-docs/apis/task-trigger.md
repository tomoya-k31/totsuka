---
type: API Endpoint
title: task/trigger（Orchestrator → プラグイン）
description: 「いまこの key のタスクを 1 つ作れ」をソースプラグインに頼む JSON-RPC（protocol 0.7.8、O→P、capability task_trigger）。プラグインは submit せず応答として TaskSubmitParams を返し、Orchestrator がそれを task/submit と同じ経路で取り込む。totsuka routine run の実体。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/methods.rs
tags: [api, json-rpc, plugin-protocol, task-source, routine, trigger]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T21:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 概要

`task/trigger` は **Orchestrator → プラグイン（O→P）** 方向の RPC（protocol 0.7.8、[ADR-0117](/decisions/adr-0117-routines.md)）。トランスポートは他の全メソッドと同じ stdio 上の NDJSON JSON-RPC 2.0。`totsuka routine run <name>` → 制御 UDS の [`POST /routine/run`](/apis/agent-events.md) → Engine がこれを送る。

**`Capabilities.task_trigger` を宣言したプラグインにだけ送られる**（`HOST_REQUESTS`）。宣言したソースが無ければ、Engine は送らずに断る。

# Schema

## Request（`TaskTriggerParams`）

```json
{ "jsonrpc": "2.0", "id": 9, "method": "task/trigger", "params": { "key": "nightly" } }
```

| フィールド | 型 | 意味 |
|---|---|---|
| `key` | string | 何を走らせるか、ソース自身の言葉で（routine 名） |

## Response（`TaskSubmitParams`）

```json
{ "jsonrpc": "2.0", "id": 9,
  "result": { "task": { "id": "nightly@manual-20261018T093012Z", "source": "routine",
                        "title": "nightly", "body": "…", "repo_hint": "web-app",
                        "series": "nightly" },
              "workflow": "routine" } }
```

`task/submit` の params と同じ形。Orchestrator はこれを**そのソースからの `task/submit` と同じ経路**（workflow の実在と所有の検査、`series` による `busy`、重複検査）で取り込み、結果を `POST /routine/run` の呼び出し元へ返す。

知らない `key`・壊れた定義など、タスクを作れないときは **JSON-RPC エラー**で返す。メッセージはそのまま利用者に見せるので、次の一手まで書く（task-source-routine は `INVALID_PARAMS` で、知らない routine 名とその routine を探したディレクトリを示す）。

# 呼び出し側の契約

- **プラグインは応答の中で `task/submit` を呼び返さない。** Engine は `task/trigger` の応答を自分のイベントループの中で待つので、その間に同じプラグインから届く submit は処理されない。タスクは応答として返すこと（だから応答型が `TaskSubmitParams` になっている）。
- 返すタスクの `id` は、定期の実行と重ならないものにする。重なると `duplicate` になり、「いまはもう始めた」と断られる（task-source-routine は `@manual-<UTC 秒>`）。
- `series` を付ければ、実行中の同じ series に対しては `busy` で断られ、その理由（実行中のタスク番号）が呼び出し元に返る。

# 実装

- SDK: `TaskSourceHandler::task_trigger`（既定 `METHOD_NOT_FOUND`。`task_trigger` を宣言するなら上書きし、上書きするなら宣言する）
- core: `Engine::trigger_task`（[orchestrator-core](/components/orchestrator-core.md)）
- 実装しているプラグイン: task-source-routine
