* **Update**: `trigger.assignee = "@any"` が **people プロパティのマップを要求しなくなった**（#582 の一部）。[plugin-sdk](/components/plugin-sdk.md) の `assignee` の行と [config-reference](/development/config-reference.md) の `trigger` の記述を更新した。

* **Note**: **`@any` は assignee 一覧を読まない。** `AssigneeFilter::matches` は `terms` が `None`（= `@any`）なら、リストを見る前に `true` を返す。にもかかわらず起動時検査は `is_explicit()` で `@any` も通し、その先で `people_property == Some(false)` を無条件にエラーにしていた。既存テストはこの経路を `@none` で検証しており、**`@any` をこう扱うことは意図的に決められていなかった**（テストが無かった）。

* **Note**: **これが #582 の穴と繋がっていた。** `property_map.assignee` が未マップのとき、`@any` も含めて**あらゆる明示的な値がエラー**になるので、「assignee で絞り込まない」と述べる手段が存在せず、**キーを省略するのが唯一の静かな道**になっていた。ところが省略は既定 `["@me", "@none"]` を選ぶことであり、未マップでは `@none` が全ページで真になって**データベース全体が取り込まれる** —— つまりエスケープハッチが塞がれていたことが、穴が残る理由の一部だった。

* **Note**: 判定は `reads_assignees()`（`terms.is_some()`）という 1 つの述語に寄せた。「明示的に書いたか」ではなく「**assignee を実際に読むか**」が、people プロパティを要るかどうかの正しい基準だからである。エラー文にも `@any` を逃げ道として明示した。

* **Note**: **穴そのものの方針は未決のまま #582 に残した。** 未マップ＋既定トリガーをエラーにするか警告にするかは破壊的変更の判断を含む。github は `people_property` に `None` を渡すので、この修正の影響は notion に閉じている。
