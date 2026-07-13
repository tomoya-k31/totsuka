---
type: Term
title: Notifier（ノーティファイア）
description: waiting_input / done / failed / pending イベントを人間へ届ける通知プラグイン。配送は fire-and-forget でタスク実行に影響しない（F-93）。
tags: [glossary, plugin]
timestamp: 2026-07-13T04:30:00Z
status: active
owner: tomoya-k31
---

# Notifier（ノーティファイア）

Orchestrator が発するイベント（`waiting_input` / `done` / `failed` / `pending`、F-90）を通知手段（例: [notifier-macos](/components/notifier-macos.md) = macOS 通知センター）へ届ける `notifier` kind のプラグイン。JSON-RPC notification として受信し応答しない。配送失敗はタスク実行に影響させない（F-93）。ワークフロー×イベント別のフィルタを設定できる（F-92）。
