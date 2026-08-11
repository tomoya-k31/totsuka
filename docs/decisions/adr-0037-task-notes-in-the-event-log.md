---
type: Decision
title: ADR-0037 動かない理由は events の「ノート行」として記録し、状態が動いた瞬間に自動で解消させる
description: "タスクが `Queued` のまま動かない理由（#399 の外部ツール未整備）を `totsuka status` から後追いできるようにするにあたり、`tasks` の列ではなく `events` の非遷移行として記録することを選んだ理由と、その結果として何が保証されるか。"
tags: [state, cli, observability, 407]
status: stable
generated: { by: claude-code/opus-5, at: 2026-08-11T00:00:00Z }
---

# Context

[#399](https://github.com/tomoya-k31/totsuka/issues/399)（[ADR-0033](/decisions/adr-0033-workflow-profile.md) D9）で、`profile = "implement"` のタスクは `gh` が Orchestrator のプロセスから見えないと dispatch されず `Queued` のまま待機するようになった。通知は一度出る。

**通知は流れて消える。** 30 分後に「なぜこのタスクは動いていないのか」を調べる人にとって、`totsuka status` が何も言わないのは、#399 が防ごうとした「誰かが見るまで気づかない」の別の形でしかない。

要件は 4 つ:

1. 待機中のタスクに理由が出る
2. 解消したら消える
3. dispatch ループは 200ms ごとに同じ判定に到達するので、**記録が増え続けない**
4. この検査は偽陰性を出しうる（#399）ので、その導線が要る

# Decision

## D1 — 読むときに計算せず、Orchestrator が書いたものを読む

`totsuka status` はプロセスとして**オペレータのシェルで走る**。`.zshenv` と mise が効いているので、そこでは `gh` が PATH にある可能性が高い — Orchestrator のプロセスから見えていなくても、である。

つまり status が同じ検査を実行時に走らせると、**「このタスクは待機していません」と答えながら、実際にはその判定を下したプロセスが dispatch を拒み続ける**。偽陰性が 1 つ増えるのではなく、症状と説明が正反対になる。

だから記録する。書くのは判定を下した側だけである。

## D2 — 記録先は `events` テーブルの「ノート行」

`from_state == to_state` の行を `events` に 1 本入れ、`detail` に何が起きているかを書く。

```json
{ "note": "blocked_agent_tools", "missing": ["gh"] }
```

**採用理由は「自分で解消する」ことに尽きる。** 状態遷移は必ず event を書く（F-72）。したがってタスクが動いた瞬間 — dispatch でも cancel でも fail でも — ノートは「そのタスクの最新の event」ではなくなり、読み出し側から自動的に消える。解消イベントを書く必要すらない。

`tasks` に `blocked_reason` 列を足す案を採らなかったのは、その列を **`Queued` から出る全経路で消して回らなければならない**ためである。消し忘れた 1 経路が「永久に嘘をつく `totsuka status`」になる。これは将来の変更で増える種類の負債で、今書く 1 行では終わらない。

### 「ノートである」ことの見分け方

`detail.note` キーの**存在**で判定する。既存の候補を 2 つ落とした:

| 案 | 落とした理由 |
|---|---|
| 既存の `detail.kind` の値で判別 | `kind` は `ingested` / `dispatch` / `publish` など**全遷移**が既に使っている。ノート専用の値を足しても、`kind` を見る側が「これは遷移である」という前提で書かれている |
| `from_state == to_state` で判別 | `Escalated → Escalated` は**正当な遷移**（`(s, E::Escalate) if !s.is_terminal()`）。自己遷移をノートの印にすると、それをノートとして読んでしまう |

### dedup はタスクの履歴に対して行う

「同じノートを既に書いたか」は、**そのタスクの最新 event が同一の `detail` か**で判定する。呼び出し側のメモリではない。

これは意図的に、**通知の dedup（`Engine` 内の `HashSet`）とは別の問い**にしてある:

- **記録**は再起動を跨いで重複してはいけない。同じ待機が 2 行になる意味がない
- **通知**は再起動後にもう一度出てよい。オペレータは最初の通知を見ていないかもしれない

`detail` が変わったら（`missing` が増えた等）新しい行を書く。dedup は kind ではなく `detail` 全体の一致で判定するので、これは黙って捨てられない。

## D3 — 待機が終わったら「もう言った」記憶も終わる

`Engine.blocked_on_tools`（通知 dedup の `HashSet`）は、これまで**一度入ったら消えなかった**。dispatch できるようになった時点で `remove` する。

ブロック → dispatch → retry → 再びブロック、は**オペレータがまだ知らない新しい状況**である。消さないと 2 回目の待機は通知でもノートでも無言になる。

## D4 — 文面は記録せず、読むときに組み立てる

ノートが持つのは構造（`missing: ["gh"]`）だけで、オペレータに見せる文章は `agent_tools::blocked_reason` が読み出し時に組み立てる。#399 の通知も同じ関数を使う。

- 通知と `status` が**remedy と偽陰性の但し書きについて食い違えない**。この但し書きは、検査が fail ではなく skip である理由そのものなので、片方から落ちるのが一番まずい
- 古いノートを新しいバイナリが**現在の文言で**説明する

このバイナリが知らないツール名も行として出す。remedy だけ省く。名前ごと落とすと「理由なくブロックされている」と読める。

## D5 — 表示は表の列ではなく独立ブロック

理由は remedy を含む一文で、表の最終列は source が決めるタイトルである。列を足すと長さが噛み合わない。

```text
not starting yet:
  task 12 (2026-08-11T09:00:00Z): gh unavailable in the orchestrator's environment → …
```

`--json` は `tasks[].wait_reason = { kind, since, message }`。`kind` を構造として残すのは、散文をパースさせないため。無い場合は**キーごと省く**（`null` ではない）。

# Consequences

## 良くなること

- 「なぜ動いていない」に通知が消えた後も答えられる
- 記録が「動いた瞬間に無効になる」ので、**status が古い理由を表示し続ける経路が存在しない**
- 「タスクが動かない理由」一般に使える形になった。次の kind を足すのは `detail` と `status` の 1 分岐だけで、スキーマ変更が要らない

## 引き受けたコスト

- **`events` が純粋な遷移ログではなくなった。** `task show` の履歴に `queued → queued` の行が出る。`detail` を見れば分かるが、テーブルの名前は説明していない
- 判定に「最新の event」を使うので、**ノートの後に別のノート以外の event が入ると解消扱いになる**。今は `Queued` のタスクに event を書くのは ingest とこのノートだけなので成立しているが、これは不変条件として保証されているわけではない
- `status` は検査を実行しないので、**Orchestrator が動いていない間に環境が直っても表示は残る**。次に `run` が回った時点で消える

# 関連

- [ADR-0033](/decisions/adr-0033-workflow-profile.md) D9 — 検査本体（#399）。fail ではなく skip / warn という方針の出どころ
- [config.toml リファレンス](/development/config-reference.md) — 待機の運用面
- [state.db スキーマ](/data/state-db.md) — `events` テーブル
