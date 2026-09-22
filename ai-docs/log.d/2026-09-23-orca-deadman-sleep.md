* **Update**: [agent-ide-orca](/components/agent-ide-orca.md) — deadman の連続エラーを「起きている間に連続した」ものだけ数えるようにした。スリープ中の dark wake ごとに Orca が `runtime_timeout` を返し、それが積み上がって生きているエージェントが `failed` にされていた
