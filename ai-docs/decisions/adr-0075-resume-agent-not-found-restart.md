---
type: Decision
title: ADR-0075 resume 付き dispatch でも `agent_not_found` で `agent.start` を再発行する
description: "追いメンションのたびにエージェントが前の会話を失っていた実機バグ（#685）に対し、resume 付き dispatch だけを agent.start 再発行の対象から外していた #261 の判断を撤回する決定。SESSION_UNRESUMABLE の写像は残し、再試行を使い切った後の最終手段へ後退させる。プロンプト二重送信のガードは PromptFailure 型で分離する。"
tags: [herdr, resume, conversation, dispatch, agent-ide]
generated: { by: claude-code/opus-5, at: 2026-09-15T16:10:00+09:00 }
verified: { by: claude-code/opus-5, at: 2026-09-15T16:10:00+09:00 }
status: stable
sources:
  - id: ref-1
    resource: https://github.com/tomoya-k31/totsuka/issues/685
    title: "実機観測と測定 — #685"
  - id: ref-2
    resource: /decisions/adr-0015-conversation-task-identity.md
    title: "SESSION_UNRESUMABLE を定義した決定 — ADR-0015"
  - id: ref-3
    resource: /components/agent-ide-herdr.md
    title: "再送判定の現行仕様 — agent-ide-herdr プラグイン"
---

# Status

