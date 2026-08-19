---
type: Decision
title: ADR-0043 design / implement の完了は人間が pane 上で承認し、マーカーは「確認依頼のプロトコル」になる
description: "attended pane 前提の design / implement profile で、エージェントは完了と思ったら NEEDS_INPUT で人間に確認を求め、pane 上の明示承認後にのみ COMPLETED を出す決定。実装はプロンプト既定の profile 分岐（marker_self_report_confirm）と llm 検収 rubric の承認検査への差し替え（verification_rubric_human_approval）のみで、状態機械の改修はゼロ。マーカー自体を消す案と verification = human + CLI 案を退けた理由も記録する。"
resource: https://github.com/tomoya-k31/totsuka/issues/440
tags: [decision, profile, marker, verification, attended-pane, prompts, adr]
generated: { by: claude-code/fable-5, at: 2026-08-13T17:50:00+09:00 }
status: stable
verified:
  - { by: claude-code/fable-5, at: 2026-08-13T17:50:00+09:00 }
  - { by: human:tomoya-k31, at: 2026-08-18T21:56:00Z }
owner: tomoya-k31
sources:
  - id: issue-440
    resource: https://github.com/tomoya-k31/totsuka/issues/440
    title: "feat(profile): design/implement の完了判断を人間の pane 上承認に移す"
  - id: issue-439
    resource: https://github.com/tomoya-k31/totsuka/issues/439
    title: "feat(core): timeout_secs = 0 で D-03 無音掃引を無効化できるようにする"
---

# Status

