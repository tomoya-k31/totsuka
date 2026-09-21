* **Creation**: [ADR-0091](/decisions/adr-0091-trigger-exclude.md) — github / notion の trigger に `exclude` テーブルを足した。trigger と同じ語彙で、中のどれか 1 つに一致したら取り込まない。キー同士は AND・配列は OR を共通規則として明文化し、github の `label` を配列と大小無視の照合に広げた（破壊的変更）。notion の `exclude` に `filter` は書けない。`in_progress_statuses` は統合しない
* **Creation**: [Trigger（用語）](/glossary/trigger.md) — 取り込み条件と除外条件の 2 部構成として定義した
* **Update**: [設定リファレンス](/development/config-reference.md) — `[[workflows]]` 表の `trigger` の 1 セルに詰まっていた全ソースの語彙を「`trigger` の語彙」節へ移し、共通規則・ソースごとのキー表・`exclude`・notion の `filter` の説明（`exclude` との違い）を足した
* **Update**: [plugin-sdk](/components/plugin-sdk.md) / [task-source-github](/components/task-source-github.md) / [task-source-notion](/components/task-source-notion.md) — `unknown_exclude_keys` / `one_or_many` と各ソースの `EXCLUDE_KEYS`
