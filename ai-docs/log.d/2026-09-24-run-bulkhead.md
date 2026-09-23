* **Update**: [ADR-0098](/decisions/adr-0098-task-state-optimistic-concurrency.md) — Engine の隔壁（`isolate_task`）を入れた（#763）。外部で状態が動いたタスクへの書き込みは warn を残してそのタスクだけを諦め、run は止まらない。版数一致の不正遷移は debug では止め、release では隔離する
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `run` の隔壁と、それを当てている境界を追記
