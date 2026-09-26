* **Update**: [ADR-0100](/decisions/adr-0100-secrets-stdin.md) — `doctor` と `config validate` も `--secrets-stdin` を受け付けるようにした（#754 の層 2）。付けないときは `secret:` を解決せず、それを使うプラグインの検査を注記付きで飛ばす
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) — `doctor` / `config validate` の `--secrets-stdin` を追記
* **Update**: [設定リファレンス](/development/config-reference.md) — `secret:` の項に `doctor` / `config validate` の扱いを追記
