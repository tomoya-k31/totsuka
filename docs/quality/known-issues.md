---
type: Known Issue
title: 既知の不具合・制約パターン
description: テスト・運用で判明した既知の制約（LLM VCR 未対応、recovery 時の成果物欠落、worktree テストのフレーク要因など）と回避策。
resource: https://github.com/tomoya-k31/totsuka
tags: [known-issue, testing, recovery, llm, flake]
generated: { by: human:tomoya-k31, at: 2026-07-14T02:00:00Z }
status: stable
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

`worktree.rs` / `run_loop.rs` の実 git テストは、ローカルの 1Password コミット署名エージェントに seed commit がブロックされてフレークしていた。共有ヘルパ [`test-support`](https://github.com/tomoya-k31/totsuka/tree/main/crates/test-support) が `commit.gpgsign=false` / `tag.gpgsign=false` を注入して解消済み。新規に実 git を叩くテストは `test_support::git` を使うこと（署名無効化が組み込まれている）。

## 共有テストヘルパ

実 git リポジトリ・scratch ディレクトリの生成は `test-support` クレートに集約済み（`git` / `scratch` / `bare_origin_and_clone`）。worktree・run-loop・CLI E2E の各テストはこれを dev-dependency として共有する。E2E の `mock_plugin` バイナリは毎回 `cargo build`（依存追跡により最新時は no-op、テストと同一プロファイル）で用意するため、編集後の鮮度ずれは起きない。
