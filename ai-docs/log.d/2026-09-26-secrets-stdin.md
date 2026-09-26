* **Creation**: [ADR-0099](/decisions/adr-0099-secrets-stdin.md) — `run` が親プロセスから解決済みの機密情報を受け取れるようにした（#754）。`secret:<name>` スキームと `run --secrets-stdin`（stdin の 1 行目の JSON、EOF は待たない）。フラグを付けたプロセスは Keychain・`op`・`bw`・`cmd:` を backend を呼ばずに拒否する。`doctor` は `secret:` を解決せず注記する
* **Update**: [設定リファレンス](/development/config-reference.md) — シークレット参照に `secret:<name>` を追加
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `platform::supplied` と、`PlatformSecretStore` が供給された値へ全参照を回すことを追記
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) — `run --secrets-stdin` を追記
