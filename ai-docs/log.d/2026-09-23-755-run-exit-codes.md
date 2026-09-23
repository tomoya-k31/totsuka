* **Creation**: [ADR-0095](/decisions/adr-0095-run-startup-exit-codes.md) — `run` の起動時エラーに専用の exit code を割り当てた。人が直すまで再起動しても直らない失敗（config・機密参照・`env_file`・プラグインの導入不備と `CONFIG_INVALID`）は 4、lock の競合は 5、それ以外は従来どおり 1。ネイティブアプリの監視が設定ミスで再起動ループしないようにするため
* **Update**: [ADR-0012](/decisions/adr-0012-cli-exit-codes-json-errors.md) / [orchestrator-cli](/components/orchestrator-cli.md) — exit code の表に 4 / 5 を足した
* **Update**: [orchestrator-spec](/product/orchestrator-spec.md) / [ja](/product/orchestrator-spec.ja.md) — `run` の終了コードの記述に 4 / 5 を足した
* **Update**: [設定リファレンス](/development/config-reference.md) — スキーマ版の不一致で `run` が止まるときの終了コードを 1 から 4 に直した
