---
type: Playbook
title: リリース前手動チェックリスト（実機結合）
description: CI で自動化できない実機（herdr / orca）・設計プレビュー・通知・waiting_input 応答の目視確認手順。リリース前に実施する。
resource: https://github.com/tomoya-k31/totsuka
tags: [release, manual-test, checklist, herdr, orca, e2e]
generated: { by: claude-code/opus-5, at: 2026-09-17T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 使い方

CI（[テスト戦略](/quality/test-strategy.md)）は実 mock プラグインで全経路を通すが、**実機エージェント・GUI 通知・人間の応答**は自動化対象外（§9、herdr/orca を CI に組み込まない）。リリースタグ前にこのチェックリストを実施し、結果を PR / リリースノートに残す。

# 前提

- [ ] `totsuka setup` 済み、`config.toml` に実リポジトリ・実プラグインを設定
- [ ] `totsuka config validate` が緑（オンライン、プラグイン疎通含む）
- [ ] `totsuka doctor` が全項目 ok（git / DB / プラグイン / LLM キー参照 / 孤児 worktree）
- [ ] `totsuka doctor --online` の `llm-online` が ok（#267。`llm` チェックは参照の**解決可否**しか見ないため、失効した鍵はここでしか出ない。生体認証プロンプトが出うるので手元で実行する）

# task_source（GitHub / Notion）

- [ ] `totsuka run --dry-run` で実タスクが正しいワークフロー・リポジトリ・エージェントにマッチする
- [ ] `run` 後、対象 Issue / Notion ページのステータスが `on_success` の値に更新される（F-84）
- [ ] Slack の返信案を承認すると、本人名義でスレッドに投稿される（F-07 の書き戻し。**github / notion は publish しない** —— 成果物はエージェントが `gh` / Notion MCP で自分で書くので、Issue コメントの有無で判定しない）

# agent_ide（herdr）

- [ ] dispatch でエージェントが worktree 上で起動し、`running` が `status` に反映される
- [ ] **設計プレビュー**（plan モード）がサイドペイン / 画面に表示される（F-34）
- [ ] エージェントが人間に質問すると `waiting_input` になり、通知が届く（F-35）→ 応答後に `run` で再開する（F-44）
- [ ] worktree が detached HEAD で作られ、エージェントがリポジトリの規約に沿ったブランチを切る。完了後 `totsuka task show` にそのブランチ名が出る（F-86、[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）

# agent_ide（orca）

前提: 対象リポジトリを `orca repo add --path <repository>` で登録しておく（[ADR-0081](/decisions/adr-0081-orca-herdr-parity.md)）。

- [ ] dispatch で Orchestrator の worktree に orca の端末タブが開き、タスク本文が 1 ターンとして Claude に届く（`orca worktree create` による 2 本目の worktree が**できない**こと）
- [ ] タブタイトルが `totsuka <task_id>` になり、Claude のターン後も保たれる。`totsuka doctor` の pane チェックがそのタブを所有 pane として数える
- [ ] 完了が hook で報告され（`TOTSUKA_JOB_ID` が端末の環境に入っている）、`done` → 結果の公開まで進む
- [ ] 通知クリック（`totsuka focus <task>`）で orca がそのタブへ切り替わる
- [ ] エージェントを手で終了させると `failed` になる（deadman）。worktree 掃除でタブが閉じる（`session/release`）

# notifier（macOS）

- [ ] `waiting_input` / `done` / `failed` / `pending` の各イベントで通知センターに通知が出る（F-90）
- [ ] ワークフロー × イベント種別のフィルタ設定が効く（F-92）
- [ ] 通知配送が失敗してもタスク実行は継続する（F-93）

# 信頼性・回復

- [ ] `run --watch` 中に SIGINT で graceful 停止し、ロックが解放される（F-74）
- [ ] 実行中に強制終了（SIGKILL）→ 再起動で `session/attach` により再接続、再接続不能なら継続確認待ち（§5.3）
- [ ] `task retry` が worktree / セッションを再利用して再開する（F-44）

# 掃除

- [ ] 完了タスクの worktree が掃除ポリシー（immediate / retention_days / manual）どおりに処理される（F-23/85）
- [ ] `totsuka doctor` が孤児 worktree を検出し、対話的に掃除できる（F-24）
