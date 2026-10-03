* **Creation**: [ADR-0112 Event Gateway が却下モーダルを開き、その送信を中継する](/decisions/adr-0112-gateway-reject-modal.md) — 任意のボットトークンで views.open、view_submission を新しいレコード種別として中継
* **Update**: [ADR-0072](/decisions/adr-0072-slack-event-gateway.md) — 決定 1・4 を却下モーダルの経路に限って ADR-0112 が改めたことを Status に追記
* **Update**: [ADR-0111](/decisions/adr-0111-reject-modal-alt-reply.md) — Gateway 経由の扱いを ADR-0112 へリンク
* **Update**: [slack-event-gateway](/components/slack-event-gateway.md) — `modal` モジュール、`bot_token`、`view_submission` の射影
* **Update**: [task-source-slack](/components/task-source-slack.md) — `gateway_contract` / `gateway` が `view_submission` を受ける
* **Update**: [Event Gateway 構築手順](/operations/event-gateway-setup.md) — 任意の `bot_token` と、それで増える資格情報
