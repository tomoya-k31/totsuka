---
type: Decision
title: ADR-0073 プラグインの「エラーではない警告」を protocol 0.7.3 の warnings で運ぶ
description: "プラグインが「設定は正しいが伝えたいこと」をホストへ渡す口を ConfigValidateResult.warnings として足す決定。それまでの選択肢は errors（正しい設定を拒否する）とプラグインのログ（doctor が読まない）の 2 つだけで、どちらも誤りだったため、知っている事実が最も役に立つ場所で不可視になっていた。加算的・省略可能なので既存プラグインは無改修、マニフェストの下限も動かない。doctor は warning チェックとして描き、ok は true のまま。第 1 の利用者は Event Gateway の「一度も受信していない / しばらく静か」の区別。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/methods.rs
tags: [decision, adr, plugin-protocol, doctor, diagnostics, slack, gateway]
generated: { by: claude-code/opus-5, at: 2026-09-13T23:43:32+09:00 }
status: stable
owner: tomoya-k31
---

# Status

**採択（stable）。** protocol 0.7.3（#662）。加算的な変更なので、既存のどの決定も置き換えない。

# Context

## 症状が同じで原因が違うものを、区別する手段が無かった

[ADR-0072](/decisions/adr-0072-slack-event-gateway.md) の Event Gateway 方式では、「繋がっているのに何も来ない」の原因が Socket Mode より増える。ADC 切れ・IAM 不足・サブスクリプション名の誤り・Slack 側の Request URL 未設定が、**すべて同じ症状**になる。

このうち最初の 3 つは `pull` が失敗するので、区別する材料は HTTP のステータスコードとして既に手元にある。問題は 4 つ目で、**Request URL が入っていない場合、キューは正常に存在し、権限もあり、ただ空である**。正常に空なキュー（静かな週末）と見分けがつかない。

見分けるには「これまでに一度でも受信したか」を知る必要がある。それはプラグインが持てる情報だが、**プラグインからホストへ渡す口が無かった**。

## 既存の 2 つの口は、どちらも誤りだった

| 口 | なぜ使えないか |
|---|---|
| `ConfigValidateResult.errors` | `valid` が false になる。**構築直後のまだ何も受信していない状態は正しい設定**であり、これを赤くすると「赤いのが普通」を教えることになる。診断が信用されなくなる |
| プラグインの `tracing` ログ | `totsuka doctor` はプラグインのログを読まない。**知識が、それが最も役に立つ唯一のコマンドに届かない** |

同じ形の問題は既にもう 1 つある。#658 のスコープ警告（`usergroups:read` が無いとグループメンションだけ黙って死ぬ）は、まさに「設定は有効だが伝えたいこと」であり、いまログにしか出ていない。

# Decision

## 1. `ConfigValidateResult` に `warnings` を足す（protocol 0.7.3）

```rust
pub struct ConfigValidateResult {
    pub valid: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,   // 0.7.3
}
```

**`warnings` は `valid` に影響しない。** 「有効だが警告あり」がこの口の存在理由であり、`valid` に畳み込めば「知らせたい」が「拒否する」に化ける。

`errors` と同じ「原因 → 次のアクション」の形にする。**行動できない警告は雑音であり、雑音は診断が読まれなくなる経路そのもの**である。

## 2. patch bump で、マニフェストの下限は動かさない

加算的かつ省略可能で、`#[serde(default)]` により**不在と空が同じ主張**になる。古いプラグインは省略し、古いホストは未知キーを無視する。したがって `>=0.6.0, <0.8` のような既存の宣言はすべてそのまま通る。

0.7.1 / 0.7.2 が patch を選んだのと同じ理由である（この 0.x 系では patch が後方互換な追加を表す）。

## 3. `doctor` は warning チェックとして描く

`Check::warn` は既にある（`ok: true` のまま、アクション付きで表示）。`--json` にも `warning: true` として出る。

**警告を 1 件も送らないプラグインの出力は 1 バイトも変わらない。** これは #662 の受け入れ条件でもある —— Socket Mode の既存利用者の `doctor` が変化してはならない。

分割は ` → ` で行う。矢印の無い警告も落とさず、全文を detail にして表示する ——
**行を落とすことだけが、不完全に分割することより悪い結果**だからである。

## 4. 最初の利用者: 受信履歴の 1 ファイル

ドレインループが、**空でない `pull` が返るたび**に受信時刻を `{state_dir}/plugins/{source_name}/gateway-receipt.json` に記録する。`config/validate` がそれを読み、

- 記録なし → **「一度も受信していない」**。構築が未完の形であり、Slack アプリの Request URL は **2 箇所**あるので両方を名指しする
- 記録あり・24 時間以上前 → 「N 日間受信なし」。静かなだけかもしれないと明示する
- 記録あり・最近 → **何も言わない**

**フィルタを通ったレコードではなく、`pull` が何かを返したこと自体を記録する。** この受信履歴が答える問いは「Slack からここまでの経路が動くか」であり、このビルドが捨てたレコードもそれを同じだけ証明する。`filed` を鍵にすると、静かな週を「一度も受信していない」と報告して、正しかった Request URL を再確認させに行くことになる。

**Unix 秒で持つ。** 警告が使うのは「N 日」だけで日付を描かない。RFC 3339 文字列にすると、日付ライブラリを足すか 2 つ目の自前フォーマッタを書くかになる —— このファイルしか読まないフィールドのために。

# Consequences

- プラグインは「設定を拒否する」か「黙る」かの二択から解放される
- #658 のスコープ警告を同じ口に載せ替えられる（**本 ADR では実施しない**。`check_scopes` はネットワークを使い、`config/validate` は意図的にオフラインなので、移すには置き場所の設計がもう 1 つ要る）
- `doctor` の出力に新しい状態が 1 つ増える。「緑」と「赤」の間に「黄」が入るので、**exit code の契約（0 / 1 / 3）は変わらない**ことを明示しておく必要がある
- 受信履歴ファイルは消えても警告が 1 つ出なくなるだけで、動作には影響しない。ドラフトストアと分けたのはこのためである
