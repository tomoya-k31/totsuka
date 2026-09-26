* **Creation**: [ADR-0102](/decisions/adr-0102-core-internal-layering.md) — orchestrator-core の domain を config から切り離した（#762）。値型（`Profile` / `WorkflowMode` / `OutputPolicy` / `VerificationMode` / `CleanupPolicy`）は domain に、`config.toml` からの変換は `config::interpret`（`RootConfig::domain_workflows`）に置き、domain / ports が config と adapters を参照しないことを `arch-lint` の `core-layer` で検査する
* **Update**: [ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md) — `core-layer` を追記
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — domain と `config::interpret` の行
