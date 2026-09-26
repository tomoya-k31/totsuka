---
type: Decision
title: ADR-0105 opencode の完了検知を v2 のプラグイン API に移し、opencode を必ず --standalone で起動する
description: opencode v2 で totsuka-opencode.js が読み込まれず、共有バックグラウンドサーバーに pane の env も届かないため、opencode タスクの完了が一切検知されなくなった。プラグインを v2 の既定エクスポート + ctx.event.subscribe に書き直し、argv の先頭に --standalone を固定した決定。v1 互換は捨てる。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/hooks/totsuka-opencode.js
tags: [decision, opencode, tool, plugin, hooks, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T22:30:00+09:00 }
verified:
  - { by: claude-code/opus-5.5, at: 2026-09-26T22:15:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。opencode v2.0.18 の実機で、UDS 受信側を立てて SessionStart / Stop / QuestionPending の到達を確認した。totsuka 経由の pane での通しの確認はまだ（下の Consequences）。

# Context

discord-clip を opencode（v2.0.18）で動かすと、エージェントは commit / push まで終えるのに、タスクが `dispatched` のまま pane が閉じなかった。`totsuka task show` にフックイベントが 1 件も無かった。原因は 2 つ重なっていた。

1. **プラグインが読み込まれていない。** v2 のローダーは `export default { id, setup }` しか受け付けず、v1 形（名前付きエクスポートの関数が `event` フックを返す）の `totsuka-opencode.js` を `Plugin must export a default definition with an id and an effect or setup function` で捨てる。エラーは opencode のログに WARN で出るだけで、TUI には何も出ない
2. **読み込まれても env が無い。** v2 の TUI は、既定では共有のバックグラウンドサーバー（`opencode serve --service`、launchd 相当で常駐）に接続し、プラグインはそのサーバー側で動く。サーバーは pane より先に起動しているので、pane に渡した `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_JOB_ID` を持たず、プラグインの env ゲートで何も購読しない

加えて、v1 で使っていたイベント（`session.idle` / `session.status`）と `client.session.messages` は v2 に無い。実機のイベント列（`opencode run --standalone` に記録用プラグインを入れて採取）では、1 ターンは `session.execution.started` → … → `session.text.ended`（本文を持つ）→ `session.execution.succeeded` で閉じ、`question` ツールは `form.created`（`metadata.kind = "question"`）として現れた。

# Decision

1. **プラグインを v2 の API で書き直す。** 既定エクスポート `{ id: "totsuka-opencode", setup(ctx) }` で `ctx.event.subscribe()` を読み、ワイヤ契約（[POST /agent-events](/apis/agent-events.md)）は変えずに次へ対応づける
   - `session.execution.started`（セッションごとの初回）→ `SessionStart`。v2 のイベント列にセッション作成イベントが無いため
   - `session.text.ended` → 最新の assistant メッセージの本文を ordinal 順に保持（マーカー解析の材料。別途メッセージを取りに行かない）
   - `session.execution.succeeded` → `Stop`（最後のマーカーを解析、無ければ UNKNOWN）。`prompt_id` は assistant メッセージ id
   - `session.execution.interrupted` → `reason` が `shutdown` 以外なら `Stop`（v1 で中断後に idle が来ていたのと同じ扱い）。`shutdown` は opencode の終了で、ターンの終わりではない
   - `session.execution.failed` → `Stop{FAILED}`
   - `form.created` かつ `metadata.kind = "question"` → `QuestionPending`。`prompt_id` はフォーム id（質問ごとに distinct）。フォームが開いている間はターン終了イベントが来ないので、v1 の `pendingQuestions` による idle 抑止は要らなくなった
2. **opencode の argv の先頭に `--standalone` を固定する。** pane の子として専用サーバーが立ち、env を継ぐ。`mode_args` / `plan_args` の置き換えでは消えない位置に置く — これが無いと完了検知が無言で止まるので、運用者が上書きで外せる値にしない
   - サーバーの env を外から設定する案（`opencode service set` 等）は却下。サーバーは全 pane・個人セッションで共有されるので、タスクごとの `TOTSUKA_JOB_ID` を持てない
   - プラグインの `options` で渡す案も却下。options は設定ファイルに書くもので、タスクごとに変わる値を運べない
3. **v1 互換は捨てる。** 1 ファイルで両方の形を出すと、v1 ローダーは全エクスポートを関数として呼ぶので既定エクスポートのオブジェクトで壊れる。`--standalone` も v1 には無い。opencode は自動更新が既定で、v1 に留まる利用者を想定しない
4. plan エージェント `agents/totsuka-plan.md` は変えない。v2 は v1 形の `permission: {edit, bash, task: deny}` を `edit` / `shell` / `subagent: deny` に変換して末尾に置くことを `opencode debug agents` で確認した

# Consequences

- JS にはテスト基盤が無いので、Rust 側の文字列ピン（`embedded_plugin_uses_the_v2_plugin_api` ほか）で既定エクスポート・購読・イベント名を固定した。ピンは形を守るだけで、opencode 側のイベント名が変わったことは捕まえられない。メジャー更新時は [OpenCode ランブック](/operations/opencode-tool-setup.md) の手順で読込を確かめる
- 実機確認は `opencode run --standalone` で行った（受信側は UDS の小さな HTTP サーバー）。`opencode run` はターン終了直後にプロセスを畳むため、ソケットが無いときの spool への退避は確かめられていない（TUI は残るので本番では 5 秒のタイムアウト後に退避される想定）
- totsuka が起動した pane（TUI）での通しの確認は、この PR のマージ後に実タスクで行う
- 各 pane が専用サーバーを持つので、pane ごとに opencode サーバープロセスが 1 つ増える

# 関連

- [OpenCode ツールのセットアップと運用](/operations/opencode-tool-setup.md)
- [POST /agent-events](/apis/agent-events.md)
- [ADR-0050](/decisions/adr-0050-question-tool-asking.md)（質問ツールの経路。opencode 側の送出元はこの ADR で変わった）
- [ADR-0014](/decisions/adr-0014-tool-abstraction.md)（ツール抽象）
