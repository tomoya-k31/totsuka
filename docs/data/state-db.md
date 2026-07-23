---
type: Data Model
title: 状態DB（SQLite state.db）スキーマ
description: タスク実行状態を永続化する SQLite DB（$XDG_STATE_HOME/totsuka/state.db）の tasks/sessions/events/hook_events/schema_migrations スキーマと設計判断。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/adapters/state_db.rs
tags: [sqlite, state, schema, statemachine, hooks]
timestamp: 2026-07-23T00:00:00Z
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
| thread_key | TEXT NULL | 会話継続相関キー `"{channel}:{thread_ts}"`（v2/#134、E-09）。`idx_tasks_thread_key`。Slack 追いメンションの resume 元検索に使う |
| last_signal_at | TEXT NULL | 最終フックシグナル時刻（v2/#134、R-10 タイムアウト起点）。`touch_last_signal` が更新 |

## sessions（F-37、#57）

`task/dispatch` が返す `session_id` をタスク・所有プラグインに紐付けて永続化する（再起動後の `session/attach` 再接続に使う）。1 タスクに複数行を許し（リトライは新セッションを追記）、`(task_id, created_at DESC)` インデックスの最新行が re-attach 対象。ストア API は `StateDb::record_session`（追記）/ `latest_session`（最新1件）/ `list_sessions`（履歴・新しい順）。

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | rowid |
| task_id | INTEGER FK→tasks(id) | 所有タスク |
| plugin | TEXT | 所有プラグイン名（"herdr" 等） |
| session_id | TEXT | エージェントの会話/セッションID |
| created_at | TEXT | ISO 8601 (UTC)。最新行が attach 対象 |
| tool_session_id | TEXT NULL | ツール（Claude Code 等）ネイティブの `session_id`（v2/#134、E-09。`--resume` 相関）。フックの SessionStart 観測時に `set_tool_session_id` が記録。`idx_sessions_tool_session`。v4/#196 で `claude_session_id` から改名 |

追加ストア API（v2/#134）: `set_tool_session_id(session_row_id, sid)`（当該セッション行へツールネイティブのセッション ID を記録）/ `find_session_by_tool_session_id(sid)`（ツールセッション ID から最新セッション行を逆引き）。フック dispatch 配線（#138）: `reserve_session(task_id, plugin)` が `session_id` 空でセッション行を先行確保し、その行 id を `job_id = job-{task_id}-{session_row}` の `session_row` に用いる（フックが echo する job_id は起動時に env 注入するため、`task/dispatch` 応答前に行 id が必要）。`task/dispatch` 応答後 `set_session_native_id(session_row_id, session_id)` で実 session_id を埋める。dispatch 失敗時は `delete_session(session_row_id)` で予約行をロールバック（空 id 行を残さず retry/recovery が誤 reattach しない）。

## hook_events（#131/#134、D-05/N-01/E-09）

Claude Code フック（Stop / Notification / SessionStart / SessionEnd / heartbeat）を UDS 経由で受信し**冪等に永続化**する監査ログ（N-01）。冪等キー `(job_id, tool_session_id, prompt_id, event, status)` の `UNIQUE` 制約で、多重発火・スプール再送・curl 再送の重複到着（**同一 status**）を無害化する（D-05/E-05/E-06）。**UNIQUE 構成列は NULL でなく空文字既定**（`tool_session_id` / `prompt_id` / `status` は `NOT NULL DEFAULT ''`）— SQLite は UNIQUE で NULL 同士を区別するため、NULL 既定だと重複排除が効かない。**`status` を冪等キーに含める理由（v3/#131 実機検収）**: Stop フックの `block` 差し戻しはエージェントを**同一ターン内で再完了**させるため、再完了 Stop は初回の空 Stop と `(job_id, session, prompt_id, event='stop')` を共有しつつ **status が変わる（UNKNOWN → COMPLETED）**。status を鍵に含めないと再完了が「再送」として dedup で捨てられ、完了が届かずタスクが `dispatched` で滞留する。status を含めることで status 変化は通し、同一 status の再送は従来どおり弾く（受け入れ #4 の二重遷移防止を維持）。書き込み経路（Engine 統合）は #138。

| 列 | 型 | 備考 |
|---|---|---|
| id | INTEGER PK | rowid |
| job_id | TEXT | 相関キー `"job-{task_id}-{session_row}"`（E-09。session_id 単独推測は禁止） |
| task_id | INTEGER FK→tasks(id) | 所有タスク |
| tool_session_id | TEXT NOT NULL DEFAULT '' | ツールネイティブの `session_id`（無ければ ''。v4/#196 で `claude_session_id` から改名） |
| prompt_id | TEXT NOT NULL DEFAULT '' | フック入力の `prompt_id`（無ければ ''） |
| event | TEXT | `stop` / `notification` / `session_start` / `session_end` / `heartbeat` |
| status | TEXT NOT NULL DEFAULT '' | `stop` の自己申告 `COMPLETED`/`NEEDS_INPUT`/`FAILED`/`UNKNOWN`、それ以外は `''`（v3 で冪等キーに参加。#131） |
| payload | TEXT | 受信 JSON 全文（監査 N-01） |
| received_at | TEXT | ISO 8601 (UTC) |

`idx_hook_events_task (task_id, id)`。追加ストア API:

- `record_hook_event(&HookEventInsert) -> HookEventOutcome` — `INSERT ... ON CONFLICT DO NOTHING`。新規は `New`、冪等キー衝突は `Duplicate`（呼び出し側は黙って捨てる）。
- `unknown_stop_streak(task_id) -> u32` — stop イベントを id 降順に走査し、最初の非 UNKNOWN stop までの UNKNOWN 連続数（D-02 のエスカレーション計数。**フック自己申告の block_count は信用せず DB から再計算**）。`idx_hook_events_task` + 早期 break で実質 O(streak)。

