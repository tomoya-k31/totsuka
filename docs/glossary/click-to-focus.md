---
type: Term
title: click-to-focus（クリックで pane を開く）
description: 通知をクリックすると、その通知を出したタスクの herdr pane が前面に来る機能（F-94）。terminal-notifier の -activate（GUI 前面化）+ -execute（totsuka focus → 制御 UDS /focus → agent_ide の session/focus 委譲）の 2 段で実現し、縮退はすべて静か。
tags: [glossary, notifier, terminal-notifier, focus, pane, f-94]
timestamp: 2026-07-23T00:00:00Z
status: active
owner: tomoya-k31
---

# click-to-focus

通知（`waiting_input` / `escalated` / `verification_pending` / `failed`）をクリックすると、GUI ターミナルが前面化し、**その通知を出したタスクの herdr pane がフォーカスされる**機能（F-94、#155）。タスク並走時に「どの pane で待っているのか」を探す手間をなくす。

2 段で実現する（[ADR-0005](/decisions/adr-0005-click-to-focus.md)）:

1. **GUI 前面化** — [notifier-macos](/components/notifier-macos.md) の terminal-notifier バックエンドが `-activate <bundle-id>` でネイティブに行う。
2. **herdr 内 pane フォーカス** — クリックの `-execute` が `totsuka focus <task_id>` を実行し、制御 UDS [`POST /focus`](/apis/agent-events.md) → Engine が task→session を解決 → agent_ide プラグインの `session/focus`（プロトコル 0.1.4、`pane_control` 宣言時のみ）→ herdr の `workspace.focus`→`tab.focus`→`pane.focus` チェーン。session id の復号はプラグイン内に閉じる（F-37 不透明契約）。

縮退はすべて静か（クリック経路を壊さない）: terminal-notifier 未導入 → osascript フォールバック、Orchestrator 停止中 → アプリ前面化のみ、pane 消失・`pane_control` 非宣言 → `focused: false` の正常応答。導入手順は [click-to-focus セットアップ](/operations/click-to-focus-setup.md)。
