# apis

APIエンドポイント・イベント・Webhookの意味と利用文脈。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->

* [POST /claude-events（UDS フック受信）](claude-events.md) - Claude Code フックが完了/通知/セッションイベントを orchestrator-core へ通知する UDS 上の HTTP エンドポイント。Bearer 認証・即 200・AgentSignal 正規化。
