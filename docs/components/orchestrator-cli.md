---
type: Component
title: orchestrator-cli クレート
description: totsuka の CLI エントリポイント（bin: totsuka）。init / run / status / task / plugin / config / doctor / logs / completion サブコマンドを提供する（§5.1）。#45 時点では --version/--help のみ。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [rust, crate, cli]
timestamp: 2026-07-12T00:00:00Z
status: active
owner: tomoya-k31
---

# 責務

ユーザー向けの CLI 表面。`clap` でコマンドを解釈し、[orchestrator-core](/components/orchestrator-core.md) のユースケースを呼び出す。

# 公開インターフェース

- bin 名: `totsuka`
- #45 時点: `--version` / `--help` のみ動作する最小実装。
- サブコマンド体系（`run` / `status` / `task` / `plugin` / `config` / `doctor` / `logs` / `completion` と共通フラグ `--debug` / `--json` / `--dry-run` / `--config`）は #63 / #64 で実装する。

# 依存

- `clap`（derive）
- [orchestrator-core](/components/orchestrator-core.md)

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [Spec §5.1 起動・CLI](/product/orchestrator-spec.ja.md)
