---
type: Playbook
title: click-to-focus セットアップ（terminal-notifier / bundle id / 切り分け）
description: 通知クリックで対象タスクの herdr pane を開く F-94 の導入手順。terminal-notifier の導入、plugins/notifier-macos.toml の backend / activate_bundle_id / click_command 設定、bundle id の調べ方、動作確認、クリックが効かない・通知が出ないときの切り分け表。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/notifier-macos
tags: [operations, playbook, notifier, terminal-notifier, click-to-focus, macos]
timestamp: 2026-07-23T00:00:00Z
status: active
owner: tomoya-k31
---

# 目的

通知（`waiting_input` / `escalated` / `verification_pending` / `failed`）をクリックしたとき、GUI ターミナルを前面化し対象タスクの herdr pane をフォーカスする（F-94、[ADR-0005](/decisions/adr-0005-click-to-focus.md)）。既定の osascript バックエンドは**クリック不可**（owner の Script Editor が開くだけ）なので、click-to-focus には terminal-notifier バックエンドへの切り替えが必要。

# セットアップ手順

1. **terminal-notifier を導入する**（Homebrew 配布 = 署名済みバイナリ）:

   ```bash
   brew install terminal-notifier
   ```

2. **前面化したい GUI ターミナルの bundle id を調べる**:

   ```bash
   osascript -e 'id of app "Alacritty"'   # → org.alacritty
   # 例: iTerm2 → com.googlecode.iterm2 / Kitty → net.kovidgoyal.kitty / WezTerm → com.github.wez.wezterm
   ```

3. **`plugins/notifier-macos.toml` を設定する**（notifier プラグインの設定。[notifier-macos](/components/notifier-macos.md)）:

   ```toml
   backend = "terminal_notifier"
   activate_bundle_id = "org.alacritty"          # 環境依存・手順 2 の値
   # 以下は既定値のままでよい:
   # terminal_notifier_bin = "terminal-notifier"
   # click_command = "totsuka focus {task_id}"
   ```

   `totsuka` バイナリは terminal-notifier がクリック時に起動するシェルから見える PATH に必要（Homebrew/`~/.local/bin` 等の標準的な場所なら通常問題ない）。

4. **検証する**:

   ```bash
   totsuka config validate          # terminal-notifier の -help 疎通（未導入なら actionable エラー）
   totsuka run --watch              # 実タスクで通知を発生させる
   totsuka focus <task-id>          # クリックを介さず focus 経路だけを手動確認
   ```

   通知クリックで「GUI ターミナルが前面化し、対象タスクの pane がフォーカスされる」ことを確認する。並走タスクがある場合は、それぞれの通知が正しい方の pane を開くこと（`-group totsuka-<task_id>` でタスク別に集約される）。

# 切り分け表

| 症状 | 原因候補 | 対処 |
|---|---|---|
| 通知は出るがクリックしても何も起きない | `backend` が既定の `osascript` のまま | `plugins/notifier-macos.toml` に `backend = "terminal_notifier"` を設定 |
| クリックでアプリは前面化するが pane が変わらない | Orchestrator 停止中（`totsuka focus` は静かに no-op）/ pane が既に閉じている / agent が `pane_control` 非宣言（orca 等） | `totsuka focus <task-id>` を手で実行し理由を確認（`focus skipped: …` / `pane not focused — …` が原因を表示） |
| クリックでコマンドは走るがアプリが前面化しない | `activate_bundle_id` 未設定 or bundle id が誤り | 手順 2 で正しい id を確認して設定 |
| `config validate` が terminal-notifier のエラーを出す | 未導入 / PATH 外 / `terminal_notifier_bin` が誤り | `brew install terminal-notifier` するか絶対パスを設定。導入せず使う場合は `backend = "osascript"` に戻す（通知は出るがクリック不可） |
| terminal-notifier 未導入のまま run している | 送信単位で osascript へ自動フォールバック（警告ログあり） | 通知自体は届く。click-to-focus が要るなら導入する |
| 401 が返る（`totsuka focus` の出力） | 実行中 receiver と `[hooks].auth_token_ref` の不一致 | トークンを揃えて `totsuka run` を再起動（[hook-troubleshooting](/operations/hook-troubleshooting.md) 参照） |

# 関連

- [フックシグナルフロー — 通知クリック → pane フォーカス](/architecture/hook-signal-flow.md)
- [ADR-0005 click-to-focus の機構選定](/decisions/adr-0005-click-to-focus.md)
- [notifier-macos](/components/notifier-macos.md) / [orchestrator-cli（totsuka focus）](/components/orchestrator-cli.md) / [POST /focus](/apis/agent-events.md)
