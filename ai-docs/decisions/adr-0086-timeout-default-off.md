---
type: Decision
title: ADR-0086 timeout_secs の既定を 0（掃引なし）にし、権限プロンプト待ちを無音に数えない
description: "D-03 無音掃引は人間の応答待ちと止まったエージェントを見分けられないため、[[workflows]].timeout_secs の既定を 1800 から 0（掃引なし）へ変え、明示的に上限を書いた workflow でも権限 / idle プロンプト（Notification）待ちの間は掃引を止めることにした決定。"
tags: [decision, timeout, escalation, hooks, adr]
generated: { by: claude-code/opus-5, at: 2026-09-21T13:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[ADR-0042](/decisions/adr-0042-timeout-zero-opt-out.md) の「省略時の既定（30 分）は不変」を置き換える。`0` の意味（掃引の対象外）は ADR-0042 のまま。

# Context

D-03 の無音掃引は、最後のフック信号から `timeout_secs` 秒黙っている `dispatched` / `running` / `publishing` のタスクをエスカレートする。人間待ちのうち、次の 2 つは `WaitingInput` に入るので掃引されない:

- `Stop{NeedsInput}`
- `AskUserQuestion` の `QuestionPending`（design / implement profile のみ）

残る待ちは `Running` のまま数えられ続けていた:

- 権限プロンプトと idle プロンプト（claude の `Notification`、codex の `PermissionRequest`）。R-08 により承認待ちは質問待ちと区別され、slot を持ったまま `Running` に留まる
- 上記以外の profile での `AskUserQuestion`

herdr / orca プラグインは pane の `blocked` / `permission` を `WaitingInput` に写す関数を持っている。しかし使うのは `session/attach`（再起動時の再接続）だけで、実行中の状態ストリームはプロセス終了を検知する deadman に縮退している。どちらのプラグインでも、実行中の人間待ちは core のフック経路でしか分からない。

# Decision

1. **`timeout_secs` の既定を `0`（掃引なし）にする。** 上限が要る無人 workflow は値を明示的に書く
2. **権限 / idle プロンプト待ちの間は掃引を止める。** 最後の信号が `Notification` だったタスクは掃引の対象外にし、他の種類の信号が 1 つ来たら再び対象に戻す。R-08 は変えない（状態は `Running` のまま、slot も解放しない）

# Consequences

- 上限を書いていない workflow では、止まったエージェントが自動では検知されなくなる。無人運用（Slack 系など）には `timeout_secs` を明示的に書くこと
- この判定はエンジンのメモリ上にしか持たない（`Engine.awaiting_approval`）。プロンプト待ちの途中で `totsuka run` を再起動すると判定が消え、以前と同じく `last_signal_at` から数え直す。問題になったら `last_signal_at` の隣に永続化する
- プロンプトに答えたあと次の信号が届くまでの作業時間も掃引の対象外になる。claude が作業途中に出すフック信号は少なく、承認の瞬間そのものを捉える信号は無い。そのため、ここが無音検知の限界になる
- `hook_events` の最新行は判定に使わない。重複配信は記録されずに捨てられるので、プロンプト後の heartbeat が重複だった場合に最新行が `notification` のまま残るため
- フラグを立てるのは**新規の** `Notification` だけで、外すのは重複を含むすべての非 `Notification` 信号である。再送された古いプロンプトは、エージェントが先へ進んだ後にフラグを立て直せない。それでも、spool から遅れて再生された初回配信のプロンプトだけは区別できない。その場合も次の信号 1 つで解除される

# 不採用案

- **権限待ちも `WaitingInput` へ遷移させる**: R-08（承認待ちと質問待ちの区別）を覆し、slot の解放と再取得まで絡む。タイマーを止めたいだけならやりすぎである
- **herdr / orca の pane 状態を購読する**: 完了検知をフックへ移したときに外した経路（[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)）を、判定 1 つのために戻すことになる。orca 側には待ち状態を報告する信頼できる経路が無い
- **`tasks` に最後の信号種別の列を足す**: 再起動をまたいで判定を保てるが、スキーマ移行が要る。上記の限界が実害になるまで見送る
