---
type: Decision
title: ADR-0096 プラグインのログを JSON Lines で中継し、元のレベルのまま本体で出し直す
description: プラグインの stderr は整形済みテキストのまま一律 INFO で中継されていて、プラグインの WARN・ERROR が本体のレベル指定で消え、フィールドも 1 本の文字列に潰れていた。SDK が stderr に全レベルの JSON Lines を書き、本体がそれを分解して元のレベル・target・フィールドで出し直す構成にし、フィルタを本体の [log] level の 1 か所に集約する決定。使われていなかった [plugins.<name>] log_level は削除する。
tags: [decision, logging, plugin, tracing, adr]
generated: { by: claude-code/opus-5, at: 2026-09-24T10:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: go-plugin
    resource: https://github.com/hashicorp/go-plugin/blob/main/client.go
    title: hashicorp/go-plugin — client.go（logStderr）
  - id: mcp-stdio
    resource: https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio
    title: MCP 仕様 2026-07-28 — stdio transport
  - id: mcp-logging
    resource: https://modelcontextprotocol.io/specification/2026-07-28/server/utilities/logging
    title: MCP 仕様 2026-07-28 — logging（SEP-2577 で非推奨）
  - id: tf-log
    resource: https://developer.hashicorp.com/terraform/plugin/log/managing
    title: Terraform — Managing Log Output
  - id: tracing-2730
    resource: https://github.com/tokio-rs/tracing/issues/2730
    title: tokio-rs/tracing#2730 — Non-const event level
---

# Status

stable。[プラグイン開発ガイド](/development/plugin-dev-guide.md)の「ログと stderr」（#497）の中継方式を置き換える。

# Context

プラグインは stdout を JSON-RPC に使うので、ログは stderr に書く。本体はこの stderr を 1 行ずつ読み、`tracing::info!(plugin = %name, "{line}")` で自分のログに流していた。読み続けるのはパイプのバッファを詰まらせてプラグインを止めないためで、`plugin=<name>` の付与と流量制限（10 秒 100 行）のために本体のログを通していた。

この中継には次の問題があった。

- **レベルが常に INFO になる。** プラグインの `WARN` も `ERROR` も `INFO orchestrator_core::adapters::plugin_host: … WARN agent_ide_orca::agent: …` のように INFO の行の中に埋まり、レベルが 2 つ並ぶ。
- **フィルタが 2 か所にあり、設定が別々だった。** プラグイン側はそれぞれの `RUST_LOG`（既定 info）で、本体は `[log] level` で絞る。本体を `warn` にするとプラグインの WARN・ERROR まで消え、本体を `debug` にしてもプラグインの debug は出ない。
- **フィールドが 1 本の文字列に潰れる。** `task_id` で `logs --task` の絞り込みができず、秘密情報のフィールド名 denylist も効かない（値のパターンによるマスクだけが効いていた）。
- **`[plugins.<name>] log_level` はどこからも読まれていなかった。** schema と設定テンプレートにはあり、[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md) は「core がその値で何かを決めるキー」に数えていたが、実際には何もしていなかった。

2026 年時点の外部の実装を調べると、形は揃っていた。

- MCP は stdio では stderr をログに使ってよいとし [^mcp-stdio]、プロトコルに載せるログ通知（`notifications/message`）は 2026-07-28 版で非推奨にした。移行先は stderr への構造化ログか OpenTelemetry [^mcp-logging]。
- go-plugin は子の stderr の行が JSON ならレベルとフィールドを取り出してホストのロガーで出し直し、JSON でない行は既定のレベルに落とす。`panic:` 以降は ERROR にする。レベルの判定はホスト側でだけ行う [^go-plugin]。
- Terraform は本体とプロバイダのログレベルを別の環境変数で指定できる [^tf-log]。

# Decision

1. **経路は stderr のまま。プロトコルにログ用のメソッドは足さない。** MCP が非推奨にした方向で、stderr なら SDK を使わないプラグインや panic の出力も同じ経路で拾える。

2. **SDK（`plugin_sdk::runtime::init_tracing`）は、stderr がパイプなら JSON Lines を全レベルで書く。** `tracing-subscriber` の JSON 形式で、フィールドを `message` と同じ階層に平らに置く（`flatten_event`）。span の情報は出さない（span を開くプラグインが無い）。`RUST_LOG` は読まない。stderr が端末なら（手で動かしたとき）これまでどおり人間向けの表示で、`RUST_LOG`（既定 info）に従う。

3. **本体（`spawn_stderr_logger`）は各行を分解し、元のレベルで出し直す。**
   - JSON で `level` を持つ行: `level` / `target` / `message` を取り出し、残りをフィールドにする。`timestamp` は捨て、本体の時刻を使う（差はパイプの遅延だけ）。
   - それ以外の行（SDK を使わないプラグイン、runtime の出力）: `INFO` で、行をそのまま message にする。`thread '…' panicked at` の行とそれ以降は `ERROR`。go-plugin の既定は DEBUG だが、本体の既定が INFO なのでそれでは見えなくなる。
   - 空行は捨てる。

4. **フィルタは本体の `[log] level` の 1 か所だけにする。** プラグインは全レベルを出し、本体が判定する。本体に集約すれば、SDK を使わないプラグインにも同じ判定がかかり、設定も 1 か所で済む。代わりに依存ライブラリの TRACE もパイプを流れ、本体はそれを読んで捨てる。この負荷は受け入れる。

5. **流量制限はレベル判定の後で数える。** 前は判定の前に数えていたので、捨てるはずの DEBUG が 10 秒 100 行の枠を食い、同じ窓の WARN を押し出せた。

6. **実行時に決まるレベル・target・フィールドは、予約フィールドで Layer に渡す。** `tracing` はイベントのレベル・target・フィールド名をコンパイル時に固定する [^tracing-2730]。そこで:
   - レベルは 5 つの呼び出し地点を `match` で呼び分ける（`level_enabled` / `emit_stderr_record`）。
   - target は `plugin.target`、フィールドは JSON オブジェクト 1 つにまとめて `plugin.fields` という予約フィールドで渡す（`logging::PLUGIN_TARGET_FIELD` / `PLUGIN_FIELDS_FIELD`）。
   - `RedactingLayer` はこの 2 つを見つけたら、target を差し替え、フィールドを 1 つずつ展開する。展開したフィールドは通常のフィールドと同じ経路を通るので、denylist によるマスクとプロンプト系フィールドの抑止が効く。人間向け表示では、プラグイン由来の target も値と同じくエスケープする。
   - `plugin` フィールドは最後に記録し、プラグインが同名のフィールドを書いても上書きされないようにする。

7. **`[plugins.<name>] log_level` を削除する。** 本体で判定するので、プラグインごとのレベルを渡す先が無い。必要になったら `[log] level` を `EnvFilter` 形式（`info,agent_ide_orca=debug` のような指定）に広げる。`[plugins.<name>]` は `deny_unknown_fields` なので、このキーを書いた設定は起動時にエラーになる（破壊的変更）。

# 代替案と不採用理由

- **中継時にテキストの行からレベルを読み取る（形式は変えない）。** 変更は本体だけで済むが、フィールドは文字列のままで `logs --task` もフィールド名のマスクも効かない。
- **プロトコルにログ通知を足す（LSP の `window/logMessage`、MCP の `notifications/message`）。** MCP がちょうど非推奨にした方向。SDK を使わないプラグインや panic は拾えず、stderr の中継は結局残る。
- **プラグイン側で絞る（本体が `RUST_LOG` に同じ値を渡す）。** 捨てる行の分の負荷は減るが、判定が 2 か所になり、`RUST_LOG` を読まないプラグインは本体だけが頼りになる。判定を 1 か所にすることを優先した。
- **プラグインごとの `log_level` を残して本体の判定に使う。** 使う場面が見当たらず、これまでも誰も使えていなかった（読まれていなかった）。
- **OpenTelemetry（OTLP）で出す。** ログを読むのはローカルの `totsuka logs` と `jq` で、収集基盤が無い。JSON の形は OTel の Logs Data Model に対応が取れる（level → Severity、target → InstrumentationScope、フィールド → Attributes）ので、必要になれば `opentelemetry-appender-tracing` を足せる。

# Consequences

- ログファイルでプラグインの行が `"level":"WARN","target":"agent_ide_orca::agent","plugin":"orca"` の形になり、`jq` でレベルや `task_id` で絞れる。
- 本体を `[log] level = "debug"` にすれば、プラグインの debug も出る。
- SDK を使わないプラグインの行は、これまでどおり INFO として扱われる。自分のレベルで扱われたいなら、stderr に `level` / `target` / `message` を持つ JSON Lines を書けばよい（[プラグイン開発ガイド](/development/plugin-dev-guide.md)）。
- `tracing-subscriber` の `json` feature を `plugin-sdk` で有効にした（`tracing-serde` が推移的に入る）。

[^mcp-stdio]: MCP 仕様 2026-07-28 — stdio transport
[^mcp-logging]: MCP 仕様 2026-07-28 — logging（SEP-2577 で非推奨）
[^go-plugin]: hashicorp/go-plugin — client.go（logStderr）
[^tf-log]: Terraform — Managing Log Output
[^tracing-2730]: tokio-rs/tracing#2730 — Non-const event level
