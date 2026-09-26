---
type: Runbook
title: OpenCode ツールのセットアップと運用
description: リポジトリ/ワークフローを OpenCode で動かすためのセットアップ（インストール確認・config 設定・アセット自動配置）と、Codex/Claude と異なる縮退（llm 検収不可・heartbeat 無し・triage/design で gh を使えない）の運用上の注意。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core/src/hooks
tags: [operations, runbook, opencode, tool, plugin, doctor]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T15:30:00Z }
status: stable
owner: tomoya-k31
---

# 概要

`[tools]` レジストリ（#196 / [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）で `kind = "opencode"` のツールを割り当てると、
pane 内で OpenCode（TUI）が起動する。完了検知は同一の UDS フック契約
（[POST /agent-events](/apis/agent-events.md)）で、OpenCode 側は `$XDG_CONFIG_HOME/opencode/plugins/` へ
自動配置される totsuka の **JS プラグイン**（`totsuka-opencode.js`）が担う。
plan モードは自動配置される **totsuka-plan エージェント**（`--agent totsuka-plan`、
edit/bash/task 全 deny）で実現する。

**opencode v2 が前提**（[ADR-0105](/decisions/adr-0105-opencode-v2-plugin.md)）。
検証済み: **opencode v2.0.18**（2026-09-26 実機。`opencode run --standalone` に
UDS 受信側を立てて確認）— プラグインの読込（既定エクスポート `{ id, setup }`）、
`ctx.event.subscribe()` の `session.execution.started` / `session.text.ended` /
`session.execution.succeeded` から SessionStart・Stop（マーカー解析込み）の POST、
`question` ツールの `form.created` からの QuestionPending、プラグインからの
UDS POST（Bun fetch `unix:`）、plan agent の `permission` が v2 の
`edit` / `shell` / `subagent: deny` に変換されること。
v1（1.x）は非対応 — プラグインの形が違い、`--standalone` フラグも無い。

totsuka は opencode の argv に必ず **`--standalone`** を付ける（`mode_args` /
`plan_args` で上書きできない）。v2 の既定は共有バックグラウンドサーバー
（`opencode serve --service`）でセッションを動かすが、プラグインはその
サーバー側で動くので、pane に渡した `TOTSUKA_*` env が届かず完了通知が
一切出ない。`--standalone` なら pane の子として専用サーバーが立ち、env を継ぐ。

Codex と違い **trust 手順は不要**（opencode は plugins/ 配下を無条件に読み込む。
そのぶんディレクトリ自体がセキュリティ境界なので、書き込み権限の管理に注意）。
プラグインは `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_JOB_ID` が無い個人セッションでは
フックを一切登録しない。

# セットアップ手順

1. **opencode インストール + サインイン**: `opencode --version` が通ること。
   一度起動して `~/.config/opencode/` が存在すること。
2. **config.toml にツールを割り当て**（組み込み `opencode` があるため `[tools]`
   セクションは不要）:

   ```toml
   [[repositories]]
   name = "my-repo"
   path = "~/src/my-repo"
   tool = "opencode"
   ```

3. **アセット配置**: `totsuka doctor` を実行。`plugins/totsuka-opencode.js` と
   `agents/totsuka-plan.md` が自動配置される（SHA 冪等・改竄検出つき）。
   `opencode-assets` チェックが green ならセットアップ完了。

# 既知の縮退と運用上の注意（ToolCapabilities）

- **マーカー欠落時は 1 回だけ再依頼する**（claude の Stop block 相当、
  [ADR-0106](/decisions/adr-0106-opencode-hook-parity.md)）。UNKNOWN を送った後に
  プラグインが「応答の最終行に <<STATUS:…>> を付けてください」を pane に送る。
  再依頼が pane に user 入力として見えるのは claude と異なる。
- **タスク指示 + マーカー規約は不可視で届く**。プラグインの `context` フックが
  システムプロンプトへ足す（claude の UserPromptSubmit 注入相当）。
- **サブエージェント（タスクツール）のセッションは報告しない**。子のターン終了は
  タスクの完了ではないので、Stop / SessionStart を送らず、マーカー規約も注入しない。
- **triage / design は `gh` を使えない**: `totsuka-plan` が bash を全 deny するため、
  成果物（issue コメント等）をエージェント自身が書けない。claude の deny 同等化は未対応。
- **prompt 型検収なし**: `verification = "llm"` は不可（validate が警告）。
  human か none を使う。
- **heartbeat なし**: 長時間タスクは workflow `timeout_secs` を長めに。
- `opencode run`（非対話モード）には既知の不安定 issue があるため、totsuka の
  pane は TUI 起動のみを使う。
- opencode のメジャー更新でプラグイン API・イベント名が変わりうる（v1→v2 で
  `session.idle` と名前付きエクスポートの読込が無くなり、完了検知が無言で
  止まった）。更新したら下の「タスクが終わらない」の手順で読込を確かめる。

# トラブルシュート

- **エージェントは作業を終えたのにタスクが `dispatched` のまま・pane が閉じない**
  → `totsuka task show <id>` にフックイベントが 1 件も無ければ、プラグインが
  動いていない。`~/.local/share/opencode/log/opencode.log` で
  `failed to load plugin … totsuka-opencode.js` を探す（v1 形のプラグインが
  残っている — totsuka を上げて `totsuka doctor` で再配置）。読込エラーが無い
  なら、pane の opencode が `--standalone` で起動しているか
  （`ps -o command= -p <pid>`）を確認する。
- **タスクが常に timeout する** → `totsuka doctor` の `opencode-assets`。
  アセット欠落/改竄なら自動修復される。プラグイン読込は opencode の再起動後に
  有効になる点に注意（起動中の pane には反映されない）。
- **plan タスクでファイルが編集される** → `agents/totsuka-plan.md` が改竄されて
  いないか doctor で確認（edit/bash/task の全 deny が必須。permission だけの
  部分 deny ではサブエージェント委譲で貫通する — 実機確認済みの罠）。
