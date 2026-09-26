---
type: Library
title: plugin-conformance
description: プラグインのバイナリを起動して stdio の NDJSON で話し、全プラグイン共通のプロトコルの約束事（initialize 前の拒否・PARSE_ERROR・METHOD_NOT_FOUND・空行と通知への無応答・INVALID_PARAMS・config/validate の未知キー・shutdown と EOF での終了・task_source の未知トリガーキー）への違反を全部返す黒箱の適合キット。公式プラグインの tests/conformance.rs と、外部のプラグイン開発者が使う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-conformance
tags: [rust, crate, plugin, protocol, testing, conformance]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T17:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

「このプラグインのバイナリは、kind に応じたプロトコルの約束事を守っているか」を、外（stdio）から確かめる（#767、[ADR-0104](/decisions/adr-0104-plugin-conformance-kit.md)）。プラグインの内部型には触れないので、`main` から SDK ランタイム、ハンドラまでの配線をまとめて検査でき、`plugin-sdk` を使わないプラグインにも同じ形で使える。

`initialize` を成功させる経路は検査しない。成功した後は実 API に触れるからで、その経路は各プラグインの in-process テストが持つ。

# 公開インターフェース

| 項目 | 内容 |
|---|---|
| `check(binary, manifest, &InitializeParams) -> Vec<String>` | 全項目を実行し、違反を 1 行ずつ返す（空なら適合）。kind と capability は `manifest`（`plugin.toml`）から読む。`InitializeParams` は `initialize` が通る最小の形で、task_source ならワークフローを 1 つ含める。キットはこれを直接は送らず、壊した複製だけを送る |

応答と終了の待ちは、どちらも固定の 5 秒。応答が来ない、または別の id への応答が来たら会話が壊れたとみなし、そのプロセスでの残りの検査を打ち切ったことを違反として返す。

# 検査項目

全 kind:

1. initialize 前に、ホストがこのプラグインへ送るはずの kind 固有リクエスト（[`HOST_REQUESTS`](/components/plugin-protocol.md) をマニフェストの capability で絞ったもの）を、正しい形の params と `{}` の両方で投げると `INVALID_REQUEST`。notifier は、initialize 前の `notify` に応答しない
2. JSON でない行には `PARSE_ERROR`（id は `null`）
3. 未知のメソッドには `METHOD_NOT_FOUND`
4. 空行と通知には応答しない
5. 形の崩れた `initialize` の params には `INVALID_PARAMS`
6. 渡した config に未知のトップレベルキーを足して `config/validate` すると `valid: false` で、エラーがそのキー名を含む
7. `shutdown` に result で応答し、終了コード 0 で終わる
8. stdin が EOF になると終わる

task_source のみ:

9. 最初のワークフローの trigger に未知のキーを足した `initialize` は `CONFIG_INVALID` で失敗し、メッセージがそのキー名を含む

エラーは**コードだけ**を見る（メッセージの文言はプロトコルではない）。例外が 6・9 のキー名で、運用者が設定を直すための唯一の手がかりなので要求する。

検査 1 が正しい形の params も送るのは、`plugin-sdk` の dispatch は params が崩れているときにしか `initialized()` を見ないからである。形の正しいリクエストはハンドラまで届くので、ハンドラ自身の「初期化済みか」の門まで試すことになる。正しい形の見本（`sample_params`）はキットが持ち、各見本が型付きの params としてパースできることをユニットテストが保証する。

# 使い方

```rust
let violations = plugin_conformance::check(
    env!("CARGO_BIN_EXE_slack"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"),
    &init,
);
assert!(violations.is_empty(), "\n{}", violations.join("\n"));
```

# 依存

`plugin-protocol` と `serde_json` のみ。検査対象の中核である `plugin-sdk` には依存しない。これは arch-lint の `conformance-deps` が検査する（[依存境界ルール](/architecture/workspace-dependency-rules.md)）。プラグインが dev-dependency に持つことは `plugin-dev` が許可している。

# 関連

- [ADR-0104](/decisions/adr-0104-plugin-conformance-kit.md)
- [テスト戦略](/quality/test-strategy.md)
- [plugin-sdk](/components/plugin-sdk.md)（`tests/sdk.rs` が SDK のサーバー自体を検査する）
