---
type: Decision
title: ADR-0005 通知 click-to-focus は terminal-notifier + session/focus 委譲で実現する
description: 通知クリックで対象タスクの herdr pane を開く F-94 を、terminal-notifier（-execute/-activate）+ `totsuka focus` + 制御 UDS + agent_ide への `session/focus` 委譲（0.1.4 additive、pane_control 相乗り）で実現する決定。UNUserNotificationCenter 自前 .app・alerter・NotifyParams への pane_id 追加は不採用。
resource: https://github.com/tomoya-k31/totsuka/issues/155
tags: [notifier, terminal-notifier, click-to-focus, pane, herdr, plugin-protocol, f-94]
generated: { by: human:tomoya-k31, at: 2026-07-18T15:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: ref-1
    resource: https://github.com/tomoya-k31/totsuka/issues/155
    title: "Issue #155 click-to-focus 詳細設計（F-94）"
  - id: ref-2
    resource: https://github.com/sst/opencode/issues/23446
    title: "opencode #23446 — macOS 通知 owner が Script Editor になり click-to-focus できない"
  - id: ref-3
    resource: https://github.com/julienXX/terminal-notifier
    title: "terminal-notifier（-execute / -activate / -group）"
  - id: ref-4
    resource: /components/notifier-macos.md
    title: notifier-macos
  - id: ref-4b
    resource: /components/agent-ide-herdr.md
    title: agent-ide-herdr
  - id: ref-4c
    resource: /references/herdr-socket-api.md
    title: herdr Socket API
---

# Status

Accepted — 2026-07-18（[#155](https://github.com/tomoya-k31/totsuka/issues/155)、段階実装 PR 1〜5）

# Context

中間イベント（`waiting_input` / `escalated` / `verification_pending` / `failed`）は notifier のみへ配送され（R-08/D-07）、本人へ push で気づかせる設計だが、通知に気づいても**どの pane で待っているのかへ即座に飛べない**。手で Alacritty を前面化し herdr のタブ/ペインを探す必要があり、タスク並走時の負担が大きい。

これを解消する「通知クリック → 対象 pane フォーカス」（**F-94**）の実現には次の制約がある:

1. **osascript 通知はクリック不可**: 現行 [notifier-macos](/components/notifier-macos.md) の AppleScript `display notification` はクリックコールバックを持たず、クリックで開くのは owner アプリ（osascript の場合 Script Editor）。macOS 仕様上の既知の限界（外部事例: opencode #23446）。
2. **"pane" は herdr のペイン**: エージェントは herdr（AI multiplexer）の pane 内で動く。「pane を開く」には (a) ホスト GUI アプリ（Alacritty 等）の前面化と (b) herdr 内での pane/tab/workspace フォーカスの 2 段が必要。
3. **session_id は不透明**（F-37）: `session_id` は agent_ide プラグイン内部形式（herdr プラグインが `(pane_id, agent_session_id)` をエンコード）で、Orchestrator/CLI は中身を解釈しない契約。pane フォーカスを CLI から herdr socket 直叩きで行うと境界を壊す。
4. **相関のギャップ**: `NotifyParams` は pane_id を運ばないが、`task_id` は運ぶ。task → session_id の対応は core の `sessions` テーブルにある。

# Decision

## 1. 通知送出は terminal-notifier をバックエンドに追加する

`NotificationSender` trait に `TerminalNotifierSender` を追加し、`-execute 'totsuka focus <task_id>'`（クリック時コマンド実行）+ `-activate <bundle-id>`（GUI ターミナルのネイティブ前面化）+ `-group totsuka-<task_id>`（タスク別集約）で送出する。`activate_bundle_id`（例 `org.alacritty`）は環境依存の設定とし、terminal-notifier 未導入時は osascript バックエンドへ自動フォールバックする（クリック不可だが通知は出る）。Sequoia 15.x+ で `-sender` と `-activate` の併用が click-to-focus を壊すため **`-sender` は使わない**。

## 2. pane フォーカスは agent_ide プラグインへの `session/focus` 委譲で行う

クリック経路は `totsuka focus <task-id>` → Orchestrator 制御 UDS → core が `task_id → session_id` を解決 → agent_ide プラグインの新 RPC **`session/focus`**（O→P、[plugin-protocol](/components/plugin-protocol.md) 0.1.4 additive）→ herdr プラグインが session_id を復号し herdr socket で workspace/tab/pane をフォーカスする。session_id の復号は herdr プラグイン内に閉じ、不透明契約（F-37）を守る。

- **capability は既存 `pane_control` に相乗り**する（新フラグを足さない）。`pane_control` を宣言するプラグインにのみ `session/focus` を送り、旧プラグイン・orca（GUI フォーカス手段が弱い）へは送らない = アプリ前面化のみの縮退。
- pane 消失は `focused: false` で返し**エラーにしない**（タスク終了後のクリックは正常系）。
- 制御は「実行中の Orchestrator が所有するプラグイン subprocess」を経由する必要があるため（プラグインは Orchestrator が起動・所有）、**制御 UDS 経由が唯一整合する経路**。CLI 単体では session_id を復号できない。

# 代替案と不採用理由

| 案 | 不採用理由 |
|---|---|
| UNUserNotificationCenter を使う自前 `.app` バンドル | 純正 click-to-action を実現できるが署名・常駐デリゲート・配布が重い。将来 `NotificationSender` の別実装として段階移行可能（`session/focus`・制御 UDS・`totsuka focus` の配線は再利用できる） |
| alerter（クリック結果を stdout に返すブロッキング型） | 常駐プロセスとの相性が悪く fire-and-forget 設計（F-93）と噛み合わない |
| URL スキーム `totsuka://focus/42`（`-open`） | ハンドラ登録に結局 `.app` が要るため上案に吸収 |
| `NotifyParams` に pane_id を追加 | notifier に herdr 復号を持たせることになり不透明契約（F-37）を破る。`task_id` 埋め込みで足りる |

# Consequences

- [plugin-protocol](/components/plugin-protocol.md) は 0.1.4 へ（additive、`^0.1` 互換維持）。`session/focus` は `pane_control` 宣言プラグインにのみ送られるため、未対応プラグインとの組合せでも壊れない。
- `-execute` はシェル文字列を実行するため、`click_command` テンプレへの `{task_id}` 埋め込みは必ずクォート/サニタイズする（インジェクション防止）。
- Orchestrator 停止中のクリックは `-activate` によるアプリ前面化のみ成立し、`totsuka focus` は静かに no-op（クリック経路を壊さない）。
- 縮退系（terminal-notifier 未導入 / pane 消失 / `pane_control` 非宣言 / Orchestrator 停止中）はいずれもクラッシュせず、最低限アプリ前面化 or 通知表示のみに落ちる。
