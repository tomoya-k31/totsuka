* **Decision**: [ADR-0117 routines — host ごとの定期実行](/decisions/adr-0117-routines.md) — `routines/<host>/<name>.md` を `task-source-routine` プラグインが時刻ごとに submit する。逃した分は直近 1 回、冪等性は既存の重複検査、実行中の重複は `Task.series` と `busy`、手動実行は `task/trigger`
* **Update**: [plugin-protocol](/components/plugin-protocol.md) — 0.7.8 で `Task.series`・`TaskSubmitStatus::Busy`・`task/trigger`・`Capabilities.task_trigger`・`InitializeParams.config_dir` / `host`
* **Update**: [plugin-sdk](/components/plugin-sdk.md) / [プラグイン開発ガイド](/development/plugin-dev-guide.md) — `TaskSourceHandler::task_trigger`（既定 `METHOD_NOT_FOUND`、`task_trigger` でゲート）と `SubmitOutcome::Busy`
* **Creation**: [task/trigger](/apis/task-trigger.md) — O→P の API 契約（`key` → `TaskSubmitParams`、応答の中で submit を呼び返さない、`task_trigger` で送られる）
