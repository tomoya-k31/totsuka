---
type: Data Model
title: 状態DB（SQLite state.db）スキーマ
description: タスク実行状態を永続化する SQLite DB（$XDG_STATE_HOME/totsuka/state.db）の tasks/sessions/events/schema_migrations スキーマと設計判断。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/adapters/state_db.rs
tags: [sqlite, state, schema, statemachine]
timestamp: 2026-07-12T02:30:00Z
status: active
owner: tomoya-k31
---

# 概要

タスク実行状態を SQLite（`$XDG_STATE_HOME/totsuka/state.db`、WAL・`foreign_keys=ON`）へ永続化し、アプリ再起動後に実行中タスクを復元する（F-70）。埋め込みマイグレーションを起動時に自動適用し、適用前に DB ファイルをバックアップ（`{path}.bak`）する（§10.3）。

# Schema

## tasks（F-70/F-73）

`UNIQUE(source, source_task_id)` で二重取り込みを防止（F-73、`upsert_task` が冪等）。`state` は TEXT（デバッグ容易性優先、`idx_tasks_state` で `status` 高速化）。ラベル/assignee 等の残余フィールドは `source_payload` に JSON で保持（個別カラム化しない）。`finished_at` は終端状態到達時刻で worktree 掃除 retention の起点（#53）。

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | rowid |
| source | TEXT | プラグイン名（"github" 等） |
| source_task_id | TEXT | Issue番号 / NotionページID |
| workflow | TEXT | マッチしたワークフロー名 |
| mode | TEXT | plan / implement |
| repo | TEXT NULL | 選択済みリポジトリ名（pending 中 NULL） |
| worktree_path / branch | TEXT NULL | #53 が設定 |
| state | TEXT | ステートマシンの状態名 |
| priority | INTEGER | 既定 0 |
| title / url | TEXT | 表示用 |
| source_payload | TEXT NULL | JSON 残余フィールド |
| finished_at | TEXT NULL | 終端到達時刻（retention 起点） |
| created_at / updated_at | TEXT | ISO 8601 (UTC) |

## sessions（F-37、#57）

`task/dispatch` が返す `session_id` をタスク・所有プラグインに紐付けて永続化する（再起動後の `session/attach` 再接続に使う）。1 タスクに複数行を許し（リトライは新セッションを追記）、`(task_id, created_at DESC)` インデックスの最新行が re-attach 対象。ストア API は `StateDb::record_session`（追記）/ `latest_session`（最新1件）/ `list_sessions`（履歴・新しい順）。

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | rowid |
| task_id | INTEGER FK→tasks(id) | 所有タスク |
| plugin | TEXT | 所有プラグイン名（"herdr" 等） |
| session_id | TEXT | エージェントの会話/セッションID |
| created_at | TEXT | ISO 8601 (UTC)。最新行が attach 対象 |

## events（F-72）

全状態遷移を記録する監査ログ。`from_state`（取り込み時 NULL）→ `to_state`、`occurred_at`、`detail`（JSON）。実行ログ断片（F-38）は含めず JSONL ログ側（#49）に置く。

## schema_migrations（§10.3）

`version` / `applied_at`。`MIGRATIONS` 配列（index+1 = version）を順に適用。

# ステートマシン（F-71）

`domain::state` の純関数 `transition(from, event) -> Result<to>`。状態: `Queued / Pending / Dispatched / Running / WaitingInput / Publishing / Done / Failed / Cancelled`。主要遷移: `queued→dispatched→running→publishing→done`、`running⇄waiting_input`、`queued⇄pending`（F-14）、非終端→`failed`/`cancelled`、`failed`/`cancelled`→`queued`（retry, F-44）。`running`/`publishing` の実体はワークフロー（#54）が決め、ステートマシンはモード非依存。

# 再起動回復（F-37 / §5.3、#57）

`recovery::recover(db, attacher)` が起動時に `dispatched`/`running`/`waiting_input`/`publishing` のタスクを列挙し、各タスクの最新セッションへ `ports::AgentSession`（具象 `adapters::PluginAgentSession`）で `session/attach` を試みる。

- attach 成功: エージェント状態（F-32）に合わせてステートマシンを前方同期し（`apply_event`、`detail` に `kind:"recovery"` を記録）、`state/subscribe` を再確立して再開。
- セッション消失 / attach エラー / セッション未記録 / エージェント failed: タスクを「継続確認待ち」として `RecoveryReport::needs_confirmation` に載せる。**自動では failed にしない**（§5.3）。次アクション（`task retry` / `task cancel`）を人間へ提示。

リトライ（F-44）は `recovery::retry_plan(task, latest_session)` が判定: worktree＋セッションが残れば既存を再利用して会話再開、無ければ新規 worktree＋dispatch（履歴にセッション追記）。スロット再取得は `recovery::active_slot_claims(db, report)` が **再開した**タスクのうち slot 計上状態（`waiting_input` を除く）の `(repo, plugin)` を集めて `SlotManager::rebuild`（#55）へ渡す（継続確認待ちのタスクはスロットを占有しない）。

# 多重起動防止（F-74）

`run.lock` に PID を記録。取得失敗時は記録 PID の生存を [`ProcessProbe`](/components/orchestrator-core.md) で確認し、死んでいれば stale lock を自動回収する（`adapters::run_lock`）。

# 関連

- [orchestrator-core](/components/orchestrator-core.md)
- [Spec §4.8 状態管理 / §10.3](/product/orchestrator-spec.ja.md)
