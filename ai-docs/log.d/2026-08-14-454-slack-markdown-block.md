* **Creation**: [ADR-0046 Slack 返信は Block Kit markdown ブロックで投稿する](/decisions/adr-0046-slack-markdown-block.md) — エージェントの GFM が mrkdwn として崩れる問題（#454）を、標準 Markdown を受け付ける `markdown` ブロックでの投稿で解決。user token での可否と `<@USERID>` メンションの変換は実機で検証済み。12,000 字超は text-only へ縮退
* **Update**: [task-source-slack プラグイン](/components/task-source-slack.md) — `approval` の返信投稿とプレビューを `markdown` ブロック化（#454）
