* **Decision**: [ADR-0105](/decisions/adr-0105-opencode-v2-plugin.md) — opencode v2 で完了検知が止まっていた問題への対応。`totsuka-opencode.js` を v2 のプラグイン API（既定エクスポート + `ctx.event.subscribe()`）に書き直し、opencode を必ず `--standalone` で起動する
* **Update**: [OpenCode ツールのセットアップと運用](/operations/opencode-tool-setup.md) — 前提を v2 に改め、検証済みの範囲と「タスクが終わらない」の切り分け手順を追記
* **Update**: [POST /agent-events](/apis/agent-events.md) — opencode の `QuestionPending` の送出元をフォームイベントに更新
* **Update**: [設定リファレンス](/development/config-reference.md) — opencode の argv に `--standalone` が必ず付くことと、質問経路の説明を更新
