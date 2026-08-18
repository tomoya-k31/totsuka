---
type: Decision
title: ADR-0050 design / implement の確認依頼は質問ツールの選択 UI で行い、QuestionPending が park を代替する
description: "attended pane の design / implement で、完了確認や途中の質問を平文 + NEEDS_INPUT ではなく各ツールの選択 UI（claude AskUserQuestion / opencode question、codex は番号付きリスト）で行う決定。質問ダイアログ中はターンが終わらず Stop が届かないため、PreToolUse / tool.execute.before から新ワイヤイベント QuestionPending を送り WaitingInput への park（スロット解放・通知）を代替する。マーカーは完了ワイヤシグナルとして不変（ADR-0020）で、NEEDS_INPUT は質問ツール不能時のフォールバックに残る。"
resource: https://github.com/tomoya-k31/totsuka/issues/487
tags: [decision, profile, marker, attended-pane, prompts, hooks, ask-user-question, adr]
generated: { by: claude-code/fable-5, at: 2026-08-18T00:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: issue-487
    resource: https://github.com/tomoya-k31/totsuka/issues/487
    title: "design/implement の質問・完了確認を選択 UI 化する"
  - id: codex-issue-11536
    resource: https://github.com/openai/codex/issues/11536
    title: "Continue on Ask Question Tool (request_user_input in Default mode)"
---

# Status

stable（[#487](https://github.com/tomoya-k31/totsuka/issues/487)）。[ADR-0043](/decisions/adr-0043-human-approved-completion.md)（人間承認の完了プロトコル）の「訊き方」を質問ツール保有ツール向けに改める。**実機検収は未了**（検証節参照）。

# Context

[ADR-0043](/decisions/adr-0043-human-approved-completion.md) で design / implement の完了は人間が pane 上で承認する形になったが、確認依頼は「平文で要約 + `<<STATUS:NEEDS_INPUT reason="awaiting completion confirmation">>` で停止」であり、人間は自由テキストを打って応答する。これを各ツールの選択 UI（単一選択ピッカー）にしたい。

制約として確定していた事実:

- **Claude Code の `AskUserQuestion` はターンを終わらせない**。ダイアログ待機中は Stop フックが発火せず、マーカーは届かない（[ADR-0038](/decisions/adr-0038-workflow-initial-prompt.md) D6 が無人 pane でのハング要因として記録済み）。つまり「マーカーで park してから UI を出す」ことは 1 ターン内では構造的に不可能
- `WaitingInput` への park（スロット解放 F-45・D-03 掃引対象外・notifier 通知）は ADR-0043 の核で、UI 化のために失ってよいものではない
- codex の `request_user_input` は **Plan Mode 限定**（Default mode 非対応 [^codex-issue-11536]）
- opencode には native の `question` ツールがあるが、build（`--auto`）モードでの可用性と、ダイアログ待機中に `session.idle` が発火するか（発火すると JS プラグインがマーカー無しと判定して UNKNOWN を送り、D-02 のエスカレーション streak に入る）は未検証

# Decision

**確認・質問の「訊き方」を質問ツールへ移し、park は新ワイヤイベント `QuestionPending` が代替する。** マーカー語彙と COMPLETED / FAILED の完了ワイヤシグナルは不変（[ADR-0020](/decisions/adr-0020-status-marker-stays.md) はそのまま生きる）。対象は design / implement profile のみ（answer / triage は Slack 経由応答のため現状維持、spelled-out 記法も #440 と同じ線引きで無変更）。

| ツール | 訊き方 | park の経路 |
|---|---|---|
| claude | プロンプト（新キー `marker_self_report_confirm_question`、`{question_tool}` = `AskUserQuestion`）が単一選択の確認を指示 | PreToolUse フック（matcher `AskUserQuestion` → `on-ask-user-question.sh`）が `QuestionPending` を POST |
| opencode | 同じプロンプト（`{question_tool}` = `question`、visible extra_context 経由） | `totsuka-opencode.js` の `tool.execute.before` が `QuestionPending` を POST + ダイアログ待機中の idle 判定を抑止 |
| codex | `marker_self_report_confirm` に追記された番号付きリスト提示（人間は番号 1 文字で回答） | 従来どおり `Stop{NEEDS_INPUT}`（変更なし） |

機構上の要点:

1. **プロンプト変種は dispatch 時に選ぶ**（`ToolCapabilities::interactive_question` × `Prompts::confirm_selected`）。ツール解決は repo 次元（workflow.tool > repo.tool > default_tool）を持つため、workflow 単位の `resolve_for` では選べない
2. **`QuestionPending` は `on_stop_needs_input` と同型に park する**（`Dispatched|Running` → WaitInput、`Escalated` → WaitInput、いずれもスロット解放 + `WaitingInput` 通知。通知本文は質問文の要約）。event 文字列は `question_pending` で `'stop'` ではないため、D-02 の UNKNOWN streak に影響しない。未知イベント名は従来どおり Heartbeat に縮退するので、旧バイナリ + 新フックの組合せも安全
3. **`prompt_id` は質問ごとに distinct**（claude: `tool_use_id`、無ければ `tool_input` の cksum。opencode: `callID`）。冪等キーの下で空だと 2 問目が Duplicate として黙って落ちる。park 済み（`WaitingInput`）への新しい質問は状態不変のまま**再通知する** — ADR-0043 の既知の制限「2 回目の NEEDS_INPUT は再通知されない」を質問経路では解消する
4. **NEEDS_INPUT はフォールバックとしてプロンプト内に残る**（質問ツールが使えない・失敗した場合の指示）。`missing_markers` 検証（3 マーカー必須）は新変種にも同じ経路で効く
5. llm 検収 rubric（`verification_rubric_human_approval`）は**質問ツールへの人間の回答を明示承認として数える**文言に拡張（transcript 上の形が未検証のため「どんな形で現れるかを問わない」寛容な書き方）

# トレードオフ（受容済み）

**Claude のターンは質問待機中も終わらない。** task 状態は `QuestionPending` で `waiting_input` になるが、エージェントプロセスはツール結果待ちで生きている。人間が回答したあとの再開は何もイベントを発火しない（`ResumeInput` はスコープ外）ので、次の Stop まで `waiting_input` のままになる — これは現行の「pane に手打ちで応答した場合」と同じ挙動であり、後退ではない。PreToolUse が発火しない事態（実機未検証のリスク）では task は `Running` のままになり、`timeout_secs = 0`（[ADR-0042](/decisions/adr-0042-timeout-zero-opt-out.md)）の attended 運用では掃引されずに済む。

# 不採用案

- **Stop フック `decision:"block"` リレー**: 従来どおり NEEDS_INPUT で停止 → on-stop.sh がイベントを POST（park 成立）した後に block して「AskUserQuestion で提示せよ」と再開指示する案。park セマンティクスを完全維持できるが、1 ターン止めてから UI を出す遠回りで、要約テキストとピッカーが二重に出る。ユーザーがトレードオフ承知で直接方式を選択した
- **Notification フック（`agent_needs_input` matcher）で park**: AskUserQuestion 待機に対応する Notification matcher は文書化されておらず（`agent_needs_input` はバックグラウンドセッション用）、hook input で「permission 待ち」と「質問待ち」を区別する手段も無い。PreToolUse は全ツール呼び出しで発火するため確実で、質問文（`tool_input.questions`）も取れる
- **codex に `request_user_input` を指示**: Plan Mode 限定で Default mode では使えない [^codex-issue-11536]。解放要望（openai/codex #11536 / #30150）が通れば再考
- **マーカー廃止**: [ADR-0020](/decisions/adr-0020-status-marker-stays.md) の再確認そのもの。マーカーは 3 ツール共通の唯一の完了シグナルであり、本決定は「訊き方」だけを動かして完了シグナルには触れない

# 検証

ローカル: プロンプト変種の選択（profile × tool の dispatch 統合テスト）、`QuestionPending` の park / 再通知 / Duplicate / park 後 COMPLETED の publish、レンダリング（confirm profile のみ PreToolUse ブロック、他はバイト同一）、スクリプトの stdout 空・prompt_id 導出・JOB_ID ゲート・jq 欠落 spool を単体/統合テストで固定済み。

実機（live-e2e、未了）:

1. claude: PreToolUse が AskUserQuestion で実際に発火するか・`tool_use_id` の有無・空 stdout がダイアログを乱さないか
2. claude: design profile で確認ピッカー到達 → `waiting_input` park → 通知に質問文 → 承認選択後 COMPLETED → 拡張 rubric をジャッジが通すか（回答の transcript 上の形の観察）
3. opencode: build モードで `question` が使えるか（不可ならプロンプトのフォールバックで番号付きリストに縮退することの確認）・ダイアログ中の `session.idle` 発火と `pendingQuestions` ガードの実効
4. 起動中 opencode の旧プラグイン混在挙動

[^codex-issue-11536]: Continue on Ask Question Tool (request_user_input in Default mode)
