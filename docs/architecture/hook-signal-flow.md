---
type: Diagram
title: フックシグナルフロー（Slack メンション → 完了検知 → 検収 → 出力）
description: Claude Code フック完了判定のエンドツーエンド経路。Slack メンションの dispatch から herdr pane 起動・env 注入・claude --settings、Stop フックのマーカー抽出・UDS POST、hook_uds の Bearer/冪等検証、SignalPort→Engine::on_signal の検収分岐（llm/human/none）と Publishing/Verifying/Escalated、スプールフォールバックと pane.exited デッドマンまでを図示する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-core
tags: [architecture, diagram, hook, claude-code, uds, signal, verification, deadman, spool, epic-131]
timestamp: 2026-07-18T12:00:00Z
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
    HE->>CC: workspace.create / agent.start（env 注入: TOTSUKA_JOB_ID / HOOK_ENDPOINT / HOOK_TOKEN / HOOK_SPOOL_DIR）<br/>argv: claude --settings orchestrator-<workflow>.json [--resume <sid>]
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

# 要点

- **受信はコア側**（`ports::SignalPort` + `adapters::hook_uds`）。プラグインは env 注入と `--settings`/`--resume` 起動だけを不透明に配線する（[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)）。
- **冪等の正本は DB**（`hook_events` UNIQUE, D-05）。多重発火・スプール再送・curl リトライは同一冪等キーで無害化される。
- **生存アンカー**（`touch_last_signal`）は冪等判定より前に更新する。中間 Stop=heartbeat が同一冪等キーに潰れても、`sweep_signal_timeouts` の誤エスカレーションを防ぐ。
- **中間イベント**（WaitingInput / Escalated / VerificationPending / Failed）は notifier のみへ配送し、ソーススレッドへは返さない（R-08/D-07）。
- 会話継続（F-105）は `thread_key` 相関で先行セッションを `claude --resume` するが、シグナルは常に自タスクの `job_id` 起点で配路し、共有セッション id から宛先を推測しない（E-09）。

# 関連

- [F-100〜F-107 決定的な完了シグナル](/product/orchestrator-spec.ja.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md)
- [POST /claude-events（UDS フック受信）](/apis/claude-events.md)
- [orchestrator-core](/components/orchestrator-core.md) / [agent-ide-herdr](/components/agent-ide-herdr.md) / [task-source-slack](/components/task-source-slack.md) / [notifier-macos](/components/notifier-macos.md)
- [フックのセキュリティ](/security/hook-security.md) / [フックのトラブルシューティング](/operations/hook-troubleshooting.md)
