---
type: Decision
title: ADR-0004 Claude Code フック完了シグナルの受信をコア driving adapter に置く
description: Claude Code の完了検知を screen-manifest からフック機構へ移すにあたり、UDS 受信サーバを orchestrator-core の driving adapter（ports::SignalPort + adapters::hook_uds）側に置き、herdr プラグイン内には置かない決定。llm 検収はセッション内 prompt 型 Stop フックで行う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [hook, claude-code, uds, socket, verification, signal, architecture, epic-131]
timestamp: 2026-07-18T12:00:00Z
status: accepted
owner: tomoya-k31
---

# Status

Accepted — 2026-07-18（エピック [#131](https://github.com/tomoya-k31/totsuka/issues/131)、実装 #132〜#141）

# Context

対象エージェント Claude Code は **Lifecycle Authority を持たない**（[agent-ide-herdr](/components/agent-ide-herdr.md) / #124・#130）。herdr の screen-manifest（画面パターン認識）由来の完了判定は遅延・取りこぼし・誤検知が構造的に避けられず、実機一気通貫検収（#123）で「CLI 自身のエラー文を回答として publish する」等の実害が出た。

そこで完了検知を **Claude Code のフック**（`Stop` / `Notification` / `SessionStart` / `SessionEnd` の command 型フック + curl で UDS へ POST）へ全面移行する（要件 [F-100〜F-107](/product/orchestrator-spec.ja.md)）。この移行にあたり 2 つの構造判断が必要だった:

1. **UDS 受信サーバの置き場所**: フックからの POST を受ける HTTP/UDS サーバを、どのプロセスの、どのレイヤに置くか。候補は (a) herdr プラグイン内、(b) orchestrator-core の driving adapter。
2. **llm 検収の実現方法**: 「エージェントの成果が要件を満たすか」を LLM に判定させる検収（D-01）を、セッション内で走らせるか、Orchestrator から別途 re-dispatch するか。

# Decision

## 1. UDS 受信サーバは orchestrator-core の driving adapter に置く

フック POST の受信を、`ports::SignalPort`（Engine 非依存の投入境界）+ `adapters::hook_uds`（`UnixListener` 0600・最小 HTTP/1.1・Bearer 定数時間検証・`AgentSignal` 正規化）として **orchestrator-core 側**に実装する。プラグイン内には置かない。詳細は [orchestrator-core](/components/orchestrator-core.md)、受信契約は [POST /claude-events](/apis/claude-events.md)、正規化型は `domain::signal`。

**なぜプラグイン内配置を却下したか**: フックシグナルの処理は、以下 3 つを **core の状態DB（[state.db](/data/state-db.md)）と突き合わせて**初めて成立する。プラグインローカルの受信サーバはこれらを再起動を跨いで保持できない:

- **`(plugin, session_id) → task_id` の相関**: シグナルは `job_id = job-{task_id}-{session_row}` を起点にタスクへ配路する（E-09。共有セッション id から宛先を推測しない）。この索引の正本は core の `sessions` / `tasks` テーブルであり、プラグインは再起動で揮発する in-memory 状態しか持てない（Slack プラグインでバッファ・pending index が揮発するのと同型の制約）。
- **冪等キー**: 多重発火・スプール再送・curl リトライの無害化は `hook_events UNIQUE(job_id, claude_session_id, prompt_id, event)`（D-05）に依存する。冪等の真実は DB にあり、プラグインローカルでは担保できない。
- **監査ログ**: 受信 JSON 全文の監査（N-01）と、UNKNOWN 連続数の DB 再計算（D-02。フック自己申告は不使用）は core のイベント永続化と一体。

さらに Orchestrator は複数の agent_ide プラグイン（herdr / orca / 将来）を扱うが、フック完了判定は Claude Code に固有で agent 非依存の**横断的関心事**である。受信を特定プラグインに埋めると、他 agent や再起動回復（§5.3）から再利用できない。`SignalPort` を Engine から独立させ、`hook_uds` を driving adapter として隔離することで、UDS サーバ・スプール回収（`replay_spool`）・タイムアウト掃引（`sweep_signal_timeouts`）がすべて同じ DB 真実の上に乗る。

トレードオフ: フック env 注入（`TOTSUKA_JOB_ID` 等）と `--settings` 起動はプラグイン（[agent-ide-herdr](/components/agent-ide-herdr.md)）の責務として残る。受信（core）と起動（plugin）が分かれるが、プラグインは値を**不透明に配線するだけ**（生成・解釈は core）とすることで境界を単純に保つ。

## 2. llm 検収はセッション内 prompt 型 Stop フックで行う（別途 re-dispatch しない）

`verification = "llm"` の検収は、workflow 別 `orchestrator-<workflow>.json` に **prompt 型の `Stop` フック**（rubric + マーカー規約）を追加してセッション内で走らせる。Orchestrator から別セッションへ成果物を渡して re-dispatch する方式は採らない。

**根拠**:

- prompt 型 Stop フックがセッションの文脈（会話履歴・worktree の実ファイル）をそのまま参照できるため、成果物を別セッションへ再供給する必要がなく、相関・コスト・レイテンシが最小。
- 追加の session/worktree ライフサイクルを増やさない（1 task = 1 session の正規化を崩さない）。
- **Claude Code 2.1.212 で prompt 型 Stop フックがセッション内で発火し rubric を適用できることを実機確認済み**。これが前提の成否を決めるため、バージョン依存として明記する。
- `COMPLETED` 受信で Engine は Publishing へ直行する。`human` 検収は `Verifying` で `totsuka task verify --pass/--fail` を待ち（[orchestrator-cli](/components/orchestrator-cli.md)）、`none` は直接 publish する。

# Consequences

- 受信は `[hooks]` 未設定でも既定パス（`${XDG_RUNTIME_DIR}/totsuka/claude-events.sock`）で常時起動する。socket 0600 が第一の認証層、Bearer（keychain 参照）が第二層で、いずれも core が握る（[hook-security](/security/hook-security.md)）。
- フック POST は at-least-once（失敗時スプール）であり、冪等 UNIQUE 制約で重複を吸収する。冪等の正本が DB にあるため、スプール再投入（`replay_spool`）や curl リトライは無害に再送できる（[hook-troubleshooting](/operations/hook-troubleshooting.md)）。
- 旧 Orchestrator（0.1.3 未満）+ 新プラグインの組合せは `^0.1` 互換上は成立するが、env・`--settings` が付かず**完了を検知しなくなる**ため、プラグインは `protocol_version` 0.1.3 未満で警告ログを出す。
- 完了検知のフック移行に伴い、herdr の状態ストリームは `pane.exited` デッドマン専用へ縮退した（F-106）。エンドツーエンドの流れは [フックシグナルフロー](/architecture/hook-signal-flow.md) を参照。
- llm 検収の実現が Claude Code のフック仕様（prompt 型 Stop の挙動）に依存するため、Claude Code 側の仕様変化は保守タスク（§10.3）として監視対象に加える。

# Citations

[1] [Issue #131 エピック（Claude Code フック完了判定）](https://github.com/tomoya-k31/totsuka/issues/131)
[2] [F-100〜F-107 決定的な完了シグナル](/product/orchestrator-spec.ja.md)
[3] [POST /claude-events（UDS フック受信）](/apis/claude-events.md)
[4] [フックシグナルフロー図](/architecture/hook-signal-flow.md)
