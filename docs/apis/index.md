# apis

APIエンドポイント・イベント・Webhookの意味と利用文脈。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->

* [POST /agent-events（UDS フック受信）](agent-events.md) - エージェント CLI（現状 Claude Code）のフックが完了/通知/セッションイベントを orchestrator-core へ通知する UDS 上の HTTP エンドポイント。Bearer 認証・即 200・AgentSignal 正規化。制御エンドポイント POST /focus（click-to-focus、F-94）も同一ソケットに同居。
* [POST /claude-events（旧名・deprecated）](claude-events.md) - agent-events への改名（#196）前の旧 concept。実装解説は後継 agent-events.md を参照（旧パスへの POST は引き続き受理される）。
