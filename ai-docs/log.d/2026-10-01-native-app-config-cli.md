* **Creation**: [totsuka config schema / get / set / unset](/apis/config-cli.md) — メニューバーアプリの設定画面が config.toml を JSON で読み書きする CLI 契約。
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `config::json_schema`（schemars による config の JSON Schema と 2 言語のヘルプ）、`config::set_path` / `unset_path`、`plugins::plugin_schemas`（initialize 前の `config/schema`）。
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) — `config schema` / `get` / `set` / `unset`。
* **Update**: [ADR-0109](/decisions/adr-0109-native-menubar-app.md) — 5 層に分けたこと、`x-raw` / `x-schema-error`、書き込みの拒否条件、プラグイン所有の project / workflow キーが未対応であること。
