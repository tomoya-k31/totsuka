---
type: Test Strategy
title: テスト戦略（自動結合テスト / E2E / モックプラグイン）
description: totsuka のテスト層（ユニット・実プロセス結合・バイナリE2E）とモックプラグインによるシナリオ注入、フレーク対策、CI 品質ゲートの定義。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates
tags: [testing, e2e, integration, mock, ci, quality, slack]
generated: { by: claude-code/opus-5, at: 2026-08-01T10:20:00+09:00 }
status: stable
owner: tomoya-k31
---

# 目的

§9 のテスト戦略を実装として定義する。JSON-RPC のプラグイン境界（F-51）と `totsuka run` の全経路を、**実プロセス**・**実 git**で検証する。

# テスト層

| 層 | 位置 | 内容 |
|---|---|---|
| ユニット | 各モジュール内 `#[cfg(test)]` | 純粋ロジック（ステートマシン、ワークフローマッチング、スロット会計、テンプレート描画、redact 等）。LLM は `MockRouter`（`repo_select`）でスタブ化。 |
| 実プロセス結合 | [orchestrator-core `tests/`](https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core/tests) | `plugin_host.rs`（プラグイン起動・相関・クラッシュ隔離）、`worktree.rs`（実 git・bare origin）、`session_recovery.rs`、`config_e2e.rs`、`run_loop.rs`（`Engine` を実 mock サブプロセス + 実 git で駆動：全経路・再起動回復・出力ポリシー）。 |
| バイナリ E2E | [orchestrator-cli `tests/e2e.rs`](https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/tests/e2e.rs) | 実 `totsuka` バイナリを XDG scratch 環境で起動し、`run`/`status`/`task show` を通す。config ロード・プラグイン起動・ロック・ログまで含めユーザー視点で検証。 |
| Slack E2E | [orchestrator-cli `tests/slack_e2e.rs`](https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/tests/slack_e2e.rs) | 実 `totsuka` + 実 `task-source-slack` バイナリ + mock agent を、in-process の **モック Slack**（Web API = raw TCP HTTP、Socket Mode = WebSocket）に対して駆動。メンション envelope → `task/submit`（push）→ dispatch → `result/publish` → 承認ボタン → user トークンでのスレッド返信 + 両面 finalize、および `doctor` の TokenGuard プローブ（auth.test + apps.connections.open）を検証（#108）。 |

# モックプラグイン（シナリオ注入）

単一バイナリ [`mock_plugin`](https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/bin/mock_plugin.rs)（`orchestrator-core` の bin）が、`initialize` config によって 3 種すべての `kind` を演じる。CARGO による同一 target への配置を利用し、E2E 側は store へインストールして使う。

| config キー | 効果 |
|---|---|
| `task_submit` / `submit_tasks` | `task_submit: true` で push 型 task_source を演じ、`initialize` 応答直後に `submit_tasks` の各エントリを `task/submit` として push（1 タスクの重複投入で orchestrator 側 dedup=`duplicate` ack の検証にも使える） |
| `stream_states` | `state/subscribe` 後に再生する状態列（例 `["running","done"]` / `["running","waiting_input"]`）（agent_ide, F-38） |
| `session_id` | `task/dispatch` が返すセッション ID。`gone`/`done`/`waiting`/`fail` を含めると `session/attach` の応答を制御（回復シナリオ #57） |
| `commit_on_dispatch` | dispatch 時に worktree でブランチを切って実コミット（`branch_on_dispatch` で名前を指定、既定 `feat/mock-agent-work`）。worktree は detached で渡るので、ブランチが先 |
| `crash_on_dispatch` | dispatch 中に自殺（クラッシュ隔離 §5.3） |
| `no_state_stream` | `state_stream` capability を落とす（非対応エージェント拒否 #63） |
| `notify_log` | 受信した `notify` / `task/update_status` / `result/publish` を JSON Lines で記録（テスト側でアサート） |

# 自動化済みの異常系

- **プラグインクラッシュ**: `crash_on_dispatch` → タスク failed、Orchestrator は生存（`e2e_agent_crash_fails_task_and_orchestrator_survives`、`plugin_host.rs`）。
- **attach 失敗 / セッション消失**: `session_id` に `gone` → 継続確認待ち（自動 failed にしない、§5.3）（`run_loop.rs`, `session_recovery.rs`）。
- **waiting_input**: ワンショットが待機タスクを残して終了（`e2e_waiting_input_leaves_task_and_status_shows_it`, `run_loop.rs`）。
- **down 中に完了 / finalize 途中クラッシュ**: 回復時 finalize と成果物復元（`run_loop.rs`）。
- **出力ポリシー失敗**: ゼロコミット PR、PR 作成失敗→retry 再開、ワークフロー消失時の worktree 保持（`run_loop.rs`）。

