* **Creation**: [ADR-0094](/decisions/adr-0094-task-control-endpoints.md) — `task cancel` / `retry` を実行中の Engine へ届ける制御ルート（`POST /task/cancel`・`/task/retry`）を hook UDS に足した（#760）。Engine が run ループの中で遷移とスロット等の解放を行う。cancel で pane は閉じない。DB 直接書き込みはフォールバックとして残す
* **Update**: [エージェントイベント](/apis/agent-events.md) / [claude-events（旧名）](/apis/claude-events.md) — 制御パスが 3 本になった。新ルートの契約を追記した
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `task_control` モジュールを追加し、`FocusPort` を `ControlPort` に改名・拡張した
