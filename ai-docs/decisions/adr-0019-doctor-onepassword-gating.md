---
type: Decision
title: ADR-0019 doctor の op:// 解決は「TTY があるか」ではなく「op セッションがあるか」で出し分け、走らなかった検査は skipped として報告する
description: doctor が ADR-0006 の非対話原則を自分で破っていた問題（#289）に対し、check_onepassword を最初に動かして op whoami の結果を可否判定に使い、セッションが無いときだけ op:// を要する probe を skipped として報告する決定。TTY 判定（案 D）ではなくプロンプトが実際に出る条件を直接見る（案 E）。plugin probe を --online の裏に隠す案 B と、挙動を変えず約束だけ直す案 C は不採用。あわせて Check に skipped 重大度を追加し、検出ヘルパが toml 0.9 の Value パーサ誤用で常に false を返していた（＝1Password 検査が一度も走っていなかった）バグを修正した。
resource: https://github.com/tomoya-k31/totsuka/issues/289
tags: [doctor, onepassword, secret, non-interactive, cli, adr-0006]
generated: { by: human:tomoya-k31, at: 2026-07-26T23:00:00Z }
status: stable
owner: tomoya-k31
---

# Status

Accepted — 2026-07-26（[#289](https://github.com/tomoya-k31/totsuka/issues/289)）

[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) の「doctor は非対話を保つ」方針を**否定せず、実装が実際にそれを満たすようにする**。

# Context

`doctor` の `check_llm_key` と `check_hook_token` は `op://` 参照を**意図的に解決しない**。`op read` が生体認証プロンプトを出す（無人実行ではハングする）ためで、ADR-0006 の非対話原則として正しい。

**ところが同じ `doctor` の実行中に、別のチェックが同じ参照を実解決していた。**

`check_plugins` は `check_llm_key` より**前に無条件で**走り、enabled なプラグインごとに `plugin_spec()` を呼ぶ。その中の 2 経路がどちらも `secret_resolver(env)` をそのまま通す:

| 経路 | 解決対象 | 走る条件 |
|---|---|---|
| `plugin_init_config()` → `resolve_strings()` | `plugins/{name}.toml` の**全文字列 leaf** | enabled なプラグイン**すべて** |
| `llm_info()` | **`[llm].api_key_ref`** | `task_source` のプラグイン |

つまり doctor は「プロンプトを避けるため解決しない」と表示した直後に、まさにその参照を解決していた。**無人実行（CI・cron）でハングしうる**という、ADR-0006 が避けようとした失敗そのものである。

調査で issue に挙がっていなかった**3 つ目の経路**も見つかった: `check_hook_socket` が live receiver がいるとき `[hooks].auth_token_ref` を無条件に解決する。`check_orphan_panes` も `plugin_spec` を 2 回呼ぶ。

## 前提として先に壊れていたもの

修正の途中で、**1Password 検査が導入以来一度も走っていなかった**ことが判明した。

ゲート役の `config_mentions_onepassword` が `content.parse::<toml::Value>()` を使っていたが、**toml 0.9 の `FromStr for Value` は「単一の値」のパーサ**であり、ドキュメントを渡すと必ずエラーになる（`"a = 1"` すら `unexpected content, expected nothing`）。したがってこのヘルパは常に `false` を返し、`op://` を使っている構成でも `1password` / `1password-session` は 1 行も出ていなかった。

このバグは [#169](https://github.com/tomoya-k31/totsuka/pull/169) でヘルパが書かれた時点から存在する（toml 0.9 は #71 で既に入っていた）。**#289 の対処は「セッションの有無で出し分ける」ことなので、セッションを見るチェックが動いていなければ成立しない。** 同じ変更で直す必要があった。

# Decision

## 1. 判定は `op whoami` の結果で行う（案 E）。TTY の有無では代用しない（案 D）

`op read` がプロンプトを出すのは**セッションが確立していないときだけ**である。`check_onepassword` は既に `op whoami`（それ自体はプロンプトを出さない）を実行しているので、その結果をそのまま可否判定に使う。

`OpReadiness` の 3 状態:

| 状態 | 条件 | 意味 |
|---|---|---|
| `NotUsed` | config に `op://` が無い | 何も解決しないのでゲート不要 |
| `Ready` | `op whoami` 成功 | プロンプトは出ない。probe は従来どおり走る |
| `WouldPrompt` | `op` が無い / 壊れている / セッション無し | 解決するとプロンプト or ハング |

**TTY 判定を採らなかった理由**: `io::stdin().is_terminal()` は「人がいるか」の近似でしかない。セッションが生きている無人実行（`OP_SERVICE_ACCOUNT_TOKEN` や事前 signin）では probe を不必要に落とし、逆にセッションの無い対話実行では TTY があるからと解決に進んでプロンプトを出す。**測りたいのは「人がいるか」ではなく「プロンプトが出るか」**で、後者は直接測れる。

## 2. `check_onepassword` を最初に動かす

判定が必要な側より後に走っていては可否を渡せない。`cfg` が取れた直後、`check_worktree_location` より前へ移した。副作用として `doctor` の出力で 1Password の行が先頭付近に来る。

## 3. ゲートはプラグイン単位で判断する

`plugin_needs_onepassword()` が「`plugins/{name}.toml` に `op://` があるか」または「task_source かつ `[llm].api_key_ref` が `op://`」で判定する。**1 つのプラグインの `op://` が、シークレットを必要としない他のプラグインの probe まで黙らせてはいけない。**

kind は**マニフェストと config のロスターの両方**に尋ね、**どちらか一方でも task_source と言えばゲートする**。

当初はロスターだけを見ていたが、これは**穴になる**。`plugin_spec` が `llm_info()` を呼ぶかどうかは **`manifest.kind`** で分岐するのに対し、両者の不一致を修復する仕組みがどこにも無い:

- `config validate` は **`manifest.kind` を一度も読まない**。ロスターの自己申告 kind を「参照している workflow が期待する kind」と突き合わせるだけで、しかも workflow から参照されている場合にしか働かない
- `plugin install` は config を書かない。`set_plugin_enabled` も既存エントリには `kind_if_new = None` を渡す

したがって、**マニフェストの kind が変わるプラグイン更新を挟むとロスターは古いまま固定される**。片側だけを信じると、その不一致が無人ハングを再び開く。安全側（skip する側）に倒す。

マニフェストが読めない場合の特別扱いは要らない。`plugin_spec` はマニフェストを最初に読み、失敗すれば何も解決する前にエラーを返すため。

## 4. `Check` に `skipped` を足す

「走らなかった」を「通った」と区別できるようにする。`ok` は true のまま（走らないことは失敗ではない）なので exit code には影響しない。`warning` と同じく `skip_serializing_if` なので、**`--json` の消費者から見た文書形状は変わらない**。

これは既存の 3 通りに分裂していた skip 表現（何も push しない / `Check::warn` の detail を `"skipped: "` で始める / `op://` だけ `Check::ok` + 説明文）を収束させる意味も持つ。

## 5. `llm` / `hook-token` の文面を正直にする

旧文面「`(checked by the 1password probes, not resolved here)`」は**嘘だった**。probe が見ているのは `op` の存在とセッションの有無であって、その item が解決するかではない。「非対話を保つためここでは解決しない、1password の検査は上を見よ」という狭い主張に書き換えた。

# Alternatives considered

## 案 A: `check_plugins` も `op://` を解決しない（`plugin_spec` にオフラインモード）

**不採用。** プラグインは実シークレットが無いと起動も `config/validate` 応答もできない（Slack の TokenGuard は `auth.test` を叩く）。解決は付随処理ではなく**ライブ疎通 probe の本体**なので、これをやると `plugin:{name}` チェックが空洞化する。加えて `secret_resolver` は本番コードに store 注入経路を持たず、`plugin_spec` / `plugin_init_config` のシグネチャと `run_cmd` / `focus_cmd` の呼び出し側まで波及する。

## 案 B: プラグインのライブ probe も `--online` の裏に置く

**不採用。** 線引きとしては一貫するが、`doctor` の既定挙動に対する**破壊的変更**である。`plugin:{name}` は #64 以来 doctor の中心的な価値で、既定から外すと「プラグインが壊れていても doctor が緑」という別の穴が開く。本 ADR の案では**セッションがあれば従来どおり走る**ので、1Password 利用者が恒常的に半分しか診断されない状態にはならない。

## 案 C: 挙動は変えず、約束（ADR-0006）の方を狭める

**不採用。** 最も安いが**無人ハングは直らない**。#289 が報告している実害はそれである。ただし文面の是正自体は必要なので、決定 5 として取り込んだ。

## 案 D 単独: 非対話環境（`!IsTerminal`）を検出して縮退する

**不採用（案 E に吸収）。** 判定の前例は `check_orphans` / `check_orphan_panes` にあるが、上記のとおり TTY は測りたい条件の近似でしかない。

# Consequences

- **無人実行（CI・cron）で `totsuka doctor` がハングしなくなる。** セッションが無ければ該当 probe は `skipped` として報告され、exit code は変わらない。
- **1Password 検査が初めて実際に動く。** `op://` を使っている構成では `1password` / `1password-session` の 2 行が新たに出る（これまで 0 行だった）。既存の期待値を持つスクリプトには見た目の変化になる。
- **`doctor` の出力順が変わる。** 1Password の行が先頭付近へ移動する。
- **`--json` に `skipped: true` が現れうる。** 省略可能フィールドなので既存の消費者は壊れないが、「`ok: true` なら検査済み」と読んでいたスクリプトは `skipped` を見る必要がある。
- **セッションがあるときの挙動は従来と完全に同じ。** ゲートは `WouldPrompt` のときだけ効く。
- 1Password の Service Account トークン（ADR-0006 が「後続」としたもの）が入れば、無人でも `whoami` が通るため `Ready` になり、ゲートは自然に無効化される。

# 検証

`op` の偽物を PATH に置く統合テストで、**`op read` が一度も spawn されないこと**を marker ファイルの不在で確認する（「doctor が終了した」だけでは証明にならない）。あわせて、セッションがあるときは probe が従来どおり走ること、シークレットを必要としないプラグインは巻き添えにならないこと、1Password の検査がゲート対象より**前**に現れることを固定している。

toml パーサの誤用については、`parse::<Value>()` がドキュメントで**失敗すること自体**を assert するユニットテストを置いた（将来 `Value` がドキュメントを解せるようになったらコメントの方が古くなるため）。

# 関連

- [ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) — 本 ADR が実装を追随させた対象
- [ADR-0016](/decisions/adr-0016-doctor-online-probe.md) / [#267](https://github.com/tomoya-k31/totsuka/issues/267) — このギャップを発見した経緯。`--online` の文言修正のみ行い是正は範囲外とした
- [ADR-0012](/decisions/adr-0012-cli-exit-codes-json-errors.md) — exit code 体系。`skipped` が exit に影響しない根拠
- [orchestrator-cli](/components/orchestrator-cli.md) — doctor のチェック一覧
