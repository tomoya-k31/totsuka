---
type: Decision
title: ADR-0057 publish の配送方式と worktree 掃除を workflow 単位で上書きできるようにする
description: "Slack の triage 報告を承認なしでスレッドへ直接投稿し、workflow ごとに worktree/pane の掃除ポリシーを変えるための設計。ADR-0003 の「承認フロー必須」を workflow 単位の opt-out へ条件付きに緩める。配送方式は core が [[workflows]].publish から決めて ResultPublishParams.delivery（0.5.2）でプラグインへ渡し、プラグインは従うだけ。絵文字→挙動の対応表をプラグインに持つ案・kind をプラグインに解釈させる案は不採用。掃除は [[workflows]].cleanup が [worktree] の mode 既定に勝つ。"
resource: https://github.com/tomoya-k31/totsuka/issues/548
tags: [decision, slack, workflow, config, protocol, worktree, cleanup, adr]
generated: { by: claude-code/fable-5, at: 2026-08-24T09:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[#548](https://github.com/tomoya-k31/totsuka/issues/548) の実装とともに確定した。

[ADR-0003](/decisions/adr-0003-slack-reply-assistant.md)（本人名義返信 + 承認フロー必須）を**条件付きに緩める**: 承認は既定のまま、workflow 単位で opt-out できるようにする。[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md) が「承認フローの防波堤は不変」とした据え置きは、**既定としては**引き続き真である。[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)（pane の寿命は worktree に従う）は変えない。

# Context

Slack の `:books:`（`profile = "triage"`、#324/#450/#542）は「スレッドを読んで issue を起票し、URL を報告する」経路として実機で通るようになったが、実運用には 2 つ障りがある。

1. **報告が承認待ちで止まる。** `result/publish` はエフェメラルの下書きになり、人がボタンを押すまでスレッドに何も残らない。だが triage の報告は「issue を立てた」という**事実の通知**であって、質問への意見の代弁ではない — 承認で守りたいもの（本人名義の*発言*を勝手に出さない）がそもそも掛かっていない
2. **完了と同時に pane と worktree が消える。** 起票の裏取り（エージェントが何を調べてそう書いたか）を読み返せない

どちらも「全 workflow 一律」の設定しかないことに帰着する。承認は `publish_draft()` に分岐が無く、掃除は `[worktree]` の `cleanup` / `plan_cleanup` が `mode` だけで選ばれる — `Profile::Answer | Triage | Design` はすべて `plan` に解決するので、`plan_cleanup = "manual"` は answer の 12 タスク（実測）まで巻き込む。

# Decision

## 1. `[[workflows]].publish = "draft" | "direct"` を新設する（既定 `draft` = 現状）

キー名は `approval` ではない。既に `verification = "human"`（**完了**の検収ゲート）があり、`approval = "human"` を足すと「human が 2 種類」になって確実に混同される。`draft` はプラグインの確立した語彙（`Draft` / `drafts.json`）で現行挙動の名前そのもの、`direct` がその対になる。`output` = どこへ出すか、`publish` = どう出すか、と読める。

## 2. 配送方式は core が決め、プラグインは従う（protocol 0.5.2）

**Slack プラグインは publish 時に profile を知らない**（`ResultPublishParams` は `task_id` / `content` / `format`、`PendingMention` は座標と送信者だけ）。判定材料を持つのは core だけなので、`ResultPublishParams.delivery`（additive・省略可）で結果を渡す。

- **欠落 = `draft`**。0.5.2 より前の Orchestrator からの呼び出しは、その Orchestrator が書かれた当時の挙動に落ちる
- **未知の値も `draft`**（`PublishDelivery::Unrecognized`、`#[serde(other)]`）。2 つのモードの差は「人間のゲートを飛ばすか」であり、**読めない指示でゲートを飛ばすのは誤る側が逆**である
- 読むのは slack だけ（github / notion は #398 以降 `outputs = []` で `result/publish` を受けない）。承認は Slack 固有の機構なのでそれでよい — このフィールドの存在意義は汎用性ではなく、**方針（どの workflow が承認不要か）を運用者が書いた core config に置く**ことにある

### 不採用案

- **`ResultPublishParams` に `instructions_kind` を渡してプラグインに解釈させる**: 方針判断がプラグインに散る。ソースが増えるたびに同じ kind→挙動表を各プラグインが持つ
- **`plugins/slack.toml` に `auto_send_reactions = ["books"]`**: 絵文字 → 挙動の対応表は #396 が意図してプラグインから `[[workflows]].trigger` へ移したもので、それを復活させる
- **プラグインが `task_id`（`books:C…:ts`）から絵文字を読む**: 同じ結合をコードに埋める形でより悪い

## 3. direct の座標消費は投稿成功の後

現行 `publish_draft` は座標（`PendingMention`）を消費してから下書きを作るが、**下書きがローカル（`drafts.json`）に残るから** API 失敗から復旧できる。direct には残るものが無いので、先に消費すると `chat.postMessage` 失敗の時点で再送手段が消える（プラグイン再起動でも pending は戻らない）。**投稿成功を確認してから消費する**。失敗は publish 失敗として core へ返り、`fail_publish` が worktree を保全する。

## 4. `[[workflows]].cleanup` を新設し、`[worktree]` の mode 既定に勝たせる

```toml
[[workflows]]
name = "slack-books"
profile = "triage"
publish = "direct"
cleanup = "manual"    # この workflow だけ worktree と pane を残す
```

キー名は `plan_cleanup` ではない — workflow は自分が plan か implement かを `profile` で既に決めており、workflow 行で plan 側と限定する意味が無い（`[worktree]` が 2 つに分かれているのは全 workflow 共通の設定だから）。

**pane だけ残す設定は足さない。** pane を閉じる `session/release` は掃除が `Remove` と判定したときにだけ走り、`Retain` なら到達しない（ADR-0010: pane は worktree を見るための窓）。`:books:` の用途では worktree も一緒に残るほうが起票の根拠を読み返せて都合が良い。

**タスク完了後に workflow を config から削除・改名した場合、sweep は上書きを引けず mode 既定へ縮退する。これは仕様とする** — 設定を変えた側の責任であり、引けない上書きのために別の永続化を持ち込むほどの問題ではない。縮退は sweep が 1 行 log に出す。

# Consequences

- `:books:` の報告は承認なしで**本人名義**のスレッド返信になる。ADR-0003 の承認が守っていた「本人の発言を勝手に出さない」は、`answer` 系 workflow では**既定のまま**残る。`publish = "direct"` を answer に書くことも構文上は可能で、それは**運用者が明示的にゲートを外す**行為である（設定面から権限的な決定に到達できてしまう形ではあるが、ADR-0023 が禁じたのは*プロンプト文字列*からの到達であり、これは output と同じ「配線の選択」の側に置く）
- 直接投稿は `<@sender>` プレフィックス付きの通常メッセージなので Slack 通知が飛ぶ。ADR-0021 の nudge DM は draft 経路にだけ要る（direct では投稿そのものが通知になる）
- `cleanup = "manual"` の workflow は worktree が**黙って**溜まる。`doctor` の孤児検査は捕まえ**ない** — `check_orphans` の known 集合は `list_tasks()` の worktree を state 無関係に全部含むので、タスク行が残っている worktree は定義上孤児にならない（孤児検査は「DB に対応が無い worktree」のためのもので、これは「対応があるが残す判断をした worktree」）。見えるのは `totsuka status` のタスク一覧とディスク使用量だけなので、無制限に溜めたくなければ `keep_7d` を書く
- 検収は「triage だけ」を主張する変更なので、**同じ run で answer 側が巻き添えになっていないこと**（承認待ちのまま／pane が消えるまま)を対で確かめる必要がある
