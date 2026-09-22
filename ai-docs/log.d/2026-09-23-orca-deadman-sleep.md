* **Update**: [agent-ide-orca](/components/agent-ide-orca.md) — deadman の連続エラーを「起きている間に連続した」ものだけ数えるようにした。スリープ中の dark wake ごとに Orca が `runtime_timeout` を返し、それが積み上がって生きているエージェントが `failed` にされていた
* **Update**: [運用ガイド](/operations/operations-guide.md) — 「スリープ明けに複数タスクがまとめて failed」の切り分けを FAQ に足した
