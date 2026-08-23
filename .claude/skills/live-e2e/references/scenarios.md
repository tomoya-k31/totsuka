# テストパターン一覧

各シナリオの「起動 → 観測 → 判定」と、自動／手動／目視の内訳。**順序は上から**（後ろほど
人間の関与が増えるので、先に自動で終わるものを片付けると待ち時間が減る）。

凡例: 🤖 自動 / 🙋 手動（人間に依頼） / 👀 目視（人間が画面で判定）

---

## S1. GitHub / implement 通し 🤖

**人間の関与ゼロ。** 最初にこれを通す。ここが通らなければ他は全部止まる。

**毎回、新しい issue を作る。** `seed` の引数は **issue 番号**で、閉じた issue にも
Project に入っていない issue にも打てるが、どちらも**タスクは生まれない**。使い回すと
`wait` が前回の done を掴んで「PASS した」ように見える（2026-08-23 に一度そうなった）:

```bash
url=$(gh issue create --repo "$E2E_GH_OWNER/$E2E_GH_REPO_WEB" \
        --title "feat: … 関数を追加する（<何の検収か>）" --body "<仕様と完了条件>")
n="${url##*/}"

# **item-add より前に基準時刻を置く。** Project #7 は新規 item を自動で Todo に
# するので、`item-add` の時点でもう取り込み対象になる。基準を後で書くと、
# item-add と seed の間に poll が走ったとき**本物のタスクが「seed より古い」に
# なり**、`wait` が「使い回し issue の問題だ」と正反対の診断へ誘導する。
mkdir -p "$E2E_HOME/state/live-e2e"
date -u +%Y-%m-%dT%H:%M:%SZ > "$E2E_HOME/state/live-e2e/seed-$E2E_GH_REPO_WEB-$n"

iid=$(gh project item-add "$E2E_GH_PROJECT" --owner "$E2E_GH_OWNER" --url "$url" \
        --format json --jq .id)
# item id をキャッシュへ入れておく（`item-list` の 102 points を毎 run 節約する）
bash .claude/skills/live-e2e/scripts/github.sh prime-item web "$n" "$iid"

bash .claude/skills/live-e2e/scripts/github.sh seed  web "$n"   # Issue を Todo にする
bash .claude/skills/live-e2e/scripts/github.sh wait  web "$n"   # **その issue の**タスクを待つ
bash .claude/skills/live-e2e/scripts/github.sh verify web "$n"
```

`wait` は `source_task_id`（issue の node id）で対象を特定し、**基準時刻より後に動いた
タスクだけ**を受け付ける。前回の done しか無ければ `（seed 前の古いタスクのみ）` と
言い続けてタイムアウトする — **黙って緑にならない**のが要点。基準が無いときは
待たずに `exit 2` する（承知のうえで従来動作にするなら `ALLOW_NO_BASELINE=1`）。

`verify` も同じ基準を使う。**F-86 / ADR-0026 は「この run で作られた PR」だけを数える** —
以前はサンドボックス全体の累積数だったので、2 周目以降はエージェントが何もしなくても
両方 `[ok]` になっていた。

本文は自由だが、**完了条件に「終わったらブランチを push して PR を作る」を入れる**
（下記のとおり push / PR はリポジトリの `CLAUDE.md` とタスク本文が指示して初めて起きる）。

| 検証点 | 見るもの |
|---|---|
| 取り込み | `tt task list` にタスクが出る |
| F-10 repo_hint 即決 | ログ `repository selected: repository hint ... matched` |
| dispatch | `herdr agent list` に `t-…` が `working` で出る |
| 実装 | ブランチが切られ、コミットが乗る |
| **F-86 / ADR-0026** | **ブランチが push され、PR が作られる** |
| 完了検知 + `verification = "llm"` | `dispatched → running → publishing → done` |
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

## S0. herdr の暗黙契約 🤖（**herdr の版を上げたら必ず**）

herdr の版を上げた直後は**これを最初に回す**。schema に載らない依存なので、
型化も CI の schema 差分も 1 つもカバーしない — 実機で測る以外に知る方法が無い。
一覧・確かめ方・壊れたときの現れ方は
[herdr の暗黙契約](../../../../ai-docs/references/herdr-implicit-contracts.md)。

| 検証点 | やり方 | 落ちたら |
|---|---|---|
| **C-5 `pane.split` の shell pane が env を継承しない** | S1 の dispatch 後、**シェル pane**（`w…:p2`）で下記 1 行 | **必須項目。`TOKEN=set` ならセキュリティ問題**として扱い、以降を止める |
| C-1 token 値の 80 文字上限が「拒否」に変わっていないか | 上記 concept の 1 コマンド（`--clear-token` まで） | `report_metadata` がエラーなら identity 報告が丸ごと落ちる |
| C-2 pane id が `w1:p1` 形式 | 上記 concept の 1 コマンド | cancel / release が workspace を閉じられず、空の workspace が残る |