## events（F-72）

全状態遷移を記録する監査ログ。`from_state`（取り込み時 NULL）→ `to_state`、`occurred_at`、`detail`（JSON）。実行ログ断片（F-38）は含めず JSONL ログ側（#49）に置く。

## schema_migrations（§10.3）

`version` / `applied_at`。`MIGRATIONS` 配列（index+1 = version）を順に適用。追記のみ（既存バージョンは不変）で、未適用があれば適用前に DB ファイルを `{path}.bak` へバックアップ。現行 v4（v1 = 初期スキーマ、v2 = #134 の `hook_events` テーブル・`tasks.thread_key`/`last_signal_at`・`sessions.claude_session_id`、v3 = #131 実機検収フォローアップで `hook_events` の `UNIQUE` キーに `status` を追加・`status` を `NOT NULL DEFAULT ''` 化。SQLite は制約を in-place 変更できないためテーブルを再構築（`RENAME`→新規 `CREATE`→`INSERT ... SELECT COALESCE(status,'')`→旧 `DROP`）。既存行は保全。v4 = #196 ツール抽象化の rename で `sessions.claude_session_id` / `hook_events.claude_session_id` を `tool_session_id` へ `RENAME COLUMN`。SQLite ≥3.25 の RENAME COLUMN はテーブル制約・インデックス内の列参照も書き換えるため `hook_events` の UNIQUE 冪等キーは再構築不要。`idx_sessions_claude_session` のみ名前のため `idx_sessions_tool_session` へ作り直し）。

[会話継続](/glossary/conversation-continuity.md)（E-09）用ストア API: `find_by_thread_key(workflow, thread_key, exclude_id) -> Option<TaskRecord>` — 同一 workflow・同一 `thread_key` の最新（id 最大）先行タスクを返す（Slack 追いメンションの resume 元特定、#140）。`exclude_id` で dispatch 中の自タスクを除外する（追いメンション自身は既に ingest 済みで最新一致になるため、除外しないと「先行」が自分自身に解決してしまう。workflow 一致とあわせ別 workflow の同名スレッド誤紐付けも防ぐ）。

# ステートマシン（F-71）

`domain::state` の純関数 `transition(from, event) -> Result<to>`。状態: `Queued / Pending / Dispatched / Running / WaitingInput / Verifying / Escalated / Publishing / Done / Failed / Cancelled`（`Verifying`=human 検収待ち・`Escalated`=人間対応待ちは #133 追加、どちらも非終端）。主要遷移: `queued→dispatched→running→publishing→done`、`running⇄waiting_input`、`queued⇄pending`（F-14）、非終端→`failed`/`cancelled`、`failed`/`cancelled`→`queued`（retry, F-44）。検収・エスカレーション遷移（#131/#133）: `running`/`waiting_input`/`escalated` →(SelfReportComplete)→ `verifying`（human 検収のみ。llm/none は既存 BeginPublish で `publishing` 直行 — `waiting_input`/`escalated` からの BeginPublish も可）、`verifying` →(ApproveVerification)→ `publishing` / →(VerificationFailed)→ `running`、全非終端 →(Escalate)→ `escalated`、`escalated` からは次シグナルで `verifying`/`publishing`/`waiting_input`/`running` へ復帰。`running`/`publishing` の実体はワークフロー（#54）が決め、ステートマシンはモード非依存。

# 再起動回復（F-37 / §5.3、#57）

`recovery::recover(db, attacher)` が起動時に `dispatched`/`running`/`waiting_input`/`verifying`/`escalated`/`publishing`（`RECOVERABLE_STATES`）のタスクを列挙し、各タスクの最新セッションへ `ports::AgentSession`（具象 `adapters::PluginAgentSession`）で `session/attach` を試みる。

- attach 成功: エージェント状態（F-32）に合わせてステートマシンを前方同期し（`apply_event`、`detail` に `kind:"recovery"` を記録）、`state/subscribe` を再確立して再開。
- セッション消失 / attach エラー / セッション未記録 / エージェント failed: タスクを「継続確認待ち」として `RecoveryReport::needs_confirmation` に載せる。**自動では failed にしない**（§5.3）。次アクション（`task retry` / `task cancel`）を人間へ提示。
- **human-gated 安全化（#133）**: `verifying`/`escalated` のタスクはエージェント状態に関わらず常に「継続確認待ち」（検収・エスカレーション解消を再起動で自動スキップしない）。また `waiting_input` 中にエージェントが Done を報告していたケースも自動 Publishing せず「継続確認待ち」とする（human 検収待ち相当のタスクが再起動を跨ぐと検収をスキップして自動 publish される穴の封鎖）。

リトライ（F-44）は `recovery::retry_plan(task, latest_session)` が判定: worktree＋セッションが残れば既存を再利用して会話再開、無ければ新規 worktree＋dispatch（履歴にセッション追記）。スロット再取得は `recovery::active_slot_claims(db, report)` が **再開した**タスクのうち slot 計上状態（`waiting_input` を除く）の `(repo, plugin)` を集めて `SlotManager::rebuild`（#55）へ渡す（継続確認待ちのタスクはスロットを占有しない）。

# 多重起動防止（F-74）

`run.lock` に PID を記録。取得失敗時は記録 PID の生存を [`ProcessProbe`](/components/orchestrator-core.md) で確認し、死んでいれば stale lock を自動回収する（`adapters::run_lock`）。

# 関連

- [orchestrator-core](/components/orchestrator-core.md)
- [Spec §4.8 状態管理 / §10.3](/product/orchestrator-spec.ja.md)
