---
type: Decision
title: ADR-0094 task cancel / retry を実行中の Engine へ届ける制御ルートを hook UDS に足す
description: totsuka task cancel / retry は DB を直接書き、実行中の Engine はそれを通知されない。既存の hook UDS（/focus と同じソケット・同じ Bearer）に POST /task/cancel と /task/retry を足し、Engine が run ループの中で遷移とスロット等の解放を行う決定。受け付けない要求は 200 + ok:false で返す。cancel で pane は閉じない（ADR-0010）。DB 直接書き込みはフォールバックとして残し、絞る段階は作らない。
resource: https://github.com/tomoya-k31/totsuka/issues/760
tags: [decision, uds, control, cancel, retry, engine, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-23T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-760
    resource: https://github.com/tomoya-k31/totsuka/issues/760
    title: "Issue #760 と詳細設計コメント"
---

# Status

stable（[#760](https://github.com/tomoya-k31/totsuka/issues/760)）。2 段で入れる。PR 1 = ルート・Engine 側・規則の共通化（本 ADR と同じ PR）、PR 2 = CLI をソケット優先に切り替える。[ADR-0005](/decisions/adr-0005-click-to-focus.md) が `/focus` で決めた「制御は実行中 Orchestrator の UDS を通す」を cancel / retry に広げる。

# Context

状態 DB の書き込み元が 2 つある。`totsuka run` の Engine と、`totsuka task cancel` / `retry` を実行する CLI プロセスである。CLI は DB を直接書き、実行中の Engine には知らせない。

- 実行中のタスクを CLI で cancel すると、Engine が保持しているスロット・セッション経路・出力バッファは、次の cycle の `release_slots_of_settled_tasks`（[ADR-0093](/decisions/adr-0093-waiting-holds-slot.md)）まで残る。それ以前は再起動まで残っていた
- #481（cancel 直後の retry が生きた pane に衝突する）は dispatch 側の防御で塞がっているが、書き込み元が 2 つある構造は残っている
- メニューバーアプリなど、外から `run` を監督する側（#753〜#756）が使える制御経路が要る

調べて分かった事実:

- `run` は `--dry-run` でないかぎり、`[hooks]` が無くてもデフォルトのパスで hook UDS を bind する。「Engine は生きているがソケットが無い」は、bind に失敗したとき（`health.json` の `hook_receiver_down`）だけ
- pane の寿命は worktree の cleanup ポリシーに従う（F-107、[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）。既定の `manual` では意図して閉じない

# Decision

1. **制御ルートは既存の hook UDS に同居させる。** 新しいソケットは作らない。完全一致の `/task/cancel` と `/task/retry` を足し、それ以外のパスは従来どおりシグナル受信（E-08）。ソケットは `[hooks]` が無くても立つので、独立させる理由が無い
2. **認証は `/focus` と同じ。** Bearer（`[hooks].auth_token_ref`）と 0600。トークン未設定なら Bearer なしで受け付ける振る舞いも同じ
3. **応答の規約は `/focus` に揃える。** 受け付けたら `200` + `{"ok": true, "from", "state", "requeued"?}`。受け付けない要求（未知の id、終わったタスクの cancel、retry できない状態）は `200` + `{"ok": false, "reason"}`。HTTP ステータスは認証・形式・サイズ・Engine の応答不能（503）だけに使う。これで、新しい Engine が 400 を返すのは body が壊れているときだけになり、古い `run`（制御パスを知らずシグナルとして解釈し、`job_id` が無いので 400 を返す）の判別に使える
4. **規則は 1 か所に置く（`task_control`）。** 何を断り、どう案内するかは、Engine と CLI の直接書き込みで同じでなければならない。遷移そのものは `domain::state::transition`、SQL は `adapters::state_db` のまま
5. **Engine は run ループの中で適用する**（`PluginEvent::TaskControl`）。dispatch やシグナル処理と直列なので競合しない。適用した cancel では、スロット・セッション経路・出力バッファをその場で解放する。直後の `dispatch_ready` で次のタスクが起動できる。retry は再キューだけで、前回の pane の解放は dispatch 側の防御（#481）に任せる
6. **cancel で pane は閉じない。** ADR-0010 / F-107 に従う。cancel を F-107 の例外にするのは、この決定の目的（書き込み元を 1 つにする）から外れる。cancel したエージェントが pane の中で動き続けるのは従来どおりで、止める仕組みは別の課題
7. **DB 直接書き込みはフォールバックとして残し、絞る段階（contract）は作らない。** `run` の停止中・bind 失敗・古い `run` では直接書くしかない。CLI の方針（PR 2）:
   - ソケットに接続できない → run lock を見て、`run` が生きていなければ黙って、生きていれば警告を出して直接書く
   - 古い `run` が 400 を返した → 警告を出して直接書く（400 は submit の前に弾かれるので二重適用にならない）
   - **送った後の失敗（タイムアウト・503）では直接書かない。** 二重の cancel や、retry でのメッセージの二重キューを避ける
   - ソケット経由で成功したときの表示は今と同じ（経路は出さない）

# 代替案と不採用理由

| 案 | 不採用理由 |
|---|---|
| 制御専用のソケットを新設し `[hooks]` から独立させる | 既に `[hooks]` と無関係に bind しているので、得るものが無い。ソケットと認証の方式が 2 つになる |
| `/control` 1 本で `{"op": ...}` を振り分ける | `/focus` と流儀が分かれる。パスで分けても完全一致の比較が 2 つ増えるだけ |
| 受け付けない要求を 409 などで返す | `/focus` の「縮退は正常応答」と揃わない。400 を古い `run` の判別に使えなくなる |
| cancel で pane をすぐ閉じる | F-107 / ADR-0010 の決定（`manual` では pane を確認の場として残す）に反する |
| 実行中は DB 直接書き込みを拒否する | bind に失敗した `run` では再起動しないと cancel できなくなる |
| ポートを `FocusPort` と別に足す | 実装は 1 つ（`EngineSignalSink`）しかなく、分けても得るものが無い。`FocusPort` を `ControlPort` に改名して広げた |

# Consequences

- `ports::FocusPort` は `ports::ControlPort` になり、`task(TaskOp, task_id)` が増えた。`hook_uds::serve` の引数の数は変わらない
- 制御パスは 3 本になった（`/focus`・`/task/cancel`・`/task/retry`）。フックスクリプトが送るパスは変わらない
- PR 1 の時点では CLI の振る舞いは変わらない（規則を `task_control` へ移しただけ）。ソケット経由になるのは PR 2 から
- `task verify` はまだ DB を直接書く。同じルートの形（`/task/<op>` + `{"task_id"}`）に載せられるが、移すのは別の課題
- #754（`secret:` スキーム）が入ると、CLI 単体で `[hooks].auth_token_ref` を解決できなくなる可能性がある。`totsuka focus` と同じ問題なので、そちらで一緒に決める
- 検証: `run::hooks` の `a_control_cancel_frees_the_slot_and_session_routes_at_once`（Engine 内の解放を外すと落ちる）、`hook_uds` の `task_routes_*` / `a_refused_task_request_is_still_200_with_its_reason` / `non_control_paths_stay_signal_ingestion`、`task_control` の単体テスト