# テストの待ち時間（[ADR-0018](/decisions/adr-0018-ci-test-time.md)）

テストが実時間を待つ箇所は、定数ではなく**型付きの値**として注入可能にする。既定値は本番値のままで、テストは**待ち時間だけ**を縮め、**回数は本番値のまま維持する**（諦め系テストは回数そのものを検証しているため）。

- `agent-ide-herdr`: `RetryPolicy`（`Server::with_retry_policy` 経由）。`Default` が実機検証値と一致することを unit test で固定している。
- `orchestrator-core`: `EngineSettings.one_shot_grace` / `worktree_sweep_interval`（config 非露出、テストは `Duration::ZERO`）。
- CLI バイナリを起動する E2E は構造体を触れないため、`run --one-shot-grace-ms`（hidden）を使う。0 ではなく 250ms を渡す — この猶予は `task/submit` が新規 spawn したプラグインから非同期に届くことへの備えで、0 にすると負荷の高いランナーでハンドシェイクと競合してフレークする。

テスト実行中に `cargo build` を呼ばない。兄弟クレートのバイナリは `test_support::sibling_bin` で解決し、CI は `TEST_SUPPORT_PREBUILT_BINS=1` でビルド自体を飛ばす（この env が `TOTSUKA_` 接頭辞を避けている理由は ADR-0018 §3）。

# フレーク対策

- E2E は原則 **ワンショット**（`--watch` を使わずタイミング非依存）。各バイナリ実行に実時計ガードを付け、ハング時は即失敗させる。例外は Slack E2E: 承認ボタンはタスク完了 *後* に届くため `run --watch` 常駐が必須で、代わりに「観測条件をポーリング + 段階別タイムアウト + 最後に kill」で決定性を担保する。
- Slack E2E のモック Slack は in-process（ポート 0 バインド）で外部ネットワークに依存しない。Web API はパス別のスティッキー応答 + 全呼び出し記録（フォームデコード済み）で、アサーションは記録に対して行う。
- 実プロセステストは実 git の bare origin をローカル tempdir に作り、外部ネットワークに依存しない。
- LLM 呼び出しは、単一リポジトリ経路（LLM 不要）と `MockRouter`（ユニット）でスタブ化。HTTP レベルの VCR 再生は将来対応（[Known Issue](/quality/known-issues.md) 参照）。
- git のコミット署名はテストヘルパで無効化（ローカル署名エージェントによるブロック回避）。
- **テスト用にバイナリを配置するときは `fs::copy` ではなくハードリンク**（`test_support::place_binary`。CLI の E2E 4 ファイルすべてがこれを使う）。Linux で `ETXTBSY`（`ExecutableFileBusy`）を踏む。原因は自分の書き込みではなく**同一プロセス内で並行する他のテスト**で、`Command::spawn` の fork が `copy` の書き込み fd を継承したまま残ると、その間 `execve` が拒否される。ハードリンクは書き込み用に開かないのでこの窓が存在しない。同一ファイルシステムでないときだけ copy にフォールバックし、有限回リトライしたうえで**尽きたら panic する** — 実行できないバイナリを置いたまま返すと、数行あとの spawn 失敗という分かりにくい形で出るため。

# CI 品質ゲート

チェック内容は #45 のまま、実行タイミングはコスト最適化のため [ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) で再設計し、実行時間そのものは [ADR-0018](/decisions/adr-0018-ci-test-time.md) で削減した。

- **毎 PR**（`ci.yml`）: `clippy / rustfmt`（rustfmt・arch-lint・`cargo-machete` をステップとして含む 1 ジョブ。7 秒の machete を独立ジョブにすると切り上げ課金 1 分が固定費になるため #281 で吸収した）と `test`（全層）。`okf-lint.yml`（`lint` ジョブ、唯一の必須チェック）は全 PR で OKF lint を実行する。
- **週次 cron**（`cache-cleanup.yml`）: クローズ済み PR の Actions キャッシュを回収する。PR ごとに約 350 MB を PR スコープで作り捨てるため放置すると 10 GB 上限に張り付き、main のベースラインまで退避されてビルドが温まらなくなる。
- **main への push**（`ci.yml`）: `coverage (llvm-cov)` のみ。計装ビルドで全テストスイートを実行するため、マージごとのテスト検証を兼ねる（カバレッジはアーティファクト化のみ、閾値ゲートなし）。
- **日次 cron + 依存ファイル変更 PR**（`audit.yml`）: `cargo-audit` / `cargo-deny`。

PR で報告されるチェックが全て緑になるまでマージしない。

# 手動チェック

実機（herdr / orca）・設計プレビュー・通知センターの目視確認は自動化対象外。[リリース前チェックリスト](/quality/release-checklist.md) を参照。
