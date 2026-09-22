* **Creation**: [ADR-0093](/decisions/adr-0093-waiting-holds-slot.md) — 人間待ち（`waiting_input` / `escalated`）のタスクもスロットを保持するようにした。入力待ちで枠が空くため `max_concurrency` が上限として働かず、キューのタスクが次々に中途半端に進んでいた。設定での切り替えは設けず、既定の挙動を変えた
* **Update**: [orchestrator-spec](/product/orchestrator-spec.md) / [ja](/product/orchestrator-spec.ja.md) — F-45 を書き換え、F-108 の「スロット解放」を「保持」にした
* **Update**: [orchestrator-core](/components/orchestrator-core.md) / [設定リファレンス](/development/config-reference.md) / [プラグイン開発ガイド](/development/plugin-dev-guide.md) / [フック完了判定のトラブルシューティング](/operations/hook-troubleshooting.md) / [要対応（用語）](/glossary/attention.md) / [状態 DB](/data/state-db.md) — 「人間待ちでスロットを解放する」前提の記述を直した
* **Update**: [エージェントイベント](/apis/agent-events.md) — QuestionPending の park を「スロットは保持する」に直した
