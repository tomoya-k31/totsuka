* **Creation**: [ADR-0050 design / implement の確認依頼は質問ツールの選択 UI で行い、QuestionPending が park を代替する](/decisions/adr-0050-question-tool-asking.md) — 完了確認・質問を claude `AskUserQuestion` / opencode `question` の選択 UI へ移し（codex は番号付きリスト）、ダイアログ待機中に届かない `Stop{NEEDS_INPUT}` の代わりに新イベント `QuestionPending`（PreToolUse / `tool.execute.before` 発）が WaitingInput への park を担う（#487）
* **Update**: [POST /agent-events（UDS フック受信）](/apis/agent-events.md) — `hook_event_name` に `QuestionPending` を追加（質問ごとに distinct な `prompt_id` が必須である制約を含む）
* **Update**: [orchestrator-core クレート](/components/orchestrator-core.md) — フックスクリプトが 7 本になり（`on-ask-user-question.sh` 追加）、シグナル語彙に `QuestionPending` が入った
* **Update**: [設定リファレンス](/development/config-reference.md) — design / implement の承認フローに質問ツール経由の訊き方とフォールバックを追記
