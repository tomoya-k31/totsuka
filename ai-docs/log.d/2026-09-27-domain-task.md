* **Update**: [orchestrator-core](/components/orchestrator-core.md) — タスクを `domain::Task` にし、`TaskRecord` を削除した。時刻は `OffsetDateTime` で持ち、文字列化は `ports::clock` の 1 組に集めた（#765 の 3 層目）
* **Update**: [state.db スキーマ](/data/state-db.md) — `tasks` の時刻列の読み書きの経路と、読めない時刻を読み出しエラーにする方針を追記
