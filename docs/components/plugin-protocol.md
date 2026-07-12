---
type: Component
title: plugin-protocol クレート
description: プラグイン開発者向けに公開する型定義クレート。JSON-RPC 2.0 エンベロープ・plugin.toml マニフェスト・capabilities を Orchestrator とプラグインの安定契約として提供する。#45 時点は空の雛形。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-protocol
tags: [rust, crate, plugin, protocol, json-rpc]
timestamp: 2026-07-12T00:00:00Z
status: active
owner: tomoya-k31
---

# 責務

Orchestrator と別プロセスのプラグイン（`task_source` / `agent_ide` / `notifier`）が JSON-RPC 2.0 over stdio でやり取りするための公開型定義。プラグイン開発者はこのクレートに依存して実装する。

# 公開インターフェース

- JSON-RPC 2.0 リクエスト / レスポンス / notification エンベロープ
- `plugin.toml` マニフェスト型（名称・種別・バージョン・対応プロトコルバージョン・capabilities）
- capability 宣言と最小メソッドセット（`initialize` / `shutdown` / `config/validate` / `tasks/fetch` / `task/dispatch` / `session/attach` / `state/subscribe` / `notify` 等、Spec §11）

#45 時点は空の雛形。具体的な型定義は #50 で実装する。

# 依存

- なし（#50 で serde 等を追加）。

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [Spec §4.6 プラグインシステム / §11 プロトコル最小メソッドセット](/product/orchestrator-spec.ja.md)
