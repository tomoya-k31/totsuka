* **Creation**: [run --events-jsonl](/apis/run-events-jsonl.md) — run が通知を stdout の JSON 行（`type: "notify"` / `"summary"`）で親プロセスへ流す経路。notifier プラグインは起動しない。
* **Update**: [orchestrator-core](/components/orchestrator-core.md) / [orchestrator-cli](/components/orchestrator-cli.md) — `run::emit_events_jsonl` / `write_event_line` と `run --events-jsonl`。
* **Update**: [Orchestrator 仕様](/product/orchestrator-spec.md) — §5.1 の CLI 表に `run --events-jsonl` を追加。
* **Update**: [ADR-0109](/decisions/adr-0109-native-menubar-app.md) — 行に `type` を付け、要約も JSON 行で出すことにした。
