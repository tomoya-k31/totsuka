---
type: Decision
title: ADR-0042 timeout_secs = 0 は「即エスカレート」ではなく「D-03 掃引の対象外」を意味する
description: "attended pane（人間が pane を見ている）前提の workflow では無音は異常の証拠にならないため、timeout_secs = 0 を D-03 無音掃引のオプトアウトとして定義した決定。従来の 0 は最初の掃引でほぼ必ずエスカレートする罠値で、意図して使える意味を持っていなかった。トレードオフとして、真にハングしたエージェントもその workflow では検知されない。"
resource: https://github.com/tomoya-k31/totsuka/issues/439
tags: [decision, timeout, escalation, attended-pane, adr]
generated: { by: claude-code/fable-5, at: 2026-08-13T00:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-439
    resource: https://github.com/tomoya-k31/totsuka/issues/439
    title: "feat(core): timeout_secs = 0 で D-03 無音掃引を無効化できるようにする"
  - id: issue-440
    resource: https://github.com/tomoya-k31/totsuka/issues/440
    title: "feat(profile): design/implement の完了判断を人間の pane 上承認に移す"
---

# Status

stable（[#439](https://github.com/tomoya-k31/totsuka/issues/439)）。attended pane 運用の本体設計（[#440](https://github.com/tomoya-k31/totsuka/issues/440)）の前提となる汎用改修。

# Context

D-03 の無音掃引は「最終フックシグナルから `timeout_secs`（既定 30 分）沈黙したタスクをエスカレートする」仕組みで、無人 pane における**止まったエージェントの検出**が役割である。掃引対象は `dispatched` / `running` / `publishing` のみで、`WaitingInput` / `Verifying` / `Escalated` は「人間待ちであって沈黙ではない」として最初から対象外になっている。

design / implement 系 workflow を attended pane（人間が pane を見ている、離席しても戻って確認する）前提で運用する場合、この掃引は誤発火源にしかならない:

- 長いビルド・テストを含む 1 ターンは、フック信号（Stop / SessionStart / SessionEnd / Notification）を 30 分以上出さないことがある
- 人間が pane を見ているので「止まったエージェントの検知」は人間側で足りている

一方、無効化を意味する設定値は存在しなかった。`timeout_secs = 0` と書くと、掃引の判定 `(now - last_signal_at) > 0` が最初の掃引でほぼ必ず真になり、**実質「即エスカレート」**として振る舞う。0 を意図的に書いて即エスカレートを期待する運用は考えにくく、従来の 0 は事実上の設定ミス値だった。

# Decision

**`timeout_secs = 0` を「この workflow は D-03 掃引の対象外」と定義する。** `sweep_signal_timeouts` は解決したタイムアウトが 0 の workflow のタスクをスキップする。

- profile とは無関係の汎用プロパティ変更。全 workflow（answer / triage 含む）で書ける
- 省略時の既定（30 分）は不変。オプトアウトは明示的に `0` と書いたときだけ

# Consequences

- **真に止まったエージェント（クラッシュ・ハング）も、その workflow では永遠に検知されない。** D-03 の本来の役割をその workflow では放棄する。attended pane 前提の workflow でのみ使い、無人 pane で走る workflow（Slack 系など）には設定しないこと。[設定リファレンス](/development/config-reference.md)に同じ注意を明記した
- 既存 config で `timeout_secs = 0` を書いていた場合、挙動は「即エスカレート」から「掃引なし」へ変わる。前者を意図して使う構成は考えにくいため、破壊的変更としては扱わない
- D-02（UNKNOWN 連続エスカレート）と R-03（マーカー欠落ブロック）は影響を受けない。オプトアウトされるのは無音掃引だけで、エージェントが「何かを言った」ことに起因するエスカレーションは残る

# 不採用案

- **profile でデフォルトを分岐（design/implement だけ長い既定値にする)**: 「止まったエージェントの検知」が全利用者で一律に遅くなり、値の根拠付けも難しい。オプトアウトは workflow を書く人が明示的に選ぶべき
- **巨大な値を書くワークアラウンドの容認**: 動くが意図が読めず、「なぜ 31536000 なのか」を config を読む人全員が推測することになる。0 に意味を与える方が 1 行の変更で意図が残る
