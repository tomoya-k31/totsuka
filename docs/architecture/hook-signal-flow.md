---
type: Diagram
title: フックシグナルフロー（Slack メンション → 完了検知 → 検収 → 出力）
description: Claude Code フック完了判定のエンドツーエンド経路。Slack メンションの dispatch から herdr pane 起動・env 注入・claude --settings、Stop フックのマーカー抽出・UDS POST、hook_uds の Bearer/冪等検証、SignalPort→Engine::on_signal の検収分岐（llm/human/none）と Publishing/Verifying/Escalated、スプールフォールバックと pane.exited デッドマン、通知クリック → pane フォーカス（click-to-focus、F-94）までを図示する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [architecture, diagram, hook, claude-code, uds, signal, verification, deadman, spool, click-to-focus, epic-131]
timestamp: 2026-07-19T12:00:00Z
status: active
owner: tomoya-k31
---

# 概要

Claude Code の完了検知を screen-manifest からフック機構へ移した後の、タスク 1 本のエンドツーエンド経路。要件は [F-100〜F-107](/product/orchestrator-spec.ja.md)、配置の意思決定は [ADR-0004](/decisions/adr-0004-hook-completion-signal.md)、受信契約は [POST /claude-events](/apis/claude-events.md)。登場コンポーネントは [task-source-slack](/components/task-source-slack.md) / [orchestrator-core](/components/orchestrator-core.md) / [agent-ide-herdr](/components/agent-ide-herdr.md) / [notifier-macos](/components/notifier-macos.md)。

正常系（Slack メンション → 完了 → 検収 → 出力）を主線とし、スプールフォールバックと `pane.exited` デッドマンを分岐で示す。

# 正常系シーケンス

```mermaid
sequenceDiagram
    autonumber
    participant U as 本人（Slack）
    participant TS as task-source-slack
    participant EN as Engine (orchestrator-core)
    participant HE as agent-ide-herdr
    participant CC as Claude Code pane
    participant OS as on-stop.sh (フック)
    participant UDS as adapters::hook_uds
    participant DB as state.db
    participant NO as notifier-macos

    U->>TS: @mention（スレッド）
    TS->>EN: tasks/fetch（Task, thread_key=channel:thread_ts）
    Note over EN: 冪等取り込み → repo 選択 → スロット確保 → worktree 作成
    EN->>EN: dispatch_one — job_id=job-{task_id}-{session_row}<br/>先行 thread_key があれば resume_session_id を解決（F-105）
    EN->>HE: task/dispatch（HookLaunchSpec{settings_path, env}, resume_session_id?）
    HE->>CC: workspace.create / agent.start（env 注入: TOTSUKA_JOB_ID / HOOK_ENDPOINT / HOOK_TOKEN / HOOK_SPOOL_DIR / PROMPT_CONTEXT）<br/>argv: claude --settings orchestrator-<workflow>.json [--resume <sid>]
    CC->>UDS: SessionStart フック → POST /claude-events
    UDS->>EN: SignalPort::submit（SessionStart）
    EN->>DB: set_claude_session_id（E-09 相関確立）

    Note over CC: エージェント作業（応答末尾に <<STATUS:...>> マーカー）
    CC->>OS: Stop フック発火
    OS->>OS: last_assistant_message から最後の <<STATUS:...>> を抽出（D-12）<br/>background_tasks 非空なら heartbeat のみ
    OS->>UDS: curl --unix-socket / Authorization: Bearer / POST /claude-events（compact JSON）
    UDS->>UDS: 0600 socket（第一層）+ Bearer 定数時間比較（第二層）<br/>job_id 検証・body<=1MiB → 即 200
    UDS->>EN: SignalPort::submit → PluginEvent::HookSignal → Engine::on_signal
    EN->>DB: touch_last_signal（冪等判定前）→ record_hook_event（UNIQUE で重複 drop, D-05）
    Note over EN: job_id → task 解決（E-09。未知 task は warn のみ・永続化しない）

    alt verification = "none" または "llm"（COMPLETED 受信）
        Note over EN,CC: llm は orchestrator-<workflow>.json のセッション内 prompt 型 Stop フック（rubric）が判定
        EN->>EN: finalize_success（マーカー除去済み last_assistant_message を成果物へ）
        EN->>HE: 出力ポリシー（pull_request=push+PR / source=result/publish / none）
        HE->>TS: result/publish（source の場合 → 本人名義スレッド返信の下書き・承認フロー）
        EN->>HE: task/cancel（冪等 → Done pane 自動クローズ, F-107）
        EN->>NO: notify(done)
    else verification = "human"
        EN->>DB: SelfReportComplete → Verifying（スロット保持, F-45）
        EN->>NO: notify(verification_pending 🔍)
        U->>EN: totsuka task verify --pass（→ 次 recover で publish）/ --fail（→ Running）
    end
```

# 異常系・フォールバック

