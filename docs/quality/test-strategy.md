---
type: Test Strategy
title: テスト戦略（自動結合テスト / E2E / モックプラグイン）
description: totsuka のテスト層（ユニット・実プロセス結合・バイナリE2E）とモックプラグインによるシナリオ注入、フレーク対策、CI 品質ゲートの定義。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates
tags: [testing, e2e, integration, mock, ci, quality, slack]
timestamp: 2026-07-20T18:00:00Z
status: active
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
| `commit_on_dispatch` | dispatch 時に worktree へ実コミット（pull_request 出力の検証 #65） |
| `crash_on_dispatch` | dispatch 中に自殺（クラッシュ隔離 §5.3） |
| `no_state_stream` | `state_stream` capability を落とす（非対応エージェント拒否 #63） |
| `notify_log` | 受信した `notify` / `task/update_status` / `result/publish` を JSON Lines で記録（テスト側でアサート） |

# 自動化済みの異常系

- **プラグインクラッシュ**: `crash_on_dispatch` → タスク failed、Orchestrator は生存（`e2e_agent_crash_fails_task_and_orchestrator_survives`、`plugin_host.rs`）。
- **attach 失敗 / セッション消失**: `session_id` に `gone` → 継続確認待ち（自動 failed にしない、§5.3）（`run_loop.rs`, `session_recovery.rs`）。
- **waiting_input**: ワンショットが待機タスクを残して終了（`e2e_waiting_input_leaves_task_and_status_shows_it`, `run_loop.rs`）。
- **down 中に完了 / finalize 途中クラッシュ**: 回復時 finalize と成果物復元（`run_loop.rs`）。
- **出力ポリシー失敗**: ゼロコミット PR、PR 作成失敗→retry 再開、ワークフロー消失時の worktree 保持（`run_loop.rs`）。

# フレーク対策

- E2E は原則 **ワンショット**（`--watch` を使わずタイミング非依存）。各バイナリ実行に実時計ガードを付け、ハング時は即失敗させる。例外は Slack E2E: 承認ボタンはタスク完了 *後* に届くため `run --watch` 常駐が必須で、代わりに「観測条件をポーリング + 段階別タイムアウト + 最後に kill」で決定性を担保する。
- Slack E2E のモック Slack は in-process（ポート 0 バインド）で外部ネットワークに依存しない。Web API はパス別のスティッキー応答 + 全呼び出し記録（フォームデコード済み）で、アサーションは記録に対して行う。
- 実プロセステストは実 git の bare origin をローカル tempdir に作り、外部ネットワークに依存しない。
- LLM 呼び出しは、単一リポジトリ経路（LLM 不要）と `MockRouter`（ユニット）でスタブ化。HTTP レベルの VCR 再生は将来対応（[Known Issue](/quality/known-issues.md) 参照）。
- git のコミット署名はテストヘルパで無効化（ローカル署名エージェントによるブロック回避）。

# CI 品質ゲート

チェック内容は #45 のまま、実行タイミングはコスト最適化のため [ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) で再設計した。

- **毎 PR**（`ci.yml`）: `clippy / rustfmt`（1 ジョブ）と `test`（全層）。`okf-lint.yml`（`lint` ジョブ、唯一の必須チェック）は全 PR で OKF lint を実行する。
- **main への push**（`ci.yml`）: `coverage (llvm-cov)` のみ。計装ビルドで全テストスイートを実行するため、マージごとのテスト検証を兼ねる（カバレッジはアーティファクト化のみ、閾値ゲートなし）。
- **日次 cron + 依存ファイル変更 PR**（`audit.yml`）: `cargo-audit` / `cargo-deny`。

PR で報告されるチェックが全て緑になるまでマージしない。

# 手動チェック

実機（herdr / orca）・設計プレビュー・通知センターの目視確認は自動化対象外。[リリース前チェックリスト](/quality/release-checklist.md) を参照。
