* **Creation**: [ADR-0104](/decisions/adr-0104-plugin-conformance-kit.md) — プラグインの約束事を、実バイナリを黒箱で検査する適合キットに 1 か所化した（#767）。in-process 検査と plugin-sdk / test-support への同居は却下
* **Creation**: [plugin-conformance](/components/plugin-conformance.md) — 9 項目の検査と `check` の使い方。slack が最初の利用者
* **Update**: [plugin-protocol](/components/plugin-protocol.md) — kind ごとの O→P リクエスト一覧 `HOST_REQUESTS` と、メソッド定数の網羅テスト
* **Update**: [plugin-sdk](/components/plugin-sdk.md) — `serve` が戻る前に `Stdio::flush()` で書き込みを出し切るようにした。キットが `shutdown` の応答が落ちているのを見つけた
* **Update**: [ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md) — plugins の dev 許可に `plugin-conformance`、`conformance-deps` 検査を追加
* **Update**: [テスト戦略](/quality/test-strategy.md) — プラグイン適合（黒箱）の層を追加
