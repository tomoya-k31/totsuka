---
type: API Endpoint
title: task/lookup（プラグイン → Orchestrator）
description: 会話が既に Orchestrator に存在するかを submit 前に問い合わせる読み取り専用 JSON-RPC（protocol 0.2.4、P→O）。既知なら task_source は新規会話でしか必要のないリポジトリ解決（LLM 分類・人間への選択 UI）を省ける。到達不能時は「未知」とみなして従来の解決へ縮退する契約。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-sdk/src/lookup.rs
tags: [api, json-rpc, plugin-protocol, task-source, conversation, lookup]
generated: { by: human:tomoya-k31, at: 2026-07-26T00:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 概要

`task/lookup` は **プラグイン → Orchestrator（P→O）** 方向の読み取り専用 RPC（protocol 0.2.4、[ADR-0015](/decisions/adr-0015-conversation-task-identity.md)）。トランスポートは他の全メソッドと同じ stdio 上の NDJSON JSON-RPC 2.0 で、`task/submit`（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）と同じ P→O 機構に相乗りする。

# Schema

## Request（`TaskLookupParams`）

```json
{ "jsonrpc": "2.0", "id": "lookup-0", "method": "task/lookup",
  "params": { "source": "slack", "task_id": "C0AGK11DMM4:1784977373.767279" } }
```

| フィールド | 型 | 意味 |
|---|---|---|
| `source` | string | 名乗るソース名。**Orchestrator は使わない** — 下記「なぜ params の source を信用しないか」 |
| `task_id` | string | 会話の識別子（= `Task.id`）。Slack なら `{channel}:{thread_ts}` |

## Response（`TaskLookupResult`）

```json
{ "jsonrpc": "2.0", "id": "lookup-0", "result": { "known": true, "repo": "totsuka" } }
```

| フィールド | 型 | 意味 |
|---|---|---|
| `known` | bool | この会話が Orchestrator に存在するか |
| `repo` | string \| null | 束ねられているリポジトリ。**`known: true` でも null がありうる** — 選択がまだ決着していない（人間が picker を見ている / 分類が不確定）。「リポジトリが無い」ではなく「ヒントは無い」と読む |

エラーは `SUBMIT_OVERLOADED`（in-flight 枠の枯渇。`task/submit` とは**別枠**で、片方のバーストが他方を枯渇させない）と `NOT_ACCEPTING`（ドレイン中）。

# 呼び出し側の契約

```text
known: true  → repo_hint なしで即 submit（LLM も picker も呼ばない）
known: false → 従来どおり解決（rule → LLM → 必要なら picker）
無応答・タイムアウト・エラー → known: false と同じ扱い
```

**失敗はエラー条件ではない。** `task/submit` と違って「いずれ必ず通す」必要はなく、答えが得られなければ *この RPC が存在しなかった場合と同じ経路*を通るだけ。したがって [`LookupClient`](https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-sdk/src/lookup.rs) は**リトライもバックオフもしない** — 1 回投げ、10 秒でタイムアウトし、`Lookup::Unknown` を返す。リトライは同じフォールバックを待つ時間を延ばすだけで、しかもその待ちは実在する（Orchestrator は自分のイベントループで応答するため、worktree 作成中なら詰まる）。

`known: true` で `repo` が null でも**解決をスキップする**のが要点。リポジトリ解決は新規会話だけの仕事であり、既存の会話は既にリポジトリを持っているか、今まさに人間が選んでいる最中で、どちらも決着をつけるのは Orchestrator 側。ここで再解決すると、よくても LLM 呼び出しの空費、悪ければ**既に選択 UI を見ている人間の前に 2 枚目の picker を出す**ことになる。

# なぜ params の `source` を信用しないか

Orchestrator は `params.source` ではなく**接続元のプラグインインスタンス名**で引く。別ソースの会話を名乗って覗けないようにするためで、`task/submit` が `task.source` を上書きするのと同じ方針（プラグインは自分のソース名を名乗るが、正本は Orchestrator が持つ）。`params.source` は診断用のヒントとして受け取るだけで、不一致でも**拒否しない** — `task/submit` の「上書きであって拒否ではない」という確立した規約と食い違わせないため。

# 実装メモ

- **読み取り専用なので失敗しても run-fatal にしない**。応答して捨てる。プラグイン側の縮退（従来どおりの解決）が正しい挙動であり、`task/submit` の永続化失敗とは扱いが違う。
- **エンジンループで処理される**。`git fetch` や worktree 作成で詰まっていれば待たされる。タイムアウト → 縮退が必須なのはこのため。
- **既知のレース**: task_source はメンションごとに並行処理することがあるため、**新規**スレッドへの 2 通が同時到着すると両方が `known: false` を見うる。実害はない（それぞれの submit が自分の `message_key` を運び、Orchestrator は 1 会話の 2 メッセージとして取り込む）。重複するのは解決の作業だけ。

# 関連

- [ADR-0015 タスクの同一性を「1 メッセージ」から「1 会話」へ変える](/decisions/adr-0015-conversation-task-identity.md)
- [会話継続（conversation continuity）](/glossary/conversation-continuity.md)
- [plugin-protocol](/components/plugin-protocol.md) — `TaskLookupParams` / `TaskLookupResult` の型定義とバージョン方針
- [plugin-sdk](/components/plugin-sdk.md) — `LookupClient`（縮退の実装）
- [task-source-slack](/components/task-source-slack.md) — 唯一の呼び出し側