Accepted — 2026-09-15（[#685](https://github.com/tomoya-k31/totsuka/issues/685)）

[ADR-0015](/decisions/adr-0015-conversation-task-identity.md) 決定 5 の `SESSION_UNRESUMABLE` 写像そのものは**維持する**。変えるのはその発火タイミングだけである。同 ADR の Context にある「`--resume` に失敗した pane は即死し」という記述は、下の実測により**推論として誤りだった**ことが判明している（同 ADR の診断＝cwd 不一致は正しい）。

# Context

追いメンションで会話が継続していなかった。1 つの Slack スレッドに 3 回メンションした実機結果は、タスクと worktree は 1 つに集約されている一方、**Claude セッションは 3 つで相互の共有メッセージが 0 件**というものだった。SessionStart フックの `source` は全件 `startup` で、`resume` は 1 件も無い。

ログには毎回これが出ていた。

```text
WARN orchestrator_core::run::dispatch: session could not be resumed
  (herdr error (agent_not_found): agent target wK:p1 not found);
  dispatching fresh — the agent starts without the earlier conversation
```

**再 dispatch 7 件中 7 件**（task 6・7・10）。`--resume` は argv に載っており、`resume_session_id` の決定は正しい。herdr が `agent_not_found` を返し、プラグインがそれを `SESSION_UNRESUMABLE` へ写像して core が fresh へ落としていた。

この写像は #261 の意図的な判断で、根拠は「resume 付き dispatch の `agent_not_found` は、pane がセッションごと死んだ形である」だった。**2026-09-15 の実測で、その根拠の両半分が否定された。**

| 実測 | 結果 |
|---|---|
| 7 件の `tool_session_id` の実在確認 | すべて `~/.claude/projects/<worktree の slug>/<uuid>.jsonl` に実在。task 10 の 3 件は**同一 project dir** にあり、worktree パスが 3 回とも一致していたことの証明でもある |
| 有効な id での `claude --resume`（実セッション 378 KB、trusted リポジトリの git worktree 上） | **会話が完全復元し、5 秒以内に入力可能、20 秒後も生存**。herdr が諦めた 18 秒は起動時間では説明がつかない |
| 無効な id での `claude --resume` | `No conversation found with session ID: …` を出して約 1 秒で `claude` だけ終了。**pane は生存し、シェルのプロンプトが戻る** |
| トラストダイアログの影響 | 素の `mkdir` と独立 `git init` では出るが、**trusted リポジトリの `git worktree` では出ない**。実 worktree はこちらなので無関係（`--resume` の有無とも無関係） |

つまり `agent_not_found` は「pane が死んだ」ではなく「**herdr がそのターゲットにエージェントを登録していない**」である。これは fresh dispatch で `agent.start` の再発行によって解消することが実測済みの、シェル起動レース（#387 / #391）と同じ形をしている。resume だけが、その再試行から 1 度も通らずに会話を捨てていた。

# Decision

## 1. 再発行の対象から resume を外さない

`prompt_means_the_cli_never_started` から `resume_session_id.is_none()` の条件を削除する。resume 付き dispatch も fresh と同じ再試行予算（初回 + `MAX_AGENT_RESTARTS` = 計 4 回の `agent.start`）を受け取る。

## 2. `SESSION_UNRESUMABLE` は最終手段として残す

写像自体は消さない。再試行を**使い切ってから**発火するよう後退させる。Orchestrator の「`resume_session_id` なしで 1 回だけ再送」という契約（ADR-0015 決定 5）は一切変わらず、変わるのは「最初の拒否で諦めるか、4 回試してから諦めるか」だけである。セッションが本当に消えている場合の復帰経路は保たれる。

## 3. プロンプト二重送信のガードは型で分離する

**これが実装上の要点である。** 条件を素朴に外すと `confirm_submission` 経路まで巻き込む。この経路は `agent_prompt_stalled` の後、つまり **herdr が本文を既にタイプして送信済み**の地点で、ここから `agent.start` を再発行するとタスクが二重に届く（#380 が防いでいるもの）。エラーコードではこの違いを表せない —— 同じ `agent_not_found` が、`agent.prompt` からなら「何もタイプされていない」、`agent.wait` からなら「もう入っているかもしれない」を意味する。

そこで判定を発生源へ移し、型で持たせる。

```rust
enum PromptFailure {
    /// 何もタイプされていない。再発行は安全。
    NeverStarted(HerdrError),
    /// プロンプトが既に届いている可能性がある、または再発行で直らない。
    Final(HerdrError),
}
```

`confirm_submission` が返すものは**常に `Final`**。`start_agent` は `NeverStarted` のときだけ再発行する。

## 不採用案

| 案 | 理由 |
|---|---|
| (A) 何もせず用語集に「セッションは共有されない」と注記する | 会話継続はこのツールの中核機能で、追いメンションのたびに文脈が失われる状態を仕様として固定することになる |
| (B) `agent_not_found` を `is_agent_not_ready` と同じ時間ベースの待機に入れる | この拒否は即座に返るので、時間ベースだと 180 秒の予算を使い切るまで CLI を再起動し続ける（#391 が回数ベースにした理由そのもの） |
| (C) `confirm_submission` 側にも再発行を許す | プロンプトが既に送信済みの地点なので、タスクが二重に届く。#380 が明示的に禁じている |
| (D) core 側で `SESSION_UNRESUMABLE` を受けた後に resume 付きで数回リトライする | 再試行はレースの起きた層で閉じるのが安い。core まで往復すると workspace の作り直しを伴い、ツール固有の事情（#196 で core から追い出したもの）が core に戻る |

# Consequences

- **追いメンションが前の会話を引き継ぐ**。7/7 で失われていた文脈が、レース由来の拒否である限り保たれる。
- **本当に復元不能なセッションの復帰は 3 回分遅くなる**。`agent_not_found` は即座に返るため実時間の増加は小さいが、`agent.start` が 4 回走る。
- **失敗の分類が型に載った**。「再発行が安全か」は今後エラーコードから推論されず、`PromptFailure` を返す側が宣言する。`confirm_submission` に新しい失敗経路が増えても、既定で `Final` に入る。
- **ADR-0015 の Context にある前提が 1 つ訂正された**。同 ADR の決定は有効なまま、`# Status` に本 ADR への参照を足した。
- 実機での最終確認（herdr がなぜ登録に失敗するのか、レースの実体）は #685 に残る。本 ADR はその実体を問わず、**誤分類によって会話を捨てないこと**だけを決めている。
