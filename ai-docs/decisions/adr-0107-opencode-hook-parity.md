---
type: Decision
title: ADR-0107 claude の --settings フックの残りを opencode v2 プラグインで揃え、サブエージェントのセッションは報告しない
description: claude に差し込んでいるフックのうち opencode に無かった不可視注入・マーカー欠落時の再依頼・Notification・SessionEnd を、v2 プラグイン API（context フック・ctx.session.prompt・permission.asked・shutdown 中断）で実装し、invisible_injection と marker_block を true にした決定。タスクツールのサブエージェント（parentID 付きセッション）は報告も注入もしない。llm 検収と triage/design の deny 同等化は見送り。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/hooks/totsuka-opencode.js
tags: [decision, opencode, tool, plugin, hooks, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-27T00:30:00+09:00 }
verified:
  - { by: claude-code/opus-5.5, at: 2026-09-27T00:10:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。opencode v2.0.18 の実機（`opencode run --standalone` + UDS 受信側）で、不可視注入・`permission.asked` → Notification・サブエージェントの除外を確認した。再依頼は `ctx.session.prompt` が受理され次のターンが始まるところまで、SessionEnd は未確認（下の Consequences）。

# Context

claude の pane には workflow ごとの `--settings` で Stop（マーカー判定 + マーカー欠落時の block）・Notification・SessionStart・SessionEnd・UserPromptSubmit（`TOTSUKA_PROMPT_CONTEXT` の不可視注入）・PreToolUse（`AskUserQuestion`）・prompt 型 Stop（`verification = "llm"`）と permissions を差し込んでいる。[ADR-0105](/decisions/adr-0105-opencode-v2-plugin.md) 時点の opencode プラグインが持つのは Stop・SessionStart・QuestionPending だけで、残りは `ToolCapabilities` の縮退（指示は可視の `extra_context`、マーカー欠落は即 UNKNOWN）として扱っていた。

v2 の `setup(ctx)` が受け取る `ctx` を読むと、v1 には無かった面がある。実機で次を確かめた。

- `ctx.session.hook("context", fn)` — LLM リクエストごとに `{sessionID, model, system, messages, options, agent, tools}` を渡され、`system.push({type: "text", text})` がそのままシステムプロンプトに入る（注入した指示にモデルが従った）
- `ctx.session.prompt({sessionID, text})` — セッションに user 入力を足して次のターンを始める（`delivery: "steer"` で受理され `session.execution.started` が続いた）
- `permission.asked` — `{id, sessionID, action, resources, message?}`。承認待ちのたびに届く
- タスクツールのサブエージェントは**別セッション**で、`session.created` に `parentID` が付き、自分の `session.execution.succeeded` を出す

最後の点は既存の不具合でもあった。プラグインはセッションを区別しておらず、サブエージェントのターン終了を Stop（マーカー無しなので UNKNOWN）と SessionStart として送っていた。

# Decision

1. **不可視注入**: `TOTSUKA_PROMPT_CONTEXT` があれば `context` フックでシステムプロンプトの末尾に足す。`invisible_injection = true` にし、dispatch は opencode にも指示を env で渡す（可視の `extra_context` には出さない）
2. **マーカー欠落時の再依頼**: `session.execution.succeeded` でマーカーが無ければ、UNKNOWN を送った**後に** `on-stop.sh` の block 理由と同じ文を `ctx.session.prompt` で 1 回だけ送る。再依頼したターンがまたマーカー無しで終わっても 2 度目は送らない（claude の `stop_hook_active` と同じ 1 往復）。オペレーターの中断（`interrupted` の `shutdown` 以外）は再依頼しない。`marker_block = true` にする
3. **Notification**: `permission.asked` を `Notification`（`permission_prompt: <action> <resources>`）で送る。`prompt_id` は承認要求 id — 同じセッションの 2 件目が冪等キーで落ちないように。`--auto` は明示 deny 以外を自動承認するので、発火するのは運用者が `mode_args` / `plan_args` を書き換えたときだけである
4. **SessionEnd**: `interrupted` の `reason = "shutdown"`（opencode の終了）を `SessionEnd{reason: "shutdown"}` で送る。従来は何も送っていなかった
5. **サブエージェントを除外する**: `session.created` に `parentID` があるセッションのターン系イベント（開始・本文・終了・失敗）は捨てる。質問フォーム（`form.created`）と `permission.asked` だけは送る — 子の質問・承認待ちでも親のターンは止まるので、送らないとタスクが park されず timeout まで待つ。`context` フックの入力には `parentID` が無いので、未分類のセッションは `ctx.session.get` で 1 回だけ引いて判定し、子にはマーカー規約を注入しない（子が `COMPLETED` を書くとタスクが早く完了してしまう）

見送ったもの:

- **llm 検収**（prompt 型 Stop）: `ctx.generate.text` で同等のことはできるが、検収プロンプトを env で運ぶ経路と判定ロジックが要り、human への縮退は安全側なので今は入れない。`prompt_verification = false` のまま
- **profile の deny の同等化**: opencode の triage / design は `totsuka-plan`（bash 全 deny）で動くため、成果物を `gh issue comment` 等で書けない。claude は危険なコマンドだけ deny して `gh issue` を残している。権限境界（[ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) / [ADR-0033](/decisions/adr-0033-workflow-profile.md)）の変更なので別の PR で議論する
- **heartbeat**: claude も `background_tasks` があるときしか送らない。opencode にバックグラウンドタスクの概念が無いので対応物が無い

# Consequences

- 3 ツールとも不可視注入を持つので、dispatch の「可視 `extra_context` に降ろす」分岐は組み込みの kind からは通らなくなった。将来の kind のために残す（`has_adapter` の拒否経路と同じ扱い）。opencode でその分岐を駆動していた結合テスト `an_initial_prompt_precedes_the_visible_marker_convention` は削除した — 注入経路の初回プロンプトは claude のテストが覆っている
- 再依頼の文は Rust 側のテスト `reask_text_matches_on_stop_sh` で `on-stop.sh` と一字一句同じであることを固定した
- **再依頼されたターンの結果は実機で見ていない。** `opencode run` はターン終了直後にプロセスを畳むため、再依頼が受理され次のターンが始まるところまでしか観測できない。TUI の pane では続きが走る想定
- **SessionEnd は実機で発火させていない。** `opencode run` ではターンが終わってから終了するので `shutdown` 中断が起きない
- JS には相変わらずテスト基盤が無く、Rust 側の文字列ピンで形を守る。opencode 側の名前（`context` フック・`permission.asked`・`parentID`）が変わってもピンは気づけない

# 関連

- [ADR-0105](/decisions/adr-0105-opencode-v2-plugin.md)（v2 プラグインへの移行）
- [ADR-0014](/decisions/adr-0014-tool-abstraction.md)（ツール抽象と縮退表）
- [OpenCode ツールのセットアップと運用](/operations/opencode-tool-setup.md)
- [POST /agent-events](/apis/agent-events.md)
