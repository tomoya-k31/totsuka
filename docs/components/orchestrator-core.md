---
type: Component
title: orchestrator-core クレート
description: totsuka のコア。ヘキサゴナルアーキテクチャの domain（ドメイン・ステートマシン）/ ports（TaskSource・AgentIde・LlmRouter・SecretStore 等の trait）/ adapters（JSON-RPC ブリッジ・SQLite・Keychain）を担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [rust, crate, core, hexagonal]
timestamp: 2026-07-12T00:00:00Z
status: active
owner: tomoya-k31
---

# 責務

totsuka のビジネスロジックの中核。外部 I/O を持たず、ports の trait 境界を介してのみ外界とやり取りする（ヘキサゴナル）。

# モジュール構成

| モジュール | 責務 | 実装タスク |
|---|---|---|
| `domain` | 純粋なドメイン型とタスクステートマシン（`queued → dispatched → running → publishing → done/failed/cancelled`） | #48 / #54 ほか |
| `ports` | 差し替え対象の trait 境界（`TaskSource` / `AgentIde` / `LlmRouter` / `SecretStore` / 永続化） | 各機能タスク |
| `adapters` | ports の具象実装（JSON-RPC プラグインブリッジ・SQLite・Keychain 等） | 各機能タスク |

#45 時点ではモジュール骨格のみ。各実装は後続タスクで追加する。

# 依存

- 現時点で外部依存なし（機能タスクで tokio / serde / rusqlite 等を追加）。
- 参照元: [orchestrator-cli](/components/orchestrator-cli.md)。

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [Spec §6 技術要件](/product/orchestrator-spec.ja.md)
