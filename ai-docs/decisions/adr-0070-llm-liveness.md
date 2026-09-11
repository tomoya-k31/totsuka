---
type: Decision
title: ADR-0070 LLM ゲートウェイの生存確認 — 起動時・復帰時・沈黙時のプローブと llm_unreachable 縮退
description: "「LLM が生きているか」を run が自分で確かめる方式の決定。LlmRouter に probe / reset_connections を足し、最後の接触から 10 分（不到達中は 60 秒）沈黙したら doctor --online と同じ最小リクエストを spawn して投げ、到達不能（transport / timeout / 5xx）を health.json の llm_unreachable として公開する。接触が無ければ即時 = 起動時に必ず 1 回。復帰検知は壁時計と単調時計の差（30 秒以上）で行い、HTTP 接続プールを捨てて即プローブする。設定キーは足さず、起動も止めない。定期の無条件プローブ・同期の起動時プローブ・OS のスリープ通知・pool_idle_timeout・プラグイン側 LLM の連携は不採用または後続。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core/src/adapters/llm.rs
tags: [decision, llm, health, liveness, run, resume, adr]
generated: { by: claude-code/fable-5-1, at: 2026-09-12T02:51:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。実装済み・テスト green。**実機（実 AI Gateway・実スリープ復帰）では未検収**なので `verified` は無い。検収すべき点は [Consequences](#consequences) の末尾に挙げる。

# Context

totsuka が LLM（OpenAI 互換 AI Gateway、`[llm]`）に頼るのはリポジトリ選択（F-11〜F-14）だけで、無くても `pending` へ倒れて動く設計である。その分、**LLM が死んでいても症状が「少し不便な正常運転」に見える**という問題が [ADR-0016](/decisions/adr-0016-doctor-online-probe.md)（#267）で一度顕在化し、`doctor --online` の `probe_auth` と、`health.json`（#586 / F-110）の `llm_key_rejected` ラッチが入った。

それでも検知できていなかったもの:

| 状態 | core（repo 選択） | Slack プラグイン | health.json |
|---|---|---|---|
| 401 / 403 | `llm_key_rejected` | warn ログ | **出る** |
| 到達不能・タイムアウト・5xx | タスクが `Failed` | picker へフォールバック | **出ない** |
| `run` 起動時点で死んでいる | 最初のタスクまで気づかない | 同左 | 出ない |
| スリープ復帰直後 | 最初の呼び出しが半開き接続で失敗 or タイムアウト | 同左 | 出ない |

つまり「鍵が悪い」は見えるが「ゲートウェイが居ない」は見えず、`run` は起動時に LLM へ一切触らず、スリープ復帰という事象そのものを持っていなかった。

前提として **LLM は HTTP のステートレス呼び出し**なので、Socket Mode や herdr の Socket API のように「接続を維持し、切れたら再接続する」対象ではない。問うべきは「**到達性をいつ再確認するか**」である。

# Decision

## 1. `LlmRouter` に `probe()` と `reset_connections()` を足す（既定実装つき）

- `probe()` の既定は `Ok(())`（テストダブルと、訊く先を持たないルータの唯一正直な答え）。`OpenAiRouter` は **`probe_auth` にそのまま委ねる** —— `doctor --online` が送るのと同じ 1 リクエスト（schema なし・リトライなし・`max_tokens: 1`・本文破棄）。engine の「生きている」と運用者の「生きている」が食い違わないようにするため。
- `reset_connections()` の既定は no-op。`OpenAiRouter` は `RwLock<reqwest::Client>` を新品に差し替え、keep-alive プールを捨てる。実行中のリクエストは clone 済みの旧ハンドルで完走する。

## 2. `AuthLatchRouter` を `LlmHealthRouter` + `LlmHealth` に置き換える

`LlmHealth` はラッチ 2 つと時刻 1 つ:

| フィールド | 立つ条件 | 下りる条件 |
|---|---|---|
| `key_rejected` | 401 / 403 | 成功 |
| `unreachable(reason)` | `LlmError::is_unreachable()` = `Transport` / `Timeout` / 5xx | **ゲートウェイから何か答えが返る**（成功・401・429・400・パース不能、すべて） |
| `last_contact` | 呼び出しまたはプローブが完了するたび更新 | `forget_contact()`（復帰時） |

429 と 401/403 以外の 4xx を到達不能にしないのは、それらが「ゲートウェイは答えている」証拠だからである。`reason` は `transport error: …`（160 字で切る）／`no answer within 30s`／`HTTP 502` の短文で、**応答本文は決して載せない** —— `health.json` と `totsuka status` の端末は redact 層を通らない。ログは**遷移時だけ**（不到達へ落ちたら warn、戻ったら info）。1 時間の障害は 2 行で、プローブ 60 回分の行にはならない。

## 3. `Degradation::LlmUnreachable { reason }` を health.json に足す

F-110 の「毎サイクル問い直せる事実」の 5 つ目。`kind` は `llm_unreachable`。散文は読み手（`status` / `menu`）が組み立てる既存契約のまま。

## 4. プローブは「沈黙したときだけ」、spawn して待たない

`cycle()` の末尾の `probe_llm_if_due`:

- **due の定義**: 最後の接触から `LLM_PROBE_INTERVAL`（10 分）以上。`unreachable` ラッチ中は `LLM_PROBE_INTERVAL_WHILE_UNREACHABLE`（60 秒） —— そのときのプローブの仕事は「復旧に気づくこと」で、直した運用者を 10 分待たせない。**接触が一度も無ければ即時** = 起動時に必ず 1 回。
- **実トラフィックが最良のプローブ**である。呼び出しがあれば `last_contact` が進み、プローブは打たれない。沈黙した `--watch` プロセスの課金は 1 日 144 トークン。
- **`tokio::spawn` で同時 1 本**。結果は `LlmHealthRouter` が `LlmHealth` に記録し、次以降のサイクルが publish する。ループがタイムアウト（既定 30 秒）分止まることはない。このために `Engine` の `L` に `'static` が付いた。

## 5. 復帰検知は 2 つの時計の差で行う

`cycle()` の先頭の `detect_resume`: 前サイクルからの**壁時計の進み − 単調時計の進み ≥ 30 秒**（`RESUME_GAP`）なら「スリープから復帰した」とみなし、info を 1 行出して `reset_connections()` + `forget_contact()`（= 同じサイクルで即プローブ）。

- サスペンドは単調時計を止め壁時計を止めない。ループ側の遅さ（2 分のプラグイン RPC）は両方の時計を等しく進めるので差にならない。
- 運用者が時計を進めた場合も引っかかるが、代償は余分なプローブ 1 回と TLS ハンドシェイク 1 回で無害。時計を**戻した**場合は resume ではない（`None`）。
- 今日の消費者は LLM ルータだけ。プラグインは自分のソケットを自分で再接続する設計で、engine から知らせる口はプロトコルに無い。

## 6. 設定キーは足さない・起動は止めない

間隔・閾値は定数（テストは `EngineSettings` の seam で縮める）。[`RestartPolicy`](/components/orchestrator-core.md) と同じ理由 —— 運用者が根拠を持って調整できる値ではなく、キーは「誰も試していない値に設定できる場所」を増やす。起動時プローブが失敗しても `run` は上がる（fail-open）: LLM 無しでも `pending` で動く設計に、起動だけ厳しくする理由が無い。

# Consequences

- `health.json` の `kind` が 5 種になる。`totsuka status` / `totsuka menu` は既存の読み方で新しい行を描く（`message()` は Rust 側が持つ）。旧 totsuka が読むと `unknown` の 1 行になる（設計どおり）。
- `LlmRouter` を実装する外部コードは、既定実装があるので変更不要。
- 沈黙時の課金は 10 分に 1 トークン。障害中は 1 分に 1 回の**失敗する**リクエストで、課金は発生しない。
- 「LLM が死んでいる」が、タスクが 1 つ失敗するのを待たずに、起動直後・復帰直後・沈黙 10 分以内に `⚠` として見える。
- **対象外**: task_source プラグインが自前で呼ぶ LLM（Slack の分類器）。プラグインの health を core へ届けるにはプロトコルに通知メソッドが要り、別の ADR になる。
- **実機で確かめるべきこと**: (a) 実ゲートウェイで起動直後の `llm-online` 相当が `health` に出ないこと（正常系）、(b) `base_url` を存在しないホストに向けて起動し 30 秒以内に `status` が `llm_unreachable` を出すこと、(c) macOS を数分スリープさせ、復帰ログ 1 行と直後のプローブが出ること。

# Alternatives considered

- **固定周期で無条件にプローブする**（例: 毎分）。実トラフィックが答えている問いを二重に訊き、課金と 429 の種になる。沈黙時だけに絞った。
- **起動時に同期でプローブしてから走る**。ゲートウェイが死んでいると起動が `timeout_secs` 分遅れ、しかも fail-closed にする理由が無い。spawn 1 本に統一した。
- **OS のスリープ通知**（macOS の `NSWorkspace` / IOKit）。移植性を落とし依存を増やす。2 つの時計の差で同じことが分かる。
- **`pool_idle_timeout` を短くして復帰直後の半開き接続を避ける**。hyper のプールは単調時計で idle を測るので、スリープ中に時間は進まず効かない。クライアントを作り直すしかない。
- **`[llm].probe_interval_secs` 等の設定キー**。上記 6 のとおり不採用。
- **Slack プラグインのラッチを initialize 応答や通知で core へ届ける**。必要だが今回のスコープ外。プロトコル変更を伴うので単独の ADR にする。
