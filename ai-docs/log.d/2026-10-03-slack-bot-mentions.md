* **Creation**: [ADR-0109 Slack のメンション経路で bot と人間を区別しない](/decisions/adr-0109-slack-bot-mentions.md) — bot 投稿のメンション（個人・グループ）もタスクにする。自アプリの bot だけは除外
* **Update**: [task-source-slack](/components/task-source-slack.md) — メンション判定表①を「`bot_message` 以外の subtype」と「自アプリの bot」に絞った
* **Update**: [ADR-0079](/decisions/adr-0079-reaction-on-bot-posts.md) — 不採用だった「メンション経路を緩める」案が ADR-0109 で覆ったことを注記
* **Update**: [設定リファレンス](/development/config-reference.md) / [Slack クイックスタート](/operations/slack-quickstart.md) / [Event Gateway セットアップ](/operations/event-gateway-setup.md) / [実機検証](/components/live-e2e.md) — bot 投稿の扱いを更新
