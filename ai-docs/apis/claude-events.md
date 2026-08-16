---
type: API Endpoint
title: POST /claude-events（旧名・deprecated）
description: agent-events への改名（#196）前の旧 concept。実装解説は後継 agent-events.md を参照（旧パスへの POST は引き続き受理される）。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/adapters/hook_uds.rs
tags: [api, uds, hook, claude-code, signal, ingress]
generated: { by: human:tomoya-k31, at: 2026-07-23T00:00:00Z }
status: deprecated
owner: tomoya-k31
---

# 概要

本エンドポイントはツール抽象化（claude 固定の解消、#196）に伴い **`POST /agent-events` へ改名**された。現行の契約・トランスポート・認証・ペイロードの正本は後継 concept を参照:

→ **[POST /agent-events（UDS フック受信）](agent-events.md)**

改名の内訳（すべて ≤0.2.2 → 改名後）:

| 旧 | 新 |
|---|---|
| `POST /claude-events` | `POST /agent-events`（旧パスも引き続き受理: 受信側は `/focus` 完全一致以外の全パスをシグナル受信として扱う E-08） |
| ソケット `claude-events.sock` | `agent-events.sock`（旧 stale ソケットは `totsuka run` 起動時に掃除） |
| `SignalSource::ClaudeHook` | `SignalSource::AgentHook`（内部 enum） |
| state.db `claude_session_id` 列 | `tool_session_id`（マイグレーション v4、[state.db スキーマ](/data/state-db.md)） |

ワイヤ上の JSON ペイロード形（`job_id` / `session_id` / `hook_event_name` / ...）は**無変更**。
