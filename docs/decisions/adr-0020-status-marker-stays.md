---
type: Decision
title: ADR-0020 ステータスマーカーは wire 信号として存置する（エンジン側 LLM 検収への移行は不採用）
description: ステータスマーカー（<<STATUS:COMPLETED>> 等、F-101）を廃止してエンジン側 LLM 検収へ移す案（Option A、#159）を評価し、現時点では不採用として現状維持する決定。マーカーはマルチツール化（#196）以降ツール非依存の唯一の完了信号になっており、廃止は 3 ツールのアダプタ・state.db の冪等キー・エスカレーション計数・publish 成果物の全経路に波及する。廃止する場合の改修一式と、その際に必要な前提条件も記録する。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/domain/signal.rs
tags: [hook, marker, verification, llm, completion, claude-code, codex, opencode, epic-131, tool-abstraction]
generated: { by: human:tomoya-k31, at: 2026-07-28T16:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: ref-1
    resource: https://github.com/tomoya-k31/totsuka/issues/159
    title: "Issue #159 マーカー廃止に必要な改修一式の洗い出し"
  - id: ref-2
    resource: /decisions/adr-0004-hook-completion-signal.md
    title: "ADR-0004 フック完了シグナルの受信配置"
  - id: ref-3
    resource: /decisions/adr-0014-tool-abstraction.md
    title: "ADR-0014 AI ツール抽象 / ADR-0015 タスク同一性を 1 会話へ"
  - id: ref-4
    resource: /product/orchestrator-spec.md
    title: "F-100〜F-107 決定的な完了シグナル"
  - id: ref-5
    resource: /architecture/hook-signal-flow.md
    title: "フックシグナルフロー / POST /agent-events"
  - id: ref-6
    resource: https://github.com/tomoya-k31/totsuka/pull/152
    title: "PR #152 マーカー単一カッコ許容 / PR #154 冪等キーへ status 追加 / PR #158 マーカー指示の事前注入"
---

# Status

Accepted — 2026-07-28（[#159](https://github.com/tomoya-k31/totsuka/issues/159) の設計検討。実装は伴わない）

[ADR-0004](/decisions/adr-0004-hook-completion-signal.md) の「llm 検収はセッション内 prompt 型 Stop フックで行う」判断を **supersede しない**。本 ADR はそこで不採用にした「エンジン側で検収する」案を再評価し、改めて不採用とした記録である。

# Context

ステータスマーカー（`<<STATUS:COMPLETED>>` / `<<STATUS:NEEDS_INPUT reason="...">>` /
`<<STATUS:FAILED reason="...">>`、F-101）は、ペイン上の AI ツールから orchestrator へ
「これが最終回答だ、publish してよい」を運ぶ**唯一の決定的な完了信号**である
（エピック [#131](https://github.com/tomoya-k31/totsuka/issues/131) / [ADR-0004](/decisions/adr-0004-hook-completion-signal.md)）。

マーカーは応答の最終行としてペインに見える。「LLM が完了を判断してそのまま送信する」方が
自然ではないか、という問いが繰り返し出るため、**廃止した場合に現実的に必要となる改修の一式**を
洗い出し、判断材料を残す（#159）。

## この問いが構造的に難しい理由

1. **フック同士は独立** — prompt 型フック（LLM 検証）の合否を command フック（`on-stop.sh`）が
   読む経路が無い。LLM の判定を送信トリガに使うには、判定結果を運ぶ別の信号が結局必要になる
2. **マーカー無しの Stop は曖昧** — 「途中の停止（承認待ち・考え中の idle）」と「最終回答」を
   区別できない。この曖昧さの排除こそが #131 の目的だった
3. **ツール呼び出しによる通知は `mode = plan` と衝突** — 完了時にエージェントがコマンドを実行して
   UDS へ POST する案は、読み取り専用モードで成立しない
4. **エンジン側 LLM 検収なら消せるが**、[ADR-0004](/decisions/adr-0004-hook-completion-signal.md) が
   不採用にした「不合格時のフィードバックをペインへ打ち込む」脆い経路が復活する

## #159 起票（2026-07-18）以降に変わった前提

洗い出しを現行コードに突き合わせた結果、**起票時の前提が 2 点変わっていた**。どちらも
「廃止しにくくなる」方向である。

### 1. マーカーはツール非依存の唯一の完了信号になった（[#196](https://github.com/tomoya-k31/totsuka/issues/196) / [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）

# 159 は Claude Code 1 ツールを前提に書かれているが、現在は 3 ツールが同じマーカー規約を共有する。

**フック機構は 3 者でまったく異なるのに、マーカーだけが共通**である:

| ツール | 完了検知の実装 | マーカー解析箇所 |
|---|---|---|
| Claude Code | 起動時 `--settings orchestrator-<workflow>.json`（command 型 + prompt 型） | `hooks/on-stop.sh` |
| Codex | ユーザーグローバルな `$CODEX_HOME/hooks.json`。**command 型のみ**（prompt 型フックが無い） | `hooks/on-stop.sh`（同じスクリプトを共有） |
| OpenCode | グローバル JS プラグイン `plugins/totsuka-opencode.js` | 同 JS 内の正規表現 |

マーカーは**アシスタントのテキストの中にある**ため、フック API の差異を越えて 1 つの規約で済んでいる。
これは偶然ではなく、この規約が生き残っている理由そのものである。廃止するなら
「ツールごとに別の完了信号を作る」ことになり、コストは #159 の見積もりの 3 倍側に振れる。

## 2. `verification = "llm"` は実質 Claude 専用で、しかも縮退が実装されていない

`ToolCapabilities.prompt_verification` は Claude だけ `true`、Codex / OpenCode は `false`
（`tool/mod.rs`）。`config/validate.rs` は非 claude ツールを pin した `verification = "llm"` に対し
**「dispatch で human へ縮退する」と警告している**が、この capability を読む箇所は
コードベースに存在せず、`run::hooks` は `Llm` と `None` を同じ腕で扱う。
すなわち実際の挙動は **human ではなく `none` 相当（未検証のまま publish）への黙示的縮退**である。

この不整合は #159 の評価にとって二重の意味を持つ:

- 「セッション内 rubric」という現行の検収方式は、**すでにマルチツールへ一般化できていない**。
  エンジン側検収（Option A）はこの点だけは改善する
- ただしそれは**マーカー廃止とは独立に**（マーカーを残したまま）実施できる。
  検収の主体をどこに置くかと、完了信号を何で運ぶかは別の軸である

バグ自体は本 ADR の範囲外として別 issue に切り出す（→ Consequences）。

### 3. 数字の更新

state.db は #159 起票時の v3 から **v7** まで進んだ（v4 = `tool_session_id` へのリネーム、
v5 = `task_messages` 新設、v6 = v5 以前のタスクへの台帳バックフィル、v7 = `tasks.thread_key` DROP。
`schema_migrations.applied_by` はスキーマ版数を上げない bootstrap 側の追加なので、この数には入らない）。

# 159 が「v4」と書いている冪等キーの再設計は、実際には **v8 相当のテーブル再構築**になる。

また [#242](https://github.com/tomoya-k31/totsuka/issues/242) / [ADR-0015](/decisions/adr-0015-conversation-task-identity.md) で
dispatch がメッセージ駆動になり、終端が可逆（`Reopen`）になったため、
「判定待ち状態を足す」設計は会話の再オープンとも整合を取る必要がある。

# Decision

**ステータスマーカーを wire 信号として存置する。** エンジン側 LLM 検収（Option A）への移行は
現時点では実施しない。

根拠:

- マーカーは 3 ツールを跨ぐ**唯一の共通完了信号**であり、決定的（正規表現で読める）・ゼロコスト
  （LLM 呼び出しを伴わない）・`verification` の設定値と直交している
- 廃止で得られるのは「ペイン末尾 1 行の消滅」だけである。[PR #158](https://github.com/tomoya-k31/totsuka/pull/158)
  のマーカー指示の事前注入以降、画面上の露出は末尾 1 行のみで、publish 時には除去される（R-07/R-11）
- 引き換えに、全 Stop に LLM 呼び出しコストと可用性の単一障害点が乗り、
  [ADR-0004](/decisions/adr-0004-hook-completion-signal.md) が避けた「ペインへの打ち込み」経路が
  ランタイムに復活する

# 検討した選択肢

## Option A: エンジン側 LLM 検収（マーカーを消せる唯一の筋）

完了判定の主体を「エージェントの自己申告」から「エンジン側の LLM 判定」へ移す再設計。
= [ADR-0004](/decisions/adr-0004-hook-completion-signal.md) が不採用にした案の採用。
必要な改修一式は以下。**この表が #159 の成果物本体である。**

| # | 対象 | 改修内容 |
|---|---|---|
| A-1 | `orchestrator-core/src/hooks/`（wire 契約） | `on-stop.sh` からマーカー抽出と block 分岐を削除し、全 Stop を `status` なしで POST（`last_assistant_message` 全文・`stop_hook_active`・`background_tasks` は現行どおり、heartbeat 導出も維持）。`orchestrator-<workflow>.json` の prompt 型フックを廃止。**`totsuka-opencode.js` も同じ改修が要る**（マーカー解析を落として全 Stop 送信へ）。Codex は同じ `on-stop.sh` を共有するので追加改修は不要 |
| A-2 | `run/hooks.rs`（最大の改修） | Stop（非 heartbeat）ごとにエンジンが `ports::LlmRouter::chat_json`（リポジトリ選択と同じ `[llm]` 設定・structured output）で `{verdict, reason}` を取る。**Engine は単一イベントループ**なので `on_signal` 内で await すると全タスクが止まる → `tokio::spawn` + 新しい内部イベント（例 `PluginEvent::VerdictReady`）で非同期化し、判定中の再入（同タスクの次の Stop・重複 POST）を直列化する |
| A-3 | `domain/state.rs` + scheduler + recovery | 「判定待ち」の非終端状態を足すか `Verifying` を流用するかの判断。`RECOVERABLE_STATES` / `resume_plan` / `counts_toward_slot` の整合。#242 の `Reopen`（終端は可逆）とも噛み合わせる |
| A-4 | `state.db`（v8 相当） | 冪等キーは現行 `(job_id, tool_session_id, prompt_id, event, status)`（v3）。`status` が自己申告値として消えると、block → 再完了の 2 連 Stop を区別していた要素が失われる（v3 = [#154](https://github.com/tomoya-k31/totsuka/issues/154) の教訓）。判定結果を後から書く列と、判定前後の重複到着の扱いを再定義する。SQLite は UNIQUE を in-place 変更できないのでテーブル再構築 |
| A-5 | エスカレーション計数 | 「UNKNOWN stop 連続 ≥ 3」（D-02/F-103、`StateDb::unknown_stop_streak` が `hook_events.status` を読む）は消滅する → 「incomplete 判定の連続」等へ置換 |
| A-6 | 不合格時のフィードバック経路（**最大リスク**） | judge が `incomplete` を返したとき続行指示をペインへ届ける経路が要る。plugin-protocol に additive な `task/feedback { session_id, message }` + `Capabilities.feedback`、agent-ide-herdr 側は既存 `submit_prompt`（`agent.send` + 着弾確認 + Enter 再押下の自己修正）の再利用。**[#124](https://github.com/tomoya-k31/totsuka/issues/124) で苦労した「入力受付と反応のずれ」の脆さがランタイム経路に復活する**（現行はこの脆さが dispatch 時 1 回に閉じている）。エージェントが入力待ちでない瞬間（ツール実行中等）の打ち込みは失われる/混入するため、状態確認 + リトライ + 失敗時 Escalate が要る |
| A-7 | NEEDS_INPUT / FAILED の代替 | 質問待ちは judge の分類に依存するか、既存 `Notification` フック（`agent_needs_input` matcher）を一次信号へ昇格。**マーカーの `reason="..."` 相当の構造化情報は LLM 抽出になり決定性が落ちる**。失敗の自己申告（現行 `<<STATUS:FAILED reason>>` は正確）も judge 分類のみになる |
| A-8 | publish 成果物 | 現行は `strip_status_markers(last_assistant_message)` が publish 成果物になる（R-07/R-11）。マーカーが無くなれば strip は不要だが、「どのメッセージを配送するか」の契約（`MARKER_SELF_REPORT_INSTRUCTION` の delivery contract = マーカーを持つメッセージだけが配送される）を judge 判定に置き換える必要がある |
| A-9 | 運用・コスト・信頼性 | judge LLM が可用性とコストの単一障害点になる（全 Stop に 1 回。現行はマーカー読み取り 0 コスト + `verification = llm` のときだけ in-session 検証）。`[llm]` 障害時のポリシー（fail-open で publish / fail-closed で Escalate）の決定が要る |
| A-10 | テストと docs | E2E / `hook_integration` / `hook_e2e` の大部分がマーカー前提。mock_plugin の合成シグナルも status レス化。ADR-0004 の改訂、F-101/F-102 の置換、[フックシグナルフロー](/architecture/hook-signal-flow.md)、[hook-security](/security/hook-security.md)（judge への本文送信 = LLM 送信範囲の拡大、N-05 相当の再評価） |

概算: protocol / herdr / core（状態機械 + 非同期判定 + DB 再構築）/ hooks（3 ツール分）/ CLI /
テスト / docs を横断する **#131 の縮小版エピック級**（サブ issue 4〜6 本）。

**実施するなら満たしておくべき前提**（現時点ではいずれも未達）:

1. A-6 のフィードバック経路が実機で安定して動くこと（#124 の再来を避ける確証）
2. `[llm]` 障害時のポリシーが決まっていること（fail-open は未検証の publish を意味する）
3. judge のプロンプトが 3 ツールの出力形式差に耐えること

## Option B: ツール呼び出しで通知（不採用）

完了時にエージェントが `curl` で UDS へ POST する。`mode = plan`（読み取り専用）の制約と衝突する。
permission 設定で特定コマンドだけ許可する回避は各ツールの設定仕様に依存して壊れやすく、
エージェント契約も変わる。

## Option C: マーカーを画面から隠す（不採用）

アシスタントメッセージがペインに全表示される以上、メッセージ内の信号は隠せない。
ツール以外に不可視の出力チャネルは存在しない。

# Consequences

- マーカーは 3 ツール共通の wire 信号として残る。**新しい AI ツールを足すときも、
  完了検知は「Stop 相当のフック + マーカー解析」の 2 点で足りる**（`ToolCapabilities.marker_block`
  が false のツールでも、block による再要求ができないだけでマーカー自体は読める）
- 本 ADR は「それでも消したくなった時」の見積もりとして参照される。前提条件（上記 3 点）が
  変わったら再評価する
- 本検討で見つかった**別問題**は本 ADR の範囲外として切り出す:
  - [#301](https://github.com/tomoya-k31/totsuka/issues/301): `verification = "llm"` × 非 claude ツールが
    human ではなく `none` 相当へ黙示的に縮退する（`ToolCapabilities.prompt_verification` が
    どこからも読まれていない）。`validate.rs` の警告文と実挙動が食い違っている
    — **解消済み（2026-07-28）**。完了信号の受信時に実効 verification を `human` へ
    縮退させ、警告文どおりの挙動にした（→ [ADR-0014](/decisions/adr-0014-tool-abstraction.md) 決定 5）
  - [#302](https://github.com/tomoya-k31/totsuka/issues/302): [Spec](/product/orchestrator-spec.md) F-104 の
    `hook_events` UNIQUE 記述が 4 列のままで、実装（v3 以降の 5 列 = `status` を含む）と
    ドリフトしている — **解消済み（2026-07-28）**
- [ADR-0004](/decisions/adr-0004-hook-completion-signal.md) の決定 2（セッション内 prompt 型 Stop
  フックでの llm 検収）は有効なまま。ただし**その適用範囲は Claude 系ツールに限られる**ことが
  本検討で明確になった
