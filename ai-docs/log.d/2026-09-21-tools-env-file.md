* **Creation**: [ADR-0090](/decisions/adr-0090-tools-env-file.md) — `[tools.<name>].env_file` を足した（#744）。`KEY=value` の最小 dotenv サブセットで、それ以外は行番号付きのエラーにする。値は既存の `SecretResolver` で `op://` / `keychain:` / `cmd:` / `bw:` と `${VAR}` を解決する。解決は `totsuka run` の起動時に 1 回だけで、hook の有無にかかわらず `ToolLaunchSpec.env` に入れる。`TOTSUKA_` で始まる名前は拒否し、`doctor` は何も解決しない
* **Update**: [設定リファレンス](/development/config-reference.md) の `[tools.{name}]` に `env_file` の行と「環境変数を渡す」節を足した（`op run` との違い、再起動が要ること、残る制約）
* **Update**: [orchestrator-core](/components/orchestrator-core.md) の `config` に `env_file` モジュールを足した