stable（[#440](https://github.com/tomoya-k31/totsuka/issues/440)）。[ADR-0042](/decisions/adr-0042-timeout-zero-opt-out.md)（`timeout_secs = 0`）とセットで attended pane 運用が完成する。

# Context

Slack 系（answer / triage）は無人前提で、totsuka が llm 検収で完了を確定する現行の設計が合っている。一方 design / implement は attended pane（人間が pane を見ている、離席しても戻って確認する）前提で運用され、**完了の最終判断は人間が行うべき**である。「エージェントに任せて最終的に COMPLETED が来ればよいのでは（マーカー不要では）」という問いから調査した。

調査で確定した事実:

- **COMPLETED は終端への唯一の入口**である。来ないタスクは `running` のまま不死身になる — 並列 slot を握り続け、worktree / pane の掃除は走らず、`on_success`（issue のステータス遷移）は発火せず、手で閉じる CLI は `tt task verify`（`verifying` 限定）しかない
- マーカーを支える機構は 4 箇所すべて無条件: プロンプト注入・`on-stop.sh`（verification モードに依らず常時注入、R-03 のマーカー欠落ブロックも無条件）・D-02（UNKNOWN 連続エスカレート）・D-03（無音掃引）
- 一方、**フロー実現に必要な状態機械はすべて既存**: `WaitingInput` は D-03 掃引対象外（「人間待ちは沈黙ではない」）・slot 解放（F-45）・notifier 通知が既にあり、`WaitingInput` からの COMPLETED も受理される（`ResumeInput + BeginPublish`）

# Decision

**マーカーは消さず、design / implement では意味を「totsuka が完了判定する信号」から「人間への確認依頼のプロトコル」に変える。** 実装は 2 つのプロンプト既定の差し替えのみで、状態機械の改修はゼロ:

1. **`marker_self_report_confirm`**（`prompts/defaults.toml` の新キー）: design / implement profile の完了自己申告指示の既定。作業を終えたと思ったら COMPLETED を自分の判断で出さず、内容を要約して確認を求め `NEEDS_INPUT reason="awaiting completion confirmation"` で停止する（**この reason は #465 の英語化まで `"完了確認待ち"` だった**。ワイヤ上でパースはされないが、`WaitingInput` 通知として運用者の目に届く文字列なので変更の影響は Slack 通知に出る）。COMPLETED は人間が会話上で明示承認した後にのみ出す。基底テキストが教えるもの（3 マーカー・ハートビート例外・配信契約）はすべて保持し、`missing_markers` 検証も同じ経路で効く
2. **`verification_rubric_human_approval`**（同・新キー）: design / implement の llm 検収 rubric の既定。「この完了申告より前の会話で人間が明示的に承認しているか」を条件にする。ジャッジはセッション内で会話を見られるので、**確認を飛ばした COMPLETED はマーカー欠落を止めるのと同じ層で機械的にブロックされる**。triage は従来どおり成果物 URL 検収（#398）のまま

配置は #398 の `verification_rubric_artifact_url` と同型: `[prompts]` キーではなく**既定の差し替え**であり、優先順位の梯子に新しい規則を足さない。当時の梯子は workflow prompts > workflow rubric > グローバル `[prompts]` > profile 既定 > 汎用既定で、グローバル上書きが profile 既定に勝つギャップが #398 と同じ形で存在した。**[#465](https://github.com/tomoya-k31/totsuka/issues/465) が上書き面を削除してそのギャップを塞ぎ**、梯子は workflow の `rubric` > profile 既定 > 汎用既定の 3 段になった（[ADR-0023 の Amendment](/decisions/adr-0023-configurable-prompt-surface.md)）。

適用は **profile のみ**。spelled-out 記法（`mode = "implement"` 手書き）は無変更 — #420 の permissions と同じ線引きで、既存 config の挙動をアップグレードで黙って変えない。

# フロー

```text
agent: 設計書を書き終えました。確認をお願いします
       <<STATUS:NEEDS_INPUT reason="awaiting completion confirmation">>
  → totsuka: WaitingInput に park（D-03 対象外・slot 解放・通知 — すべて既存動作）
human: (pane 上で) OK、完了で
agent: <<STATUS:COMPLETED>>
  → totsuka: 終端処理（on_success 等）
```

確認依頼の停止自体は NEEDS_INPUT なので non-claim 枝（#389）を満たし、ジャッジにブロックされない。自走中の無音誤エスカレートは `timeout_secs = 0`（[ADR-0042](/decisions/adr-0042-timeout-zero-opt-out.md)）で塞ぐ。

# Consequences

- design / implement の COMPLETED の意味が「人間が承認した」に変わる。llm ジャッジは完了の判定者ではなく、**確認プロトコルが守られたことの検査者**になる
- 既知の制限（スコープ外）: `WaitingInput` 中の 2 回目の NEEDS_INPUT（修正指示 → 再確認）は冪等 no-op で**再通知が飛ばない**。attended pane では人間が会話の当事者なので実害は小さいと判断
- エージェントの自走度（確認を求める前にどこまで進めるか）は各リポジトリの AGENTS.md / CLAUDE.md の責務で、totsuka のスコープ外
- ~~グローバル `[prompts].marker_self_report` を設定済みの構成は確認プロトコル版にならない（#398 と同じ documented gap）~~ → [#465](https://github.com/tomoya-k31/totsuka/issues/465) がそのキーごと削除して解消した

# 不採用案

- **`verification = "human"` + `tt task verify` CLI**: 既存機構だが、完了判断が pane の外（別ターミナルの CLI）に出る。要件は「pane の会話内で確認」。加えて `Verifying` は slot を保持し続け（`scheduler` の F-45 規則に明記）、一晩放置すると (repo, agent) の並列枠を握る。profile と `verification` の併記は validation が拒否するため、profile を崩す改修も要る
- **完全マーカーレス + 人間クローズ**: マーカー注入・R-03・D-02 / D-03 を workflow 単位で全部無効化し、終端は `tt task done` 新コマンドか SessionEnd で入れる案。改修 6 点 + 新しい状態遷移の設計が必要で、「COMPLETED が来ないタスクの不死身問題」を全部手当てし直すことになる。本決定が同じ到達点（人間が終端を握る）を既存の状態機械で実現するため割に合わない
- **design / implement にも成果物 URL 検収を併存**: 人間が成果物を見て承認しているのに URL を要求し直すのは二重検収で、承認済みの停止をジャッジが別基準で覆す矛盾（グリルで排除した「現行ルーブリックのまま」案の症状）を再導入する

# 検証

実機検収（2026-08-13）:

`github-design`（profile = design、`timeout_secs = 0`、herdr 隔離セッション → claude、Claude Code の実セッション）で一周を実測した:

1. dispatch されたエージェントは設計を issue コメントへ投稿し（URL 実在を `gh issue view` で確認）、**確認を求めて `<<STATUS:NEEDS_INPUT reason="完了確認待ち">>` で停止**した — `marker_self_report_confirm` の教示どおり。タスクは `waiting_input` に park（この reason は当時の文言そのままで、**検収記録なので書き換えない**。現行は `"awaiting completion confirmation"`）
2. pane へ人間の承認（`herdr agent prompt`）を送ると、エージェントが `COMPLETED` を出し、**ジャッジ（承認 rubric）を通過**して `done` に到達。イベント列は `session_start → NEEDS_INPUT → 通知 → COMPLETED → session_end`
3. `on_success` の書き戻し（`Design Review`、F-84）・pane 解放・worktree 削除まで確認
4. レンダリングされた `orchestrator-github-design.json` に承認 rubric・`defaultMode: auto`・deny 18 件が入っていることを直接確認

**profile の線引きも実機で分離して確認した（2026-08-14、#447）。** 同じビルド・同じ `tt run` で Slack の `slack-reply`（profile = `answer`）を回したところ、エージェントは **`NEEDS_INPUT` を経ず直接 `COMPLETED`** を出し（フック列は `session_start → COMPLETED → session_end`）、24 秒で承認フローの下書き作成まで到達した。design が park し answer が park しないことが、**同一条件下で分離して観測できた** — この決定が profile 限定であることは、ユニットテストだけでなく実機でも成り立っている。

**未検証**: 確認を飛ばした COMPLETED をジャッジがブロックする負のパスは実機では測っていない。人間役が送るプロンプト自体が「人間の発言」になるため、承認なしの完了申告を汚染なしに誘発する刺激が存在しない。この経路の根拠は、条件文としてのジャッジ挙動の実測（#389）と rubric の文面に依る。
