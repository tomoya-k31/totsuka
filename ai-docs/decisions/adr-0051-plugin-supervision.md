---
type: Decision
title: ADR-0051 プラグインの死活監視を全 kind へ広げ、予算付きで自動再起動する
description: "死活検知が agent_ide だけに配線され、しかも通知ストリームの終端に紐づいていた非対称を解消する決定。検知は子プロセスの終了そのものに紐づけ、Liveness で「クラッシュ」と「意図した停止」を区別し、予算を使い切ったらエスカレーションする。プロトコル変更なし。"
resource: https://github.com/tomoya-k31/totsuka/issues/495
tags: [decision, plugin, lifecycle, supervision, reliability, adr]
generated: { by: claude-code/opus-5, at: 2026-08-20T00:00:00Z }
verified:
  - { by: human:tomoya-k31, at: 2026-08-20T10:30:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: issue-495
    resource: https://github.com/tomoya-k31/totsuka/issues/495
    title: "feat(core): プラグインの死活監視と自動再起動を全 kind へ広げる"
  - id: adr-0008
    resource: /decisions/adr-0008-task-submit-push-ingestion.md
    title: "ADR-0008 task/submit による push 取り込み"
  - id: adr-0030
    resource: /decisions/adr-0030-herdr-pane-layout.md
    title: "ADR-0030 herdr の pane レイアウト（design_preview が誰にも読まれなかった件）"
---

# Status

stable。実装は #495（本 ADR と同一 PR）。**実機検収済み**（2026-08-20）— 実 herdr / 実 Slack を相手に、notifier と task_source の両方を本当に kill して検知・再起動を確認した。Slack ソースは Socket Mode の再接続（`socket mode: connected (hello)`）まで戻る。バックオフは実測 1→2→4→8→16 秒で、5 回で `gave up restarting after 5 attempts in 300s` に到達した。

# Context

プラグインはサブプロセスで動き、クラッシュはホストから隔離されている。しかし**その通知が kind によって非対称だった**。

| kind | 死んだときに起きていたこと |
|---|---|
| `agent_ide` | 通知ストリームの受信ループ終了 → `PluginEvent::Closed` → 在席タスクを `Fail`・スロット解放・notifier へ配送 |
| `task_source` | `take_incoming_requests` のループが**黙って終わる**。イベントは一切発生しない |
| `notifier` | 受信ループを持たない。次の `notify` が warn になるだけ |

結果、Slack ソースが落ちた `totsuka run --watch` は**起動し続けたまま、永久にタスクを受け付けない**。痕跡は WARN 1 行だけで、`run --json` のサマリにも `doctor` にも現れなかった。再起動も無く、`plugin_host` の doc comment は「v1 does not auto-restart」と明記していた。

**この非対称は当時は妥当だった。** §5.3 の設計主眼は「プラグインのクラッシュがホストを道連れにしないこと」で、巻き戻すべき状態（在席タスク）を持つのは `agent_ide` だけである。`task_source` はステートレスに見えたし、`tasks/fetch` のポーリング時代は**次のポーリングが自然な再試行だった**。

**変わったのは 0.2.0 で task_source が完全に push 専用になったこと**（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）。ホストが取りに行かない設計では、プラグインの沈黙とタスクの不在が区別できない。配線だけが取り残されていた。

# Decision Drivers

* この機構の支配的な故障モードは**無音の劣化**であり、無音は本番で最も高くつく。同型の失敗は既に [ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)（誰も読まない `design_preview` が 1 世代残った）でも起きている
* ワイヤを変えずに解決できる — プロトコルのバンプも manifest の更新も不要
* 再起動に必要な材料は既に揃っている（`PluginSpec` は `Clone`、`Plugin::launch` は同じ spec で呼び直せる）

# Options Considered

1. **現状維持。** コスト 0。0.2.0 の push 専用化以降、「ステートレスだから再試行で治る」という前提を失ったまま残る
2. **検知のみ（再起動なし）。** 全 kind に `Closed` を配線し、通知と可視化だけ行う。無音は消えるが復旧は人手
3. **検知 + 予算付き再起動（採用）**
4. **プロセス外の supervisor（launchd / systemd）に委ねる。** プラグインは totsuka の子プロセスであり、独立した supervisor から制御できない。**構造的に成立しない**

# Decision

選択肢 3 を採る。設計上の要点は 4 つある。

## 1. 検知は「子プロセスの終了」に紐づける。ストリームではない

`Liveness`（`Live` / `Crashed` / `ShutDown`）を `plugin_host` に置き、`watch` チャネルで配る。**通知ストリームの終端を検知に使うのをやめた**のが本質的な変更である — あのストリームは購読した agent にしか存在せず、`state_stream` を宣言しない agent には最初から無い。

## 2. `Crashed` と `ShutDown` を区別する。マーキングは先着優先

トランスポートから見ると両者は同じ（子が消え、stdout が閉じる）。区別しないと**正常終了のたびに全プラグインを再起動する**。`shutdown()` は「子が消えうる操作」の**前**に `ShutDown` を書き込み、後から来る EOF の `Crashed` は先着優先で弾かれる。

**`Plugin` を drop すると `Crashed` が立つ**（`kill_on_drop` が実際に子を殺すので、これは正しい）。害が無いのは、再起動が drop するのは**古いインスタンスだけ**で、その watcher は既に発火して終了しているためである。

## 3. 再起動の判断はコアに置く。トランスポートには置かない

`plugin_host` は「死んだ・理由はこれ」までを報告し、どうするかは決めない。**答えが kind ごとに違う**からである — `agent_ide` は先に在席タスクを畳む必要があり、`task_source` は購読を張り直す必要があり、`notifier` はどちらも要らない。

**agent の順序は load-bearing である。** 在席タスクを `Fail` にし `sessions` の経路を落とす処理が、新プロセスがセッション ID を配り始める**前**に終わっていなければならない。逆にすると新しい ID が死んだプロセスのタスクに紐付き、復旧不能になる。

## 4. 予算を使い切ったら黙らない

指数バックオフ（1s / 2s / 4s …）で最大 5 回・5 分のスライディング窓。**窓であって通算回数ではない** — 週に 1 回落ちるプラグインはこの予算が止めるべき障害ではないし、`--watch` は何週間も上がり続ける。

**プラグインが下がったまま**になる経路はすべて `NotifierEvent::Escalated` を配送する — 予算切れ、`restart = false`、spec 未記録の 3 つである。この通知は `task_id` も `workflow` も `None` にする: **プラグインの死はどのタスクのものでもなく**、たまたま走っていたタスクに紐づけると誤帰属になる。

計上は **2 本立て**にする。`RunStats.plugin_crashes` は「何回死んだか」で、その後どうなったかに関わらず `on_plugin_closed` で数える。`RunStats.plugin_restarts` は「何回復帰したか」。**片方だけでは足りない** — `plugin_restarts` しか無いと、再起動しない設定でクラッシュしたケースが 0 のままになり、「一度も死ななかった」と区別できない。2 つが等しければ全部直っており、少なければ何かがまだ下がっている。

`[plugins.{name}].restart = false` で個別に無効化できる。**無効化しても検知は残る** — ログに出て `plugin_crashes` に計上され、`escalated` も飛び、agent なら在席タスクも畳まれる。止まるのは再起動だけで、これはプラグインを手で調べている人間が欲しい形である。

## 既知のギャップ: クラッシュ窓中に queue されたタスク（#499）

クラッシュした `Plugin` は再起動が**成功**するまでエンジンのマップに残り、`resolve_dispatch_target` は capability しか見ないので、**死んだハンドルへ dispatch が解決する**。

2 つの時計が噛み合っていない: dispatch の自動再試行は `DISPATCH_RETRY_LIMIT` 回 × `SETTLE_TICK` 間隔で**1 秒未満**に尽きるのに対し、最初の再起動バックオフだけで**1 秒**ある。したがって**クラッシュ窓中に queue されたタスクは、プラグインが最初の復帰を試みる前に終端 `Failed` へ落ちうる** — この 1 クラスについては、下の「一過性のクラッシュが人手を介さず復旧する」が成立しない。

**本 ADR ではこれを直さない。** 直し方が自明ではないためである。dispatch 側で liveness を見て park する実装を試したところ、`crash_on_dispatch`（その dispatch 自体がプラグインを殺すケース）でタスクが `Failed` にならず queued のまま残り、既存の契約（`e2e_agent_crash_fails_task_and_orchestrator_survives`）が壊れた。**「まだ試していないタスクは待つ」と「自分の dispatch が失敗し続けているタスクは予算を使い切る」を分ける規則**が要り、それは #492 の再試行予算・one-shot の終了条件と噛み合わせて別途設計する必要がある。

`first_backoff` を再試行予算より短くするのは確率を下げるだけで構造ではないので、緩和策としても採らない。→ [#499](https://github.com/tomoya-k31/totsuka/issues/499)

# Consequences

## 良くなること

* `task_source` / `notifier` の死亡が、**そのプラグインの仕事が止まった瞬間に**可視化される
* 一過性のクラッシュが人手を介さず復旧する（**ただしクラッシュ窓中に queue されたタスクは除く** — 上の既知のギャップ / [#499](https://github.com/tomoya-k31/totsuka/issues/499)）
* `plugin_host` の「v1 does not auto-restart」は本 PR で撤回した。宣言を残したまま挙動だけ変えるのは、このリポジトリが繰り返してきた失敗そのものである

## 悪くなること・注意点

* **再起動が「一度死んだ」事実を隠しうる。** だからエスカレーションと `RunSummary` への計上を同一 PR に含めた。ここを落とすと、無音の故障を**別の無音**に置き換えただけになる。
  実際、レビューで**この穴を開けたまま出していた**ことが分かった: `restart = false` の分岐が `escalate_dead_plugin` を呼ばず、`RunStats` にも死亡の counter が無いのに、doc と本 ADR は「エスカレーションもする」と書いていた。**コードが提供していない保証を書いた**わけで、このリポジトリが繰り返している失敗そのものである。回帰テストも `plugin_restarts == 0`（＝抑止したものの不在）しか見ておらず、名前が言っている "without losing detection" を一度も検査していなかった
* **`doctor` に新しいチェックは足さない。** 既存の `plugin:{name}` チェックが「起動して `initialize` して `config/validate` に答えるか」を既に見ており、`doctor` は別プロセスなので**稼働中の orchestrator のプラグイン集合を観測できない**。ここで新チェックを足すのは、[#496](https://github.com/tomoya-k31/totsuka/issues/496) が扱っている「重複した宣言」を自分で作ることになる
* `PluginSet.specs` に spec が無いプラグインは**検知されるが再起動されない**。手で `PluginSet` を組み立てるテストがこの経路を通る。意図した縮退であり、ログにそう出る

# 検証

`crates/orchestrator-core/tests/plugin_supervision.rs` の 4 本と、`tests/plugin_host.rs` の `Liveness` 3 本。`mock_plugin` に**ファイル backed のカウンタ**で「最初の N 回の起動だけ `initialize` 直後に exit(1) する」`suicide` モードを足して駆動する — 起動のたびに別プロセスになるので、プロセス内カウンタでは「いつか復帰する」を書けない。

**プラグインが下がったままになるときにエスカレーションが飛ぶこと**を、予算切れと `restart = false` の 2 つの経路それぞれで testcase にしてある。検知配線（`wire_liveness`）を外すと **4 本とも落ちる**ことを実測で確認済み。

最初の版はここが 3 本だった。`restart = false` のテストが `plugin_restarts == 0`、つまり**抑止したものが起きていないこと**しか見ておらず、名前が言っている "without losing detection" を一度も検査していなかったためである — 検知が丸ごと壊れていても通るテストだった。**抑止したものの不在ではなく、残ると約束したものの存在を assert する。**
