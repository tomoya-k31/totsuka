* **Decision**: [ADR-0106 ホスト別の config ファイル](/decisions/adr-0106-per-host-config-file.md) — `--config` が無いとき `hosts/<host>.toml` → `config.toml` の順に選ぶ（#832）
* **Update**: [設定リファレンス](/development/config-reference.md) — 「config ファイルの選択」の節を追加
* **Update**: [セットアップ手順](/operations/setup-playbook.md) — マシンごとに `hosts/<host>.toml` へ分ける手順を追加
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) — `Cx::resolve` の選択順と `doctor` の `config-file` 行
