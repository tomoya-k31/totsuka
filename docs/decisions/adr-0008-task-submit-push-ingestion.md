---
type: Decision
title: ADR-0008 task/submit による push 型タスク取り込みと tasks/fetch の段階的廃止
description: プラグイン→Orchestrator の push RPC task/submit（persist-before-ack・冪等）を protocol 0.1.6 で追加し、tasks/fetch を deprecated 化して 0.2.0 で削除する決定。ADR-0003 の「バッファ + 短周期 tasks/fetch」判断を amend し、ポーリング型ソースは plugin-sdk の PollSource で自前タイマー化する。
tags: [plugin, protocol, task-source, push, ingestion, architecture]
timestamp: 2026-07-20T00:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-19（エピック [#182](https://github.com/tomoya-k31/totsuka/issues/182)、実装 #183〜#190）。
Phase C（#190）実装済み — 2026-07-20、protocol `0.2.0` で `tasks/fetch` と
Orchestrator 側ポーリング機構を削除し、この ADR は完全実施状態になった。

[ADR-0003](/decisions/adr-0003-slack-reply-assistant.md) の Decision §2（バッファ + 短周期 `tasks/fetch`）を amend する。同 ADR 自身が「コア無変更はエピック本体に対する判断であり、恒久の禁止ではない」と明記していた将来 F 案件の実施にあたる。

# Context

タスク取り込みは `tasks/fetch`（O→P、`poll_interval_secs` 周期のポーリング）のみだった。この pull 専用契約は 2 種類のコストを生んでいた:

1. **イベント駆動ソースの実装負担**: Slack のような push 型ソースは、受信イベントをプラグイン内メモリバッファに積み、`tasks/fetch` で drain させる「push→poll ブリッジ」を自前実装する必要がある（バッファ・drain・dedup・スレッド安全性）。プラグインを第三者が実装しやすくするという目的に反する。
2. **喪失窓**: Slack への ack は受信時に済んでいるため、orchestrator が pull する前にプラグインが落ちるとバッファ上のタスクは失われ、再送もされない（ADR-0003 が明示的に許容していたトレードオフ）。

一方 orchestrator 側には、`tasks` テーブルの `UNIQUE(source, source_task_id)` と `upsert_task`（`ON CONFLICT DO NOTHING`）による冪等 ingest、および起動時 cycle による `Queued` 行の回収が既にあり、push を受けて「永続化してから ack する」ために必要な足場は揃っていた。

# Decision

## 1. protocol 0.1.6 で P→O request `task/submit` を追加（additive）

- **persist-before-ack**: Orchestrator は task を `upsert_task` でコミットしてから ack を返す。ack を受けたプラグインはそのタスクを忘れてよく、**プラグイン側バッファは不要になる**。
- **ack は 3 値とも最終**（再送禁止）: `accepted`（永続化・キュー投入）/ `duplicate`（冪等キー衝突 = 再送や再取得。破棄してよい）/ `rejected`（workflow 不一致など恒久的に処理不能。reason 付き）。
- **リトライ可能系は JSON-RPC error**: `NOT_ACCEPTING(-32004)`（シャットダウン中）/ `SUBMIT_OVERLOADED(-32005)`（per-plugin in-flight 予算超過）/ `INTERNAL_ERROR(-32603)`（永続化失敗）はバックオフ再送。submit は冪等（ack 喪失後の再送は `duplicate` で吸収）なので再送は常に安全。
- dedup は Orchestrator 側の既存冪等キー（`UNIQUE(source, source_task_id)`）に一元化し、プラグイン側の seen-set を不要にする。

## 2. `Capabilities.task_submit` と initialize でのトリガー供給

- `task_submit = true` を宣言した source を Orchestrator は**一切ポーリングしない**。
- push source は監視条件を `tasks/fetch` の引数としてではなく `InitializeParams.triggers`（`[[workflows]]` 定義順の workflow 名 + trigger）で受け取る。`poll_interval_secs` はキーを残して意味を再定義: push source へは initialize で転送され**プラグイン内部の fetch 周期**になる（レガシー source へは従来どおり Orchestrator 側ポーリング周期）。ユーザー設定は壊れない。

## 3. `tasks/fetch` は段階的に廃止し 0.2.0 で削除

- **Phase A（0.1.6）**: 両経路併存。`tasks/fetch` は doc 上 deprecated。
- **Phase B（同リリース内）**: 同梱 3 ソース（slack / github / notion）を push 移行。`task_submit` 未宣言 source をポーリングするたび 1 回/run の deprecation warn。
- **Phase C（0.2.0、最低 1 minor リリース後、実施済み）**: `tasks/fetch`
  （`TasksFetchParams`/`TasksFetchResult` 含む）と Orchestrator 側ポーリング
  機構（`fetch_and_ingest`/`poll_sources`/`poll_interval_for`/
  `EngineSettings.poll_intervals`）を削除。`^0.1` manifest は F-54 の想定動作
  として起動拒否される（0.1.6→0.2.0 が猶予窓、v0.1.4 で満たした）。
  push-only プラグインは `>=0.1.6, <0.3` を宣言して削除をまたいで動作する。
  `one-shot`（`--watch` なしの `totsuka run`）は全 source が push 専用になった
  ことで、起動直後の未着 push を待たず即終了する潜在バグが露呈したため、
  `settled()` が空でも直近イベントから 2 秒（`ONE_SHOT_GRACE`）の静穏期間が
  経過するまでループを維持するよう修正した
  （`crates/orchestrator-core/src/run/mod.rs`）。
- ポーリングが自然な source（github / notion）は、`crates/plugin-sdk` の `PollSource` ヘルパー（trigger × 周期 → fetch → submit のタイマーループ）で自前タイマー化し、実装負担を SDK に吸収させる。

## 4. 併せて `crates/plugin-sdk` を新設

stdio runtime（単一 writer タスクによる行アトミック化 — プラグイン側 stdout インターリーブの恒久修正）/ dispatch ボイラープレート / `SubmitClient`（バックオフ再送）/ `PollSource` の最小 4 モジュール。HTTP クライアント・LLM ヘルパー・config スキーマは範囲外。

# Consequences

- Slack の喪失窓は「poll 間隔」から「submit 往復のミリ秒」に縮小する。**残余窓**（Slack envelope を ack 済みだが submit の ack 前にプラグインが死ぬ）は残る — 完全に閉じるには Socket Mode の ack を submit 完了後へ遅延させる必要があり、Slack の 3 秒再送制約と衝突するためスコープ外とする。
- persist と ack の間でクラッシュしてもプラグイン再送 → `duplicate` で二重取り込みは起きない。persist と dispatch の間のクラッシュは既存の起動時 `Queued` 回収が拾う（ack ≠ dispatch）。
- Engine はプラグイン起点 request を受ける必要がある（plugin host の reader に第 3 分岐、`PluginEvent::TaskSubmit`）。現状 reader はプラグイン起点 request を Notification として黙って破棄しており、この修正はそれ自体が堅牢性改善。
- 0.2.0 でサードパーティの `^0.1` プラグインは起動拒否される（意図的・F-54）。移行手順は [plugin-dev-guide](/development/plugin-dev-guide.md) に記載する。
- タスク取り込みの順序性は per-source FIFO（単一 channel → 単一イベントループ）となり、バッファ drain 方式より弱くならない。
- `--dry-run` は push source に対しては原理的にプレビュー対象がない（fetch する対象が無い）ため、常に空の結果を返す no-op になった。

# Citations

[1] [Issue #182 エピック](https://github.com/tomoya-k31/totsuka/issues/182)
[2] [ADR-0003 Slack メンション代理返信アシスタントの設計](/decisions/adr-0003-slack-reply-assistant.md)
[3] [plugin-protocol コンポーネント](/components/plugin-protocol.md)
