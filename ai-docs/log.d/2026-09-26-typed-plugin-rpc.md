* **Creation**: [ADR-0101](/decisions/adr-0101-typed-plugin-rpc.md) — プラグイン呼び出しのメソッド名と params/result 型の対応を `plugin_protocol::rpc` の型付き記述子に閉じ、`Plugin::request::<M>` で呼ぶようにした（#757）。生の `call` は `pub(crate)`、`call_no_params` は削除。kind ごとのクライアント trait と Engine のフェイク化は却下・切り出し
* **Update**: [plugin-protocol](/components/plugin-protocol.md) — `rpc` モジュールを追記
* **Update**: [orchestrator-core](/components/orchestrator-core.md) — `Plugin::request` と、ports の一覧を実在する 8 つに修正
