* **Creation**: [ADR-0098](/decisions/adr-0098-task-state-optimistic-concurrency.md) — タスクの状態遷移を行バージョンによる楽観的並行制御にした（#763）。`apply_event` / `retry_task` は読んだ参照（`TaskRef`）でしか呼べず、外部で動いたタスクへの書き込みは `StateError::Conflict` で拒否される。Engine の隔壁は後続の PR
* **Update**: [状態DB スキーマ](/data/state-db.md) — v9 で `tasks.state_version` を追加。遷移の書き方（`TaskRef`・Conflict）を追記
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `state_db` の遷移 API が `TaskRef` を取るようになったことを反映
