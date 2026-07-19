---
type: Decision
title: ADR-0007 CI 実行タイミングの再設計（Actions コスト最適化）
description: GitHub Actions の無料枠超過を受け、品質ゲートの内容は変えずに実行タイミングを再設計する決定。PR は clippy+rustfmt / test、main への push は coverage(llvm-cov) のみ、audit は日次 cron + 依存ファイル変更 PR に移し、全ワークフローへ concurrency を導入する。
tags: [ci, cost, quality-gate, github-actions]
timestamp: 2026-07-19T12:00:00Z
status: accepted
---

# Status

Accepted — 2026-07-19（[ADR-0002](/decisions/adr-0002-rust-workspace-ci.md) の CI ジョブ構成を一部変更する）

# Context

private リポジトリのため Actions は Pro プランの無料枠 3,000 分/月で運用しているが、直近 30 日の推計使用量は約 3,500 分に達し上限を超過した。実測の内訳から、コストの主因はジョブの実行時間そのものではなく次の3点だった。

1. **課金は 1 ジョブごとに 1 分未満切り上げ**のため、実働 0.3〜0.5 分の rustfmt / audit ジョブが毎回 1 分課金される（実働の 3〜6 倍）
2. **coverage(llvm-cov) が毎 PR 実行**（推計 約1,000 分/月、全体の 3 割）だが、結果はアーティファクト化のみで閾値ゲートなし（[#45](https://github.com/tomoya-k31/totsuka/issues/45) の決定どおり）であり、PR ごとに走らせる価値がない
3. **main への push（マージ）ごとに全 5 ジョブを再実行**（110 回/月 × 約 8 分）。PR で直前に通した内容とほぼ同一

なお main の ruleset が必須にしているステータスチェックは okf-lint の `lint` のみで、ci.yml 側のジョブは必須チェックではないため、イベントで実行を絞っても PR が "Expected" のままブロックされる問題は起きない。

# Decision

チェックの**内容**（rustfmt / clippy / test / audit / coverage の各基準）は ADR-0002 から変えず、**実行タイミング**のみ変更する。

- **PR（pull_request）**: `clippy / rustfmt`（1 ジョブに統合）と `test` を実行。rustfmt の独立ジョブは切り上げ課金の固定費でしかないため clippy ジョブへ吸収する。
- **main への push**: `coverage (llvm-cov)` のみ実行。llvm-cov は計装ビルドで全テストスイートを実行するため、マージごとのテスト検証を兼ねる（「main が壊れたら revert」フローの検知網は維持）。通常ビルドと計装ビルドで rust-cache が奪い合いにならないよう、test ジョブへは統合せず別ジョブのままイベントで振り分ける。
- **audit（cargo-audit / cargo-deny）**: `audit.yml` に分離し、日次 cron + `**/Cargo.toml` / `Cargo.lock` / `deny.toml` を触る PR + 手動起動で実行。advisory は時間経過で増えるものであり、毎 PR 実行より日次実行のほうが検知も早い。
- **concurrency**: 全ワークフローに `group: workflow-ref` を導入。ci / okf-lint / audit は `cancel-in-progress: true`（同一 PR への連続 push で古い実行を打ち切り）、release-please のみ `false`（リリース途中の打ち切りはタグと成果物の不整合を招くため直列化）。

推計効果: 約 3,500 分/月 → 約 1,900 分/月（無料枠内）。

# Consequences

- PR 上ではカバレッジが見えなくなる（main へのマージ後に artifact で確認）。閾値ゲート導入（#45 で見送り）を再検討する場合はこの決定も見直す。
- audit は PR 必須ではなくなるため、依存を触らない PR に古い advisory が混ざる余地があるが、日次 cron が翌朝までに検知する。
- ci.yml のジョブを増やす際は「1 分切り上げ × ジョブ数 × 実行回数」が固定費になることを考慮し、既存ジョブへのステップ追加を優先する。
- ローカルの実装完了条件（`cargo fmt --all --check` / `cargo clippy --workspace --all-targets --all-features -- -D warnings` / `cargo test --workspace --all-features`）は変更なし（ADR-0002 のまま）。

# Citations

[1] [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
[2] [テスト戦略](/quality/test-strategy.md)
[3] [Issue #45](https://github.com/tomoya-k31/totsuka/issues/45)
