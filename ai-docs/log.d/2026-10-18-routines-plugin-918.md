* **Creation**: [task-source-routine プラグイン](/components/task-source-routine.md) — `routines/<host>/<name>.md` を cron で submit する task_source（逃した分は直近 1 回、状態を持たない、`task/trigger` で手動実行）
* **Creation**: [routine-schedule](/components/routine-schedule.md) — routine ファイルの解析と cron の照合を持つ leaf の共有クレート（プラグインと `totsuka routine list` が共有）。[依存境界ルール](/architecture/workspace-dependency-rules.md) に `routine-leaf` を追加
* **Update**: [設定リファレンス](/development/config-reference.md) — `[routine]`（`catchup_window` / `source_name`）と routine ファイルの書式、無人実行に合う workflow の形
* **Update**: [ADR-0117](/decisions/adr-0117-routines.md) — 時差は起動時に 1 回だけ取る（夏時間は次の起動から）、routine の workflow は `profile = "unattended"`、ファイルを書いた時刻より前の予定時刻は取り戻さない
* **Update**: [task-source-routine](/components/task-source-routine.md) / [設定リファレンス](/development/config-reference.md) — routine の workflow は `profile = "unattended"`。ファイルを書いた時刻より前の予定時刻は取り戻さない。日・曜日フィールドは `*` 始まりを「制限なし」とみなす（Vixie cron と同じ）
