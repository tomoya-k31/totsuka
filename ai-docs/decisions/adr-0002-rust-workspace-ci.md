---
type: Decision
title: ADR-0002 Rust workspace 構成と CI 品質ゲート
description: totsuka を Rust edition 2024 のヘキサゴナル workspace（core/cli/plugin-protocol）として構成し、clippy deny warnings・rustfmt・cargo-audit/deny・llvm-cov を CI 品質ゲートに据える決定。
tags: [rust, workspace, ci, architecture, quality-gate]
generated: { by: human:tomoya-k31, at: 2026-07-19T12:00:00Z }
status: stable
sources:
  - id: ref-1
    resource: /product/orchestrator-spec.ja.md
    title: "orchestrator 要件定義書 §6 / §9"
  - id: ref-2
    resource: https://github.com/tomoya-k31/totsuka/issues/45
    title: "Issue #45"
---

# Status

Accepted — 2026-07-12（[#45](https://github.com/tomoya-k31/totsuka/issues/45)）

一部変更 — 2026-07-19: CI ジョブの**実行タイミング**（イベントゲート・audit の分離・concurrency）は [ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) で再設計した。品質ゲートの内容（rustfmt / clippy / test / audit / coverage の各基準）は本 ADR のまま。

# Context

[Spec](/product/orchestrator-spec.ja.md) §6 技術要件・§9 品質保証を満たす実装土台が必要だった。以降の全機能タスク（#46〜）がこの上に載るため、ワークスペース構成・toolchain・品質ゲートを最初に確定させる。

# Decision

- **言語 / edition**: Rust edition 2024、stable channel。`rust-toolchain.toml` で明示（`rustfmt` / `clippy` component 同梱）。
- **ワークスペース構成**: `resolver = "3"`。共通依存とバージョンは `[workspace.dependencies]` / `[workspace.package]` で一元管理し、各 crate が継承する。
  - `crates/orchestrator-core` — ドメイン・ステートマシン・ports trait。ヘキサゴナルの `domain` / `ports` / `adapters` をモジュール骨格として先に切る。
  - `crates/orchestrator-cli` — bin `totsuka`（エントリポイント）。
  - `crates/plugin-protocol` — プラグイン開発者向け公開型定義。
  - `plugins/` — 公式プラグイン crate は Phase 4（#58〜）で workspace members に追加。
- **lint 方針**: `[workspace.lints]` で clippy `all` と rust `warnings` を deny し、各 crate は `lints.workspace = true` で継承。warning 1 件で CI fail。
- **CI（`.github/workflows/ci.yml`）**: rustfmt check / clippy `-D warnings` / `cargo test` / cargo-audit / cargo-deny / cargo-llvm-cov のジョブ構成。カバレッジは計測・アーティファクト化のみで閾値ゲートは設けない。
- **依存ポリシー**: この時点の依存は最小限（clap のみ）。tokio / serde / rusqlite 等は各機能タスクで追加する。
- **ライセンス / 脆弱性**: `deny.toml` で許可ライセンスと advisory ポリシーを定義。

# Consequences

- 新規 crate 追加時は workspace members・`[workspace.dependencies]`・`lints.workspace = true` の3点を揃える。
- clippy/fmt を CI が deny するため、ローカルでも `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all --check` を実装完了の条件とする。
- actions はリポジトリ規約に従い commit SHA ピン留め。
