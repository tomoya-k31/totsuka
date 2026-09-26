* **Creation**: [ADR-0103](/decisions/adr-0103-engine-state-types.md) — Engine の状態は不変条件を持つものだけを型へ取り出し、その型はプラグイン・DB・git を呼ばない（#758）
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `SlotManager` がスロットの持ち主（タスク ID）も持ち、`Engine.slot_holders` を吸収した。`active_slot_claims` はタスク ID つきに
