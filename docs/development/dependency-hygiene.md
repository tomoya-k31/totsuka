---
type: Guide
title: 依存関係ハイジーン（未使用依存の検出）
description: cargo-machete による毎 PR の未使用依存チェックの運用、誤検知の抑制手順（package.metadata.cargo-machete）、および高精度な cargo-shear / cargo-udeps の定期手動実行手順。
resource: https://github.com/tomoya-k31/totsuka/blob/main/.github/workflows/ci.yml
tags: [rust, ci, dependencies, cargo-machete, cargo-shear, cargo-udeps]
timestamp: 2026-07-25T00:00:00Z
status: active
owner: tomoya-k31
---

# 背景（#171）

ワークスペースは `[workspace.dependencies]` に依存を集約し、各クレートが必要な feature だけを厳選する運用だが、リファクタで参照が消えた依存は静かに残り続け、ビルド時間・監査対象（cargo audit / deny）・サプライチェーン面積を無駄に増やす。このドリフトを検知する 2 層のガードを置く。

# 第1層: cargo-machete（毎 PR、CI 常設）

`.github/workflows/ci.yml` の `machete` ジョブが毎 PR で実行する。machete はテキストレベルのスキャンでコンパイル不要（Rust toolchain も不要）なため数秒で終わり、`ubuntu-slim` runner で走る。未使用依存を混入させた PR は CI が fail する。

ローカルでの事前確認:

```bash
cargo install cargo-machete   # 初回のみ
cargo machete                 # ワークスペースルートで実行
```

## 誤検知の抑制手順

machete はテキストスキャンゆえ、マクロ経由の使用・再エクスポート・`#[doc]` 参照などで**誤検知（false positive）**が出うる。誤検知と判断した場合は、該当クレートの `Cargo.toml` に **理由コメント付きで** ignore を追加する:

```toml
[package.metadata.cargo-machete]
# serde_derive はマクロ展開でのみ参照されるため machete が検知できない
ignored = ["serde_derive"]
```

理由コメントのない ignore を追加しない（レビューで「本当に使っているのか」を判断できなくなるため）。依存を削除した際に ignore が残っていないかも合わせて確認する。

# 第2層: cargo-shear / cargo-udeps（定期の手動実行）

machete より精度の高いツールを、四半期に 1 回程度（または大きなリファクタ・依存整理の後）に手動で実行し、machete がすり抜けた未使用依存・未使用 feature を拾う。

## cargo-shear（推奨: stable で動く）

```bash
cargo install cargo-shear   # 初回のみ
cargo shear                 # ワークスペースルートで実行
```

- マクロ展開を考慮したより精度の高い検出を行う。誤検知の抑制は machete と同様に `[package.metadata.cargo-shear] ignored = [...]`（理由コメント付き）。
- 検出された未使用依存は通常 PR（`chore(deps): ...`）として削除する。

## cargo-udeps（代替: nightly が必要）

```bash
cargo install cargo-udeps                 # 初回のみ
cargo +nightly udeps --workspace --all-targets --all-features
```

- 実際にコンパイルして判定するため最も精度が高いが、nightly toolchain が必要でビルド時間もかかる。shear で不十分な場合の精査用。

## 手動実行の記録

手動実行で削除・ignore 追加を行った PR には、実行したツールとバージョンを PR 本文に記録する（次回実行時の基準点になる）。

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- CI 定義: `.github/workflows/ci.yml`（`machete` ジョブ）、`.github/workflows/audit.yml`（cargo-audit / cargo-deny）
