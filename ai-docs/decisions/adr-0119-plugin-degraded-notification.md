---
type: Decision
title: ADR-0119 プラグインが実行中の異常を plugin/degraded 通知で core に報告する
description: task_source プラグインが実行中に気づいた異常（Slack プラグインの LLM のキー拒否・到達不能など）を、P→O の notification plugin/degraded {key, message, active} で core に届け、health.json の Degradation::PluginDegraded として status / menu / Mac アプリの Degraded 行に出す決定。core はプラグイン×key で 1 件だけ保持し、active=false・プラグインの終了で消す。ADR-0070 が範囲外として残した「プラグイン自身の LLM の健全性」の答え。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/methods.rs
tags: [decision, protocol, health, plugin, llm, macos, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T20:22:27+09:00 }
status: stable
owner: tomoya-k31
---

# Status

**採択（stable）。** #927 の stacked PR 2/3 で実装した（protocol 0.7.9）。[ADR-0070](/decisions/adr-0070-llm-liveness.md) の Alternatives が「task_source プラグイン自身の LLM には protocol の通知が要る → 別 ADR」と置いていた課題に答える。

# Context

core は自分の `[llm]` の健全性を `LlmHealth` で追い、health.json → `menu --json` → Mac アプリの Degraded 行まで出している。一方、Slack プラグインの分類器の失敗は stderr のログにしか出ない。

- プロトコルに P→O の健全性の口が無い。P→O は `task/submit` / `task/lookup` と agent の `state/notification` だけ
- source プラグインが notification を送っても、core は `wire_source` で読まずに捨てている（unbounded channel に積もる）
- `doctor --online` が probe するのも core の `[llm]` だけ

[ADR-0120](/decisions/adr-0120-slack-reply-check.md) で Slack プラグインは判定に失敗すると処理を中断するようになる。LLM が壊れている間、メンションは 1 件もタスクにならないので、運用者が気づける場所が要る。

# Decision

## 1. P→O notification `plugin/degraded` を足す

```json
{ "method": "plugin/degraded", "params": { "key": "llm", "message": "…", "active": true } }
```

- `key`: プラグイン内で異常の種類を識別する短い文字列。同じ key の後続通知は前のものを置き換える
- `message`: 人間向けの 1 文。core は解釈しない。資格情報や本文を含めないのはプラグインの責任
- `active`: `false` でその key を消す

汎用にしたのは、Gateway の「一度も受信していない」のような他の実行時異常にも同じ口が使えるため。LLM 専用の型にすると用途ごとに通知が増える。

## 2. core は Degradation::PluginDegraded として載せる

`Degradation::PluginDegraded { plugin, key, message }`。`message()` は ``"plugin `{plugin}`: {message}"``。`degradations()` はプラグイン×key の表から組む。プラグインが閉じたら（`PluginDown`）その表を消す — 閉じた事実は `plugin_down` が言うので、古い異常を残さない。通知の受信とプロセスの終了は別のタスクが運ぶので、閉じたプラグインから遅れて届いた active の報告は捨てる（残すと誰も消さない）。

## 3. Mac アプリは変えない

`menu --json` の `degraded: [String]` に文が 1 本増えるだけで、アプリはすでにそれを Degraded 行として描く。「壊れたときだけ出す」はこれで満たす。

## 4. プロトコルは patch（additive）

notification を送るのはプラグイン側だけで、古い core は読まずに捨てる（今と同じ）。manifest の下限は動かさない。

# Consequences

- 送り手は Slack プラグインが最初で（`llm::Reported`、[ADR-0120](/decisions/adr-0120-slack-reply-check.md)）、LLM 呼び出し（返信要否判定・repo 分類）の 401/403 と到達不能（transport・timeout・5xx）を key `llm` で active、成功で inactive にする
- core は source と agent の notification を読む。notifier の notification は今も読まない
- 文は 300 文字で切る（health.json と menu に載る文の長さを、送り手の誤りで無制限にしないため）
- SDK の `DegradedReporter` は key ごとに最後に送った文を覚え、変化したときだけ書く
- 異常は「最後の呼び出しの結果」であって定期 probe ではない。メンションが来ない間は古い状態のまま残る
- `status --json` には `kind = "plugin_degraded"` で出る

# Alternatives considered

| 案 | 不採用の理由 |
|---|---|
| `plugin/llm_health {state}` の LLM 専用通知 | 型は厳密だが用途が LLM に限られ、次の異常でまた通知を足すことになる |
| core の `[llm]` を共用しているときは報告しない | `[slack.llm]` を独自に書いた構成で見えないまま残る |
| プラグインが自分で health ファイルを書く | 読み手（CLI）がプラグインのファイル配置を知ることになり、ADR-0058 の境界を越える |
