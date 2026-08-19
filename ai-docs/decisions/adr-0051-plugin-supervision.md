---
type: Decision
title: ADR-0051 プラグインの死活監視を全 kind へ広げ、予算付きで自動再起動する
description: "死活検知が agent_ide だけに配線され、しかも通知ストリームの終端に紐づいていた非対称を解消する決定。検知は子プロセスの終了そのものに紐づけ、Liveness で「クラッシュ」と「意図した停止」を区別し、予算を使い切ったらエスカレーションする。プロトコル変更なし。"
resource: https://github.com/tomoya-k31/totsuka/issues/495
tags: [decision, plugin, lifecycle, supervision, reliability, adr]
generated: { by: claude-code/opus-5, at: 2026-08-20T00:00:00Z }
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

stable。実装は #495（本 ADR と同一 PR）。実機検収は未了。

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

使い切ったら `NotifierEvent::Escalated` を配送する。この通知は `task_id` も `workflow` も `None` にする: **プラグインの死はどのタスクのものでもなく**、たまたま走っていたタスクに紐づけると誤帰属になる。再起動回数は `RunSummary.plugin_restarts` にも出す（`totsuka run --json` の契約への追加フィールド）。

`[plugins.{name}].restart = false` で個別に無効化できる。**無効化しても検知は残る** — ログにも出るし、agent なら在席タスクも畳まれるし、エスカレーションもする。止まるのは再起動だけで、これはプラグインを手で調べている人間が欲しい形である。

# Consequences

## 良くなること

* `task_source` / `notifier` の死亡が、**そのプラグインの仕事が止まった瞬間に**可視化される
* 一過性のクラッシュが人手を介さず復旧する
* `plugin_host` の「v1 does not auto-restart」は本 PR で撤回した。宣言を残したまま挙動だけ変えるのは、このリポジトリが繰り返してきた失敗そのものである

## 悪くなること・注意点

* **再起動が「一度死んだ」事実を隠しうる。** だからエスカレーションと `RunSummary` への計上を同一 PR に含めた。ここを落とすと、無音の故障を**別の無音**に置き換えただけになる
* **`doctor` に新しいチェックは足さない。** 既存の `plugin:{name}` チェックが「起動して `initialize` して `config/validate` に答えるか」を既に見ており、`doctor` は別プロセスなので**稼働中の orchestrator のプラグイン集合を観測できない**。ここで新チェックを足すのは、[#496](https://github.com/tomoya-k31/totsuka/issues/496) が扱っている「重複した宣言」を自分で作ることになる
* `PluginSet.specs` に spec が無いプラグインは**検知されるが再起動されない**。手で `PluginSet` を組み立てるテストがこの経路を通る。意図した縮退であり、ログにそう出る

# 検証

`crates/orchestrator-core/tests/plugin_supervision.rs` の 4 本と、`tests/plugin_host.rs` の `Liveness` 3 本。`mock_plugin` に**ファイル backed のカウンタ**で「最初の N 回の起動だけ `initialize` 直後に exit(1) する」`suicide` モードを足して駆動する — 起動のたびに別プロセスになるので、プロセス内カウンタでは「いつか復帰する」を書けない。

**予算を使い切ったときにエスカレーションが飛ぶこと**を独立した testcase にしてある。検知配線を外すと 4 本中 3 本が落ちることを確認済み（残る 1 本は「無効化したら何も起きない」の検証なので落ちなくて正しい）。
