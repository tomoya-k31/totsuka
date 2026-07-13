---
type: Known Issue
title: 既知の不具合・制約パターン
description: テスト・運用で判明した既知の制約（LLM VCR 未対応、recovery 時の成果物欠落、worktree テストのフレーク要因など）と回避策。
resource: https://github.com/tomoya-k31/totsuka
tags: [known-issue, testing, recovery, llm, flake]
timestamp: 2026-07-14T02:00:00Z
status: active
owner: tomoya-k31
---

# 既知の制約

## LLM 呼び出しの HTTP VCR 再生は未実装

現状、LLM 判定のスタブ化はユニットの `MockRouter`（`repo_select`）と、E2E での単一リポジトリ経路（LLM 不要）で行う。録画済み HTTP レスポンスの再生（VCR 方式で多リポジトリ選択を E2E 再現）は未実装。多リポジトリ選択のロジック自体はユニットで網羅済み。

**回避策**: E2E は単一リポジトリ構成で実行する。多リポジトリ選択は `repo_select` のユニットテストで確認する。

## down 中に完了したタスクの成果物は回復不能

Orchestrator が完全停止している間にエージェントが完了した場合、`state/notification` の `log_chunk`（`output = source` の成果物）はどのプロセスにも捕捉されず、再起動時の `session/attach` でもプラグインは終端状態や過去ログを再送しない。finalize *途中*のクラッシュは `BeginPublish` イベントに永続化した成果物から復元されるが、実行中の完全停止は復元できない。

**挙動**: 復元不能時は `result/publish` に「成果物未捕捉（回復実行）」の正直な注記を publish する（誤った placeholder は出さない）。

## worktree テストのフレーク要因（解消済み）

`worktree.rs` / `run_loop.rs` の実 git テストは、ローカルの 1Password コミット署名エージェントに seed commit がブロックされてフレークしていた。テストヘルパで `commit.gpgsign=false` / `tag.gpgsign=false` を注入して解消済み。新規に実 git を叩くテストを足す場合も同様に署名を無効化すること。

## E2E のモックバイナリ鮮度

E2E（`orchestrator-cli/tests/e2e.rs`）は `orchestrator-core` の `mock_plugin` バイナリを同一 target から参照する。`cargo test --workspace`（CI）は全 bin を先にビルドするため常に最新だが、`mock_plugin` を編集した直後にローカルで `cargo test -p orchestrator-cli` のみ実行するとバイナリが古いままになりうる。編集時は `cargo build -p orchestrator-core --bin mock_plugin` を先に走らせるか `--workspace` で実行する。
