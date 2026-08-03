# テストパターン一覧

各シナリオの「起動 → 観測 → 判定」と、自動／手動／目視の内訳。**順序は上から**（後ろほど
人間の関与が増えるので、先に自動で終わるものを片付けると待ち時間が減る）。

凡例: 🤖 自動 / 🙋 手動（人間に依頼） / 👀 目視（人間が画面で判定）

---

## S1. GitHub / implement 通し 🤖

**人間の関与ゼロ。** 最初にこれを通す。ここが通らなければ他は全部止まる。

```bash
bash .claude/skills/live-e2e/scripts/github.sh seed web 1          # Issue を Todo にする
bash .claude/skills/live-e2e/scripts/github.sh wait 1              # タスクが終端に達するまで待つ
bash .claude/skills/live-e2e/scripts/github.sh verify web 1
```

| 検証点 | 見るもの |
|---|---|
| 取り込み | `tt task list` にタスクが出る |
| F-10 repo_hint 即決 | ログ `repository selected: repository hint ... matched` |
| dispatch | `herdr agent list` に `t-…` が `working` で出る |
| 実装 | ブランチが切られ、コミットが乗る |
| **F-86 / ADR-0026** | **ブランチが push され、PR が作られる** |
| 完了検知 + `verification = "llm"` | `dispatched → running → publishing → done` |
| **F-07** | **Issue にコメントが付く** |
| **F-84** | **Project の Status が `Todo` → `Done`** |
| 掃除 | `$E2E_HOME/wt/` が空、`herdr agent list` が空 |
| セッション記録 | `tt task show <id>` に `pane_id|session_id` |

> **push / PR は対象リポジトリの `CLAUDE.md` が指示していないと起きない。** ADR-0026 で push と
> PR 作成はエージェントの責務になっており、指示はリポジトリの規約が担う。`scripts/github.sh
> bootstrap` が置く `CLAUDE.md` には「終わったら push して PR を作る」が入っている。

## S2. GitHub / 取り込み制御（F-08） 🤖

S1 と同時に確認できる。`bootstrap` は cli#2 を `In Progress` にしてある。

| 検証点 | 期待 |
|---|---|
| `in_progress_statuses` の除外 | cli#2 が最後まで取り込まれない |
| Status 未設定の除外 | `(none)` の item が取り込まれない |
| 他人 assignee の除外 | **GitHub アカウントが 1 つだと素直に作れない**。`github.toml` の `github_login` を実在しない値に 1 回だけ差し替えて走らせれば、同じ `assignable_to_me` を逆側から通せる |

## S3. Slack / メンション経路 🙋👀

```bash
bash .claude/skills/live-e2e/scripts/slack.sh channels              # チャンネル ID の確認
```

**🙋 人間に依頼**（貼れる文面を渡すこと）:

> `#totsuka-e2e` で **B のクライアントから**、実装を伴う相談を打ってください。例:
> `@<A の表示名> ログ集計ツールの出力を JSON にも対応させたいです。どう実装するのが良いでしょうか？`

```bash
bash .claude/skills/live-e2e/scripts/slack.sh watch                 # タスク化 → done までを追う
bash .claude/skills/live-e2e/scripts/slack.sh draft                 # self-DM とナッジ DM に返信案が届いたか
```

**🙋 人間に依頼**: 承認ボタンを押してもらう（スレッド内エフェメラル or self-DM）

```bash
bash .claude/skills/live-e2e/scripts/slack.sh reply <thread_ts>     # A 名義のスレッド返信が生えたか
```

| 検証点 | 区分 |
|---|---|
| メンション検知 → タスク化 | 🤖 |
| **LLM リポジトリ分類（②）** | 🤖 ログの `repository resolved by the LLM classifier ... confidence=` |
| **エフェメラル picker（③）** | 🙋 confidence が閾値を割ると出る。人間が選ぶ |
| 下書き提示（2 面 + ナッジ） | 🤖 self-DM とナッジ DM は API で読める / 👀 **エフェメラル本体は読めない** |
| **承認 → 本人名義のスレッド返信** | 🙋 押下 → 🤖 `conversations.replies` で確認 |
| 送信者へのメンション前置 | 🤖 返信本文が `<@B>` で始まる |
| self-DM の ✅ 更新 | 🤖 |
| 自動返信の再検知なし | 🤖 返信自体に `bot_id` が付くので判定表①が弾く。新タスクが増えないこと |
| 却下パス | 🙋 別スレッドで却下 → 🤖 返信が生えないこと |
| 二重押下 | 🙋 もう一度押す → 👀「処理済みです」 |

## S4. Slack / リアクション経路（#319） 🙋

**🙋 人間に依頼**:

> `#totsuka-e2e` に**メンションなしの普通のメッセージ**を手で打ってください。

```bash
bash .claude/skills/live-e2e/scripts/slack.sh react <ts>            # 絵文字付けは自動（reactions.add）
bash .claude/skills/live-e2e/scripts/slack.sh watch
```

| 検証点 | 区分 |
|---|---|
| リアクションでタスクが起きる | 🤖 |
| 反応先が自分の投稿でも成立する | 🤖 mention 判定②とは意味論が逆 |
| 他人のリアクションでは起きない | 🙋 B に付けてもらう → 🤖 タスクが増えないこと |
| 付け直しで再実行されない | 🤖 dedup（`{channel}:{ts}`）。**プロセス再起動で LRU は消える** |

> **対象メッセージは人間が打ったものでなければならない。** 判定④が `bot_id` を除外するため、
> API で投稿したメッセージにリアクションを付けても起動しない。

## S5. 運用系 🤖

| 検証点 | やり方 |
|---|---|
| `task retry` からの再 dispatch | `tt task retry <id>` → **即エスカレーションしないこと**（#382 の回帰） |
| 並列実行（`max_concurrency`） | Project の item を 2 件同時に `Todo` にする → pane が 2 本立つ |
| SIGINT graceful 停止 | 🙋 人間の端末で Ctrl-C → 🤖 exit 0 とロック解放 |
| 孤児 worktree の検出 | worktree を残したまま state DB を消す → `tt doctor` が検出 |

## S6. 未検証（今回踏めていない領域）

次の機会に足す。**「やっていない」ことを報告に明記する**こと:

- `waiting_input` からの復帰（F-35 / F-44）
- `verification = "human"` の検収（`tt task verify --pass` / `--fail --reason`）
- `session/attach` による回復（§5.3。実行中に SIGKILL → 再起動）
- notifier の click-to-focus（F-94）と通知フィルタ（F-92）👀
- plan モードの設計プレビュー（F-34）👀
- orca（`agent_ide` のもう一方）

---

## 報告の書き方

「動きました」ではなく、**どの機能要件が通ったか**を書く。F-07 / F-84 / F-86 のように ID が
あるものは併記すると仕様との対応が追える。

**目視項目は「未確認」として残す。** 自動で取れた分だけで「全部通った」と書かない。
人間に何を見てほしいかは、どのチャンネルの・どのメッセージの・何を、まで具体的に伝える。
