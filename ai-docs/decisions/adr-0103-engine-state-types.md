---
type: Decision
title: ADR-0103 Engine の状態は、不変条件を持つものだけを型へ取り出す
description: "run::Engine の 28 フィールドを責務ごとの型へ分けるとき、取り出すのは自分で守る不変条件を持つものだけにし、その型はプラグイン・DB・git を呼ばないと決めた。不変条件の無い集合（待ち状態のメモ、セッションの宛先表）は Engine のフィールドのまま残す。"
resource: https://github.com/tomoya-k31/totsuka/issues/758
tags: [decision, core, run, engine, refactor, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T20:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#758）。実装は 4 つの PR を積み重ねて行う（`SlotManager` → `LlmMonitor` → 監督の帳簿 → `dispatch_one` の分割）。

# Context

`run::Engine` はフィールド 28 個・メソッド約 82 個を 1 つの `&mut self` で共有している。ファイルは 9 つに分かれているが、どのメソッドもどのフィールドにも触れるので、変更の影響範囲を型から読み取れない。

issue の原案は、フィールドを 4 つのまとまり（スロット台帳・待ち状態の帳簿・プラグイン監督・LLM の生存確認）に分け、それぞれを型にするものだった。調べると、不変条件を本当に持つのはその一部だけだった。

- `slots` と `slot_holders`: 「取得した分だけを解放する」という不変条件を、型の外（Engine）で 2 つのフィールドを揃えて守っていた。再起動時の復元も、使用量と持ち主を別々のループで作っていた
- `sessions`: 通知の宛先表。スロットとは寿命も消し方も違う（プラグイン単位でも消す）
- 待ち状態の 4 つの集合（`blocked_on_tools` / `blocked_on_agent` / `awaiting_approval` / `released_panes`）: それぞれ 1 ファイルの中でしか使われず、4 つにまたがる不変条件は無い
- `llm` と `llm_health` は「両方 Some か両方 None」だが、型で表していない
- プラグイン監督の 4 フィールドは、I/O を伴わない帳簿として使われている

# Decision

1. **取り出すかどうかの基準は、守るべき不変条件があるかどうか。** 無ければ Engine のフィールドのまま置く。包むだけの型は影響範囲を狭めない
2. **取り出した型は、プラグイン・DB・git を呼ばない。** 呼び出しの順序と失敗処理は Engine（調停役）に残す。型は判断と帳簿だけを持ち、プロセスを立てずに単体テストできる。「I/O を持たない」ではなくこの文言にするのは、`LlmMonitor` が LLM へのプローブを spawn するから（classifier は `RepoClassifier` port の向こうにあり、テストではフェイクに差し替えられる）
3. **新しい型を足すより、既存の型を深くする。** スロットの持ち主は新しい台帳型を作らず、`scheduler::SlotManager` に吸収した（`acquire(task_id, repo, agent)` / `release(task_id)`）。解放はタスク ID でしか指定できないので、取得しなかったタスクの解放・二重解放・ペアの取り違えは型の上で起きない。`rebuild` は `(task_id, repo, agent)` の 1 本のリストから使用量と持ち主を同時に作る
4. **構造を変える PR と振る舞いを変える PR は混ぜない。** 検査は「既存のテスト名の集合が変更後の集合に含まれる（削除も改名も無い）こと」と、`run_loop` / `hook_integration` / `plugin_supervision` の統合テストが緑のままであること。「テスト数が一致すること」は、型の単体テストを同じ PR で足す方針と両立しないので採らない

取り出す型と取り出さないもの:

| 対象 | 扱い |
|---|---|
| `slots` + `slot_holders` | `SlotManager` に統合（#758 の 1 つ目の PR） |
| `llm` + `llm_health` + `llm_probe` | `Option<LlmMonitor>` にまとめる。サスペンド検知の `last_cycle_clock` は Engine に残す（LLM 以外の反応も足しうる、エンジン全体の関心事） |
| `restarts` + `abandoned_plugins` + `retired_stats` + `plugin_events` | 監督の帳簿にまとめる。再起動・通知は Engine に残す |
| `sessions` | 残す（不変条件が無い） |
| 待ち状態の 4 つの集合 | 残す（互いに共有する不変条件が無い） |

# Alternatives considered

- **issue 原案どおり 4 つのまとまりをすべて型にする**: 却下。待ち状態の集合や `sessions` は、包んでもメソッドがそのまま外に並ぶだけで、影響範囲は狭まらない。「タスクが終わったら全部忘れる」を不変条件にすれば 1 つの型にする意味が出るが、それは振る舞いの変更なので、構造を変える作業には混ぜない
- **スロットの持ち主を新しい台帳型（`SlotLedger` など）にする**: 却下。`SlotManager` の中に置けば、ペアごとのカウント（`pair_used`）が持ち主の表で置き換わり、コードが減る。`SlotManager` を使っているのは `orchestrator-core` の中だけなので、公開 IF を変えても外への影響は無い
- **取り出した型を #757 の型付きクライアントのフェイクでテストする**: 不要になった。型がプラグインを呼ばないので、フェイクなしで単体テストできる

# Consequences

- Engine に状態を足すときは、まず「守る不変条件があるか」を問う。あるなら既存の型を深くするか新しい型を作り、その型の単体テストで性質を確かめる
- `SlotManager` の使用量の合計と持ち主の数は、構造上一致する（`global_used == Σ repo_used == Σ agent_used == 持ち主の数`）
- 上限を超えて resume したタスク（持ち主にならなかったもの）が完了しても、他のタスクのスロットは減らない。これは以前から Engine が 2 つのフィールドを揃えて守っていた性質で、今は型が保証する
