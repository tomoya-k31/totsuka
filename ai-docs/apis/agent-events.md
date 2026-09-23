---
type: API Endpoint
title: POST /agent-events（UDS フック受信）
description: エージェント CLI（Claude Code / Codex / OpenCode）のフック/プラグインが完了/通知/セッションイベントを orchestrator-core へ通知する UDS 上の HTTP エンドポイント。Bearer 認証・即 200・AgentSignal 正規化。制御エンドポイント POST /focus（click-to-focus、F-94）と POST /task/cancel・/task/retry（#760）も同一ソケットに同居。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/adapters/hook_uds.rs
tags: [api, uds, hook, claude-code, codex, opencode, signal, ingress]
generated: { by: claude-code/opus-5.5, at: 2026-09-23T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 概要

herdr ペイン上のエージェント CLI（Claude Code / Codex は共有フックスクリプト群、OpenCode は JS プラグイン `totsuka-opencode.js` — いずれも同一契約で送出、#196）が発火するフック（Stop / Notification / SessionStart / SessionEnd）が、完了自己申告や中間イベントを orchestrator-core へ通知するための受信口（#131 / #136）。screen-manifest 画面検出に代わる決定的な完了判定の入口である。

driving adapter [`adapters::hook_uds`](/components/orchestrator-core.md) が実装し、正規化した [`domain::signal::AgentSignal`](/components/orchestrator-core.md) を `ports::SignalPort` 経由で Engine へ投入する。

> **旧名について（#196 rename）**: 本エンドポイントは ≤0.2.2 では `POST /claude-events`（ソケット名 `claude-events.sock`）だった。ツール抽象化（claude 固定の解消）に伴い `agent-events` へ改名。受信側は制御パス（`/focus`・`/task/cancel`・`/task/retry`）の完全一致以外の**全パスをシグナル受信として扱う**ため、旧パス `/claude-events` への POST も引き続き受理される（互換窓は事実上恒久。旧 concept: [claude-events](claude-events.md) は deprecated）。

# トランスポート

- **Unix domain socket**。既定パス `${XDG_RUNTIME_DIR}/totsuka/agent-events.sock`（`[hooks].socket_path` で上書き可）。パーミッションは **0600**（同一ユーザのみ接続可＝第一の認証層、E-03）。旧既定名 `claude-events.sock` の stale ソケットは `totsuka run` 起動時に掃除される。
- **最小 HTTP/1.1**。ヘッダを `\r\n\r\n` まで読み、`Content-Length` バイト分の body を読む。**chunked 転送は非対応**（フックは固定長 `curl --data` POST）。method は検査せず、path は**完全一致の `/focus`・`/task/cancel`・`/task/retry`（制御エンドポイント、下記）のみ**ルーティングし、それ以外の全 method/path はシグナル受信として扱う（E-08 前方互換。パスは慣例上 `/agent-events`、旧 `/claude-events` も可）。
- **1 接続 1 リクエスト**で close（keep-alive 非対応）。

# 認証（E-03）

- `Authorization: Bearer <token>`。`token` は起動時に `[hooks].auth_token_ref`（keychain 参照等）を解決した値で、herdr の env 注入経由でフックへ供給される。
- 比較は定数時間。不一致・欠落は **401 + 警告ログ**のみで listener は落とさない。
- `[hooks].auth_token_ref` 未設定時は認証チェックを行わず（0600 ソケットのみで保護）、CLI が警告を出す。

# リクエスト（body JSON）

`job_id` のみ必須。未知フィールドは許容し、監査用に body 全体を `AgentSignal.payload` へ温存する（E-08）。

| フィールド | 必須 | 意味 |
|---|---|---|
| `job_id` | ✔ | `"job-{task_id}-{session_row}"`。`TOTSUKA_JOB_ID` のエコーバック。相関はこれのみで行い session_id からの推測はしない（E-09） |
| `session_id` | | ツールネイティブのセッション id（相関補助・冪等キー要素。DB では `tool_session_id`） |
| `prompt_id` | | 冪等キー要素（codex では stdin の `turn_id`、opencode では最終メッセージ id を送信側がこのフィールドへ載せ替える — ワイヤ形は不変、#196） |
| `hook_event_name` | | `Stop` / `Notification` / `QuestionPending` / `SessionStart` / `SessionEnd`。未知/欠落は `Heartbeat`（生存のみ、誤完了を避ける最も非断定な扱い）へ正規化。**これが正本のイベント種別キー**（旧 `event` フィールドではない。フックスクリプト `on-stop.sh` 等はこの名で送出する #138） |
| `status` | | `Stop` 時: `completed` / `needs_input` / `failed` / `unknown`。**大小無視で照合**（`on-stop.sh` はマーカー語 `COMPLETED` 等を大文字のまま送るため） |
| `reason` | | 補足理由 |
| `last_assistant_message` / `transcript_path` | | `Stop` 時の補助 |
| `message` | | `Notification` / `QuestionPending` 時のメッセージ（codex に Notification イベントは無く、`PermissionRequest` を `on-notification.sh` が `permission_prompt: <tool_name>` へ合成して同形で送出。`QuestionPending` では質問文の要約で、waiting_input 通知の本文になる） |
| `background_tasks` | | `Stop` 時に非空なら中間 Stop＝`Heartbeat` として扱う（#131 D-12） |

`QuestionPending`（#487, [ADR-0050](/decisions/adr-0050-question-tool-asking.md)）はエージェントが対話的な質問ツール（claude `AskUserQuestion` / opencode `question`）のダイアログを開いたことを表す。送出元は claude では PreToolUse フック `on-ask-user-question.sh`、opencode では `totsuka-opencode.js` の `tool.execute.before`。ダイアログ待機中はターンが終わらず `Stop{needs_input}` が届かないため、Engine はこのイベントで task を `waiting_input` へ park する（通知。スロットは保持する → [ADR-0093](/decisions/adr-0093-waiting-holds-slot.md)）。**`prompt_id` は質問ごとに distinct**（claude: `tool_use_id`、opencode: `callID`）でなければならない — 冪等キー `(job_id, tool_session_id, prompt_id, event, status)` の下で、空だと同一セッション 2 問目が Duplicate として黙って落ちる。

# レスポンス

| ステータス | 条件 |
|---|---|
| `200 OK` | 正常。`SignalPort::submit` 直後に即返す（検収等の後続処理は非同期、E-04） |
| `400 Bad Request` | body が不正 JSON / オブジェクトでない / `job_id` 欠落・parse 不能（E-09） |
| `401 Unauthorized` | Bearer トークン不一致・欠落（E-03） |
| `413 Payload Too Large` | body が 1 MiB 超 |
| `503 Service Unavailable` | Engine のイベントチャネルが閉じている（シャットダウン中） |

冪等性はこの層では持たない。重複 POST（多重発火・スプール再送・curl 再送）はいずれも 200 を返し二重投入されるが、`hook_events` の UNIQUE 制約で DB 層が無害化する（D-05）。

`job_id` の形式が正しくても指す `task_id` が DB に存在しない（未知/失効した）場合、受信は 200 を返すが Engine 側（`Engine::on_signal`）は **warn ログに残すだけで `hook_events` へは永続化しない**（`hook_events.task_id` は NOT NULL FK のため物理的に記録不能。これは意図的で、相関できないシグナルは状態を一切変えない E-09）。相関できたシグナルは、重複であっても生存の証跡として `last_signal_at`（R-10 タイムアウト起点）を先に更新してから冪等判定へ進む（中間 Stop=heartbeat が dedup で潰れてもタイムアウト誤判定しないため）。

# 制御エンドポイント POST /focus（F-94, #155）

通知 click-to-focus の制御口。`totsuka focus <task-id>` が同一ソケットへ `{"task_id": 42}`（JSON number または数値文字列）を POST し、Engine が task→最新セッション→agent プラグイン（`pane_control` 宣言時のみ）へ [`session/focus`](/components/plugin-protocol.md) を委譲する（[ADR-0005](/decisions/adr-0005-click-to-focus.md)）。

- **シグナルと違い request-response**: 応答は `200` + JSON body `{"focused": bool, "reason"?: string}`。「フォーカスできなかった」（pane 消失・`pane_control` 非宣言・task 不明・未 dispatch）は **reason 付きの正常応答**でありエラーステータスにしない（タスク終了後のクリックは正常系）。例外は Engine が応答不能（run ループ停止中）の **503** のみ（シグナル受信の submit 失敗時と同じ）。
- 認証（Bearer）・body 上限・1 接続 1 リクエストはシグナル受信と同一。`task_id` 欠落・非整数は 400。
- Engine 側は `PluginEvent::Focus`（oneshot 応答付き）として run ループが処理し、接続の 10 秒 deadline が待ち時間の上限。

# 制御エンドポイント POST /task/cancel・POST /task/retry（#760）

`totsuka task cancel` / `retry` と同じ遷移を、実行中の Engine の run ループの中で適用する制御口（[ADR-0094](/decisions/adr-0094-task-control-endpoints.md)）。body は `/focus` と同じ `{"task_id": 42}`（JSON number または数値文字列）。

- **応答**: `200` + JSON。受け付けたら `{"ok": true, "from": "<遷移前の状態>", "state": "<遷移後の状態>"}`、retry はこれに `"requeued": <再キューしたメッセージ数>`（#242）が付く。受け付けない要求（task 不明・cancel 済みや done の cancel・failed/cancelled/skipped 以外の retry）は **`{"ok": false, "reason": "<次の一手の案内>"}` の正常応答**で、エラーステータスにしない。案内の文言は CLI の直接書き込みと共通（`task_control`）。
- **HTTP ステータス**は認証（401）・形式（400。`task_id` 欠落・非整数）・サイズ（413）・Engine の応答不能（503）だけ。`/focus` と同じ規約。
- **cancel が Engine の中で解放するもの**: そのタスクのスロット・セッション経路・出力バッファ。次のキューのタスクは同じイベントの直後の dispatch で起動できる。**pane は閉じない** — pane の寿命は worktree の cleanup ポリシーに従う（F-107、[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）。
- **retry** は再キューだけ。前回 dispatch の pane は dispatch 側が再 dispatch の前に解放する（#481）。
- 認証（Bearer）・body 上限・1 接続 1 リクエスト・10 秒 deadline は `/focus` と同一。Engine 側は `PluginEvent::TaskControl`（oneshot 応答付き）として処理する。
- CLI（`totsuka task cancel` / `retry`）はまだこのルートを使わず DB を直接書く。ソケット優先への切り替えは後続 PR（#760 の PR 2）。

# Examples

```bash
curl --unix-socket "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock" \
  -H "Authorization: Bearer $TOTSUKA_HOOK_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{"job_id":"job-42-7","session_id":"abc","hook_event_name":"Stop","status":"completed"}' \
  http://localhost/agent-events

# click-to-focus 制御（F-94）
curl --unix-socket "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock" \
  -H "Authorization: Bearer $TOTSUKA_HOOK_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{"task_id":42}' \
  http://localhost/focus

# タスクの cancel（#760）。retry は http://localhost/task/retry
curl --unix-socket "${XDG_RUNTIME_DIR}/totsuka/agent-events.sock" \
  -H "Authorization: Bearer $TOTSUKA_HOOK_TOKEN" \
  -H "Content-Type: application/json" \
  --data '{"task_id":42}' \
  http://localhost/task/cancel
# → {"ok":true,"from":"running","state":"cancelled"}
```

# 関連

- [orchestrator-core クレート](/components/orchestrator-core.md) — `adapters::hook_uds` / `ports::SignalPort` / `adapters::engine_signal_sink`
- [state.db スキーマ](/data/state-db.md) — `hook_events`（冪等 INSERT の受け皿、#134）
- [Spec §6 技術要件](/product/orchestrator-spec.ja.md)
