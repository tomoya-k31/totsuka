* **Creation**: [task-source-routine プラグイン](/components/task-source-routine.md) — `routines/<host>/<name>.md` を cron で submit する task_source（逃した分は直近 1 回、状態を持たない、`task/trigger` で手動実行）
* **Update**: [設定リファレンス](/development/config-reference.md) — `[routine]`（`catchup_window` / `source_name`）と routine ファイルの書式、無人実行に合う workflow の形
* **Update**: [ADR-0117](/decisions/adr-0117-routines.md) — 時差は起動時に 1 回だけ取る（夏時間は次の起動から）、workflow は profile を書かず `mode` と `output = "none"`
