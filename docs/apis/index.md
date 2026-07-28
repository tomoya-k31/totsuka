# apis

APIエンドポイント・イベント・Webhookの意味と利用文脈。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->

* [POST /agent-events（UDS フック受信）](agent-events.md) - エージェント CLI（Claude Code / Codex / OpenCode）のフック/プラグインが完了/通知/セッションイベントを orchestrator-core へ通知する UDS 上の HTTP エンドポイント。Bearer 認証・即 200・AgentSignal 正規化。制御エンドポイント POST /focus（click-to-focus、F-94）も同一ソケットに同居。
* [POST /claude-events（旧名・deprecated）](claude-events.md) - agent-events への改名（#196）前の旧 concept。実装解説は後継 agent-events.md を参照（旧パスへの POST は引き続き受理される）。
* [task/lookup（プラグイン → Orchestrator）](task-lookup.md) - 会話が既に Orchestrator に存在するかを submit 前に問い合わせる読み取り専用 JSON-RPC（protocol 0.2.4、P→O）。既知なら task_source は新規会話でしか必要のないリポジトリ解決（LLM 分類・人間への選択 UI）を省ける。到達不能時は「未知」とみなして従来の解決へ縮退する契約。