```mermaid
flowchart TD
    STOP["Stop フック / on-stop.sh"] --> POST{"UDS へ POST 成功?"}
    POST -->|"200"| ENG["Engine::on_signal"]
    POST -->|"失敗（2 回リトライ後）"| SPOOL["spool_dir へ NDJSON 追記（E-07）"]
    SPOOL -.->|"replay_spool()（recover + 各サイクル）"| ENG
    SPOOL --> CORRUPT{"parse 不能行?"}
    CORRUPT -->|"あり"| QUAR[".corrupt へ隔離リネーム（削除しない）"]
    CORRUPT -->|"なし・全行処理"| DEL["スプールファイル削除"]

    ENG --> MARK{"マーカー / event 種別"}
    MARK -->|"COMPLETED"| PUB["Publishing / Verifying / 直接 publish"]
    MARK -->|"NEEDS_INPUT"| WI["WaitingInput + notify（本人がpaneで返信→次Stopで自然復帰, D-07）"]
    MARK -->|"FAILED"| FAIL["Failed（pane 保持）"]
    MARK -->|"マーカー欠落 & stop_hook_active=false"| BLOCK["block 差し戻し（再出力要求, R-03）"]
    MARK -->|"UNKNOWN（stop_hook_active=true）"| UNK{"UNKNOWN 連続 >= block_retry_limit?<br/>（DB 再計算・自己申告不使用, D-02）"}
    UNK -->|"はい"| ESC["Escalated（非終端）+ diagnostics/snapshot + notify(escalated 🚨)"]
    UNK -->|"いいえ"| WAIT["遷移なし・次シグナル待ち"]

    SWEEP["sweep_signal_timeouts()（各サイクル）"] -->|"now - last_signal_at > timeout_secs（既定1800, D-03）"| ESC

    DEAD["events.subscribe → pane.exited デッドマン専用（F-106）"] -->|"exit_code 非0 / コード無し"| FAIL
    DEAD -->|"exit_code 0（clean）"| NOP["通知なし（SessionEnd が既報）"]
```

# 通知クリック → pane フォーカス（click-to-focus、F-94）

中間イベント（`waiting_input` / `escalated` / `verification_pending` / `failed`）の通知に気づいた本人が**クリックひとつで対象タスクの pane へ着地する**経路（#155、[ADR-0005](/decisions/adr-0005-click-to-focus.md)）。GUI 前面化（terminal-notifier ネイティブ）と herdr 内フォーカス（`session/focus` 委譲）の 2 段で実現する。

```mermaid
sequenceDiagram
    autonumber
    participant EN as Engine (orchestrator-core)
    participant NO as notifier-macos
    participant TN as terminal-notifier
    participant U as 本人
    participant CLI as totsuka focus <task>
    participant CTL as 制御 UDS (POST /focus)
    participant HE as agent-ide-herdr
    participant HD as herdr socket
    participant AL as GUI ターミナル (Alacritty 等)

    EN->>NO: notify(waiting_input, task_id=42, …)
    NO->>TN: -title … -message …<br/>-execute 'totsuka focus 42'<br/>-activate <bundle-id> -group totsuka-42
    Note over TN: 通知センターに表示
    U->>TN: 通知をクリック
    par ネイティブ前面化
        TN->>AL: -activate → GUI ターミナルを前面化
    and コマンド実行
        TN->>CLI: -execute → totsuka focus 42
    end
    CLI->>CTL: POST /focus {task_id:42}（hook UDS 同居・Bearer）
    CTL->>EN: PluginEvent::Focus（oneshot 応答付き）
    EN->>HE: session/focus { session_id }（0.1.4、pane_control 宣言時のみ）
    HE->>HD: pane.get（生存確認）→ workspace.focus → tab.focus → pane.focus
    HD-->>HE: ok
    HE-->>EN: { focused: true }
    EN-->>CTL: FocusOutcome（oneshot）
    CTL-->>CLI: {"focused": true}
    Note over U,AL: GUI 前面 + 対象 pane フォーカス済み
```

縮退（すべて静か・クリック経路を壊さない）: terminal-notifier 未導入 → notifier が osascript へフォールバック（クリック不可だが通知は出る）。Orchestrator 停止中のクリック → `-activate` の前面化のみ成立し `totsuka focus` は exit 0 の no-op。pane 消失・`pane_control` 非宣言 → `focused: false` + 理由の正常応答。

# 要点

- **受信はコア側**（`ports::SignalPort` + `adapters::hook_uds`）。プラグインは env 注入と `--settings`/`--resume` 起動だけを不透明に配線する（[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)）。
- **冪等の正本は DB**（`hook_events` UNIQUE, D-05）。多重発火・スプール再送・curl リトライは同一冪等キーで無害化される。
- **生存アンカー**（`touch_last_signal`）は冪等判定より前に更新する。中間 Stop=heartbeat が同一冪等キーに潰れても、`sweep_signal_timeouts` の誤エスカレーションを防ぐ。
- **中間イベント**（WaitingInput / Escalated / VerificationPending / Failed）は notifier のみへ配送し、ソーススレッドへは返さない（R-08/D-07）。
- 会話継続（F-105）は `thread_key` 相関で先行セッションを `claude --resume` するが、シグナルは常に自タスクの `job_id` 起点で配路し、共有セッション id から宛先を推測しない（E-09）。

# 関連

- [F-100〜F-107 決定的な完了シグナル / F-94 click-to-focus](/product/orchestrator-spec.ja.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md) / [ADR-0005 click-to-focus の機構選定](/decisions/adr-0005-click-to-focus.md)
- [POST /claude-events（UDS フック受信・POST /focus 制御）](/apis/claude-events.md)
- [click-to-focus セットアップ](/operations/click-to-focus-setup.md)
- [orchestrator-core](/components/orchestrator-core.md) / [agent-ide-herdr](/components/agent-ide-herdr.md) / [task-source-slack](/components/task-source-slack.md) / [notifier-macos](/components/notifier-macos.md)
- [フックのセキュリティ](/security/hook-security.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