```bash
herdr pane run <shell-pane> 'printenv TOTSUKA_HOOK_TOKEN >/dev/null 2>&1 && echo TOKEN=set || echo TOKEN=unset'
```

**値そのものを絶対に画面へ出さない。** 上の形が安全なのは出力を捨てて終了コード
だけを見ているからで、**`echo "${TOTSUKA_HOOK_TOKEN:+set}${TOTSUKA_HOOK_TOKEN:-unset}"`
は危険**（`:-` は設定されているとき**値そのもの**へ展開する）。一度出すと pane の
履歴・`pane.read`・このシナリオを回したエージェントの transcript に残る。

**C-4（env が root pane に届く）に独立した手順は無い。** root pane で動いて
いるのは Claude Code の TUI でシェルではないので、コマンドを打っても
プロンプトとして渡るだけである。観測点は**完了検知そのもの** — env が届かなければ
Stop フックが Orchestrator を叩けず、タスクは完了報告を出さない。つまり
**S1 がフック完了で通れば C-4 は成立している**。逆に S1 が「エージェントは
答え終わっているのにタスクが `running` のままタイムアウト」で落ちたら、まず C-4 を疑う。

C-3（herdr 内部の 5 秒下限）は独立に測れない。dispatch のログで
`agent_prompt_stalled` までの時間を見る。

## S7. 複数トラッカー（#542）🤖🙋

**新しい設定形式なので、まず既存の 1 ボード構成が壊れていないことを確かめる**（S1 が
通ること）。そのうえで 2 ボード目を足す。

| 検証点 | やり方 | 落ちたら |
|---|---|---|
| 旧レイアウトが**硬く落ちる** | 変換前の `github.toml`（トップに `project_number`）のまま `tt doctor` | 黙って起動したら `deny_unknown_fields` が効いていない。**「動いた」と読まないこと** |
| 1 ボード構成の等価性 | `[[projects]]` 1 エントリへ変換 → `tt doctor` → `tt run --watch` | 変換前と**同じ item 集合**が取り込まれること。増減があれば `repos` の写し間違い |
| 2 ボードの同時 polling | 2 つ目の Project を作り `[[projects]]` を 2 エントリに → 両方に `Todo` の item を置く | 両方から取り込まれる。片方だけなら**設定順で先のボードしか見ていない** |
| ボードごとの `repos` | ボード B に、B の `repos` に無いリポジトリの issue を載せる | 取り込まれ**ない**こと |
| `update_status` の逆引き | ボード B の item を実行 → 完了で B 側の Status が動く | A 側を書き換えたら逆引きが壊れている。**再起動後にも 1 回試す**（メモが消えた状態＝フォールバック探索の経路） |
| 重複 claim の検出 | 同じリポジトリを 2 エントリの `repos` に書く → `tt config validate` | エラーになること（プラグイン内の重複） |
| **起票先の注入**（本命） | Slack で `:books:` リアクション（profile = `triage`）→ 解決先リポジトリのボードへ起票されること | ボードに載らない / 別のボードに載るなら claim か注入が壊れている |
| `trackers` チェック | `tt doctor` | claim が 1 件以上あれば `trackers` が OK と出る |

**起票先の注入は「issue が立ったか」ではなく「ボードに載ったか」で判定する。**
`destination` は機械検査されない散文（ADR-0056）で、守っているのは検収 rubric の
「投稿先の URL を報告に含めよ」だけである。エージェントが issue だけ作ってボード追加を
忘れる経路は実在しうるので、**GitHub の Project 画面で item を目視する**こと。

**Notion 側は未設定なので今回は測れない。** `plugins/notion.toml` は実運用にも
live-e2e にも存在しない。測るなら database を 2 つ作るところから要る — 測っていない
ことを報告に明記する。

## S6. 未検証（今回踏めていない領域）

次の機会に足す。**「やっていない」ことを報告に明記する**こと:

- `waiting_input` からの復帰（F-35 / F-44）
- `verification = "human"` の検収（`tt task verify --pass` / `--fail --reason`）
- `session/attach` による回復（§5.3。実行中に SIGKILL → 再起動）
- notifier の click-to-focus（F-94）と通知フィルタ（F-92）👀
- plan モードの設計プレビュー（F-34）👀
- orca（`agent_ide` のもう一方）
- **Notion の複数 database（#542）** — `plugins/notion.toml` が実運用にも live-e2e にも存在しないため、S7 の Notion 側は 1 度も回していない

---

## 報告の書き方

「動きました」ではなく、**どの機能要件が通ったか**を書く。F-07 / F-84 / F-86 のように ID が
あるものは併記すると仕様との対応が追える。

**目視項目は「未確認」として残す。** 自動で取れた分だけで「全部通った」と書かない。
人間に何を見てほしいかは、どのチャンネルの・どのメッセージの・何を、まで具体的に伝える。
