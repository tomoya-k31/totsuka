* **Creation**: [ADR-0107](/decisions/adr-0107-opencode-hook-parity.md) — claude の `--settings` フックの残り（不可視注入・マーカー欠落時の再依頼・Notification・SessionEnd）を opencode v2 プラグインで実装し、`invisible_injection` / `marker_block` を true にした。タスクツールのサブエージェントのセッションは報告も注入もしない
* **Update**: [OpenCode ツールのセットアップと運用](/operations/opencode-tool-setup.md) — 縮退一覧を更新（再依頼・不可視注入・サブエージェント除外、triage/design で `gh` を使えない点を追記）
* **Update**: [設定リファレンス](/development/config-reference.md) — opencode の質問ツールの指示が不可視で届くことに修正
