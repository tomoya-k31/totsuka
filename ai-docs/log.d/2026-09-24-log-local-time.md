* **Creation**: [ADR-0097](/decisions/adr-0097-log-timestamps-local-display.md) — ログの時刻を保存は UTC、表示はローカル時刻にした。`run` などの端末出力と `totsuka logs` が OS のタイムゾーンで出る（`logs --utc` で UTC）
* **Update**: [ログ規約](/development/logging-conventions.md) / [運用ガイド](/operations/operations-guide.md) — 時刻の扱いと `logs --utc` を追記した
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) / [orchestrator-core](/components/orchestrator-core.md) — `logs --utc`、`logging::local_offset` / `rfc3339_at` を反映した
