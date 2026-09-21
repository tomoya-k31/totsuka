* **Creation**: `[[workflows]].timeout_secs` の既定を `0`（掃引なし）にし、権限 / idle プロンプト待ちを無音に数えないことにした [ADR-0086](/decisions/adr-0086-timeout-default-off.md)
* **Update**: 既定値と除外条件を [設定リファレンス](/development/config-reference.md)・[仕様 F-103](/product/orchestrator-spec.md)・[フック信号の流れ](/architecture/hook-signal-flow.md)・[フックのトラブルシュート](/operations/hook-troubleshooting.md) に反映
