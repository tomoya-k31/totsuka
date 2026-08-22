---
name: live-e2e
description: totsuka を実機（実 Slack / 実 GitHub / 実 herdr + 実 Claude Code）で通しで動かし、結果を検証する手順。トリガー: 「実機で試したい」「実機検証」「e2e を回して」「本物の Slack で確認」「herdr で動かして」「リリース前チェック」、および herdr / Slack / GitHub 連携に手を入れた PR の検証。CI は mock プラグインまでしか検証しないので、agent_ide・task_source の実接続に関わる変更を出したら必ずこのスキルで確かめること。実機で初めて出る不具合はここでしか捕まらない。
---

# 実機 E2E 検証

CI（`slack_e2e.rs` 等）は**モック**に対して全経路を通す。実機でしか出ない不具合はそこを素通りするので、
このスキルで実 Slack / 実 GitHub / 実 herdr + 実 Claude Code に対して通す。

**このスキルは 3 つを明確に分ける。** 混ぜると検証が止まる:

| 区分 | 誰がやるか |
|---|---|
| **自動** | エージェント（このスキルの `scripts/`）が API と CLI で実行・観測する |
| **手動** | 人間にしかできない操作。**依頼して待つ**（勝手に進めない） |
| **目視** | API から読めないので人間が画面で確認する。**判定を人間に返す** |

## 0. 準備できているか確かめる

`$E2E_HOME`（既定 `~/.totsuka-e2e`）が無ければ**初回セットアップが必要**。
[references/bootstrap.md](references/bootstrap.md) を読んで、人間に依頼する作業を案内する。
アカウント・Slack アプリ・トークン・サンドボックス repo・ProjectsV2 が要るので、**数十分かかる**。

既にあるなら:

```bash
source .env && tt doctor
```

`state-db` の fail は `run` 前なら正常。それ以外の fail は先に潰す。

## 0.5. 【必須】検証対象のプラグインを入れ直す

**`cargo build` はここに効かない。** `tt run` が起動するのは
`$E2E_HOME/data/totsuka/plugins/<name>/<name>` に**インストール済みのコピー**で、
`target/debug` でも `target/release` でもない。**これを忘れると、直したはずの
コードではなく前回インストールした古いバイナリを検収することになる**（実際に、
昨日ビルドしたプラグインで検収を始めかけた）。

変更したプラグインを入れ直す:

```bash
source .env && tt plugin install --from-source --yes herdr
```

- 引数は**プラグイン名**（`herdr` / `slack` / `github` / `notion` / `orca` / `macos`）。
  ディレクトリパスを渡すと `is not a plugin in <checkout>` で落ちる
- `--from-source` は checkout から `cargo build --release` して入れる。
  Orchestrator 本体（`totsuka`）は `E2E_TOTSUKA_BIN` が指すものがそのまま使われるので、
  そちらを変えたときは `cargo build` するだけでよい

**新しさは時刻で確かめる。** 「入れ直したつもり」が一番危ない:

```bash
plug=plugins/agent-ide-herdr
installed=$(stat -f %m "$E2E_HOME/data/totsuka/plugins/herdr/herdr")
newest_src=$(find "$plug" -name '*.rs' -o -name '*.toml' | xargs stat -f %m | sort -n | tail -1)
[ "$installed" -ge "$newest_src" ] && echo "OK: install はソースより新しい" || echo "NG: 入れ直してください"
```

**比較先は「そのプラグインのソースの mtime」であって HEAD ではない。** HEAD の
committer date と比べると、**コミットせずに編集した場合に一切効かない** — live-e2e の
デバッグで一番踏みやすい「直す → 入れ直さずに再実行」がまさにそれである。
逆に `git rebase main` は committer date を現在時刻へ振り直すので、本当に新しい
install を「古い」と誤判定もする。

## 1. 【手動】常駐プロセスを起動してもらう

**エージェントからは起動できない。** 設定が `op://`（1Password）を参照しており、`op read` は
デスクトップの承認を要求するため、非対話シェルでは `authorization timeout` になる。

人間にこう依頼する:

> `source .env && tt run --watch` をあなたのターミナルで実行してください。起動したら教えてください。

起動を確認するまで次に進まない。ログに `hook receiver listening` と
`socket mode: connected (hello)` が出ていれば動いている。

## 2. シナリオを回す

[references/scenarios.md](references/scenarios.md) に**全テストパターン**と、各々の
自動／手動／目視の内訳がある。順序はこれで固定する — 後ろほど人間の関与が増えるので、
先に自動で終わるものを片付けたほうが待ち時間が減る:

0. **herdr の暗黙契約（S0）** — **herdr の版を上げたときだけ**、ただしそのときは必ず最初に。
   schema に載らない依存なので、CI も型も 1 つもカバーしない。C-5（シェル pane に
   フックトークンが載っていないこと）は**セキュリティ必須項目**
1. **GitHub / implement** — 人間の関与ゼロ。`scripts/github.sh seed` → 待つ → `verify`
2. **GitHub / 取り込み制御（F-08）** — 同上。除外されることの確認
3. **Slack / メンション** — 人間が 1 回打ち、承認ボタンを 1 回押す
4. **Slack / リアクション** — 人間が 1 回打つ。絵文字付けは自動
5. **運用系（retry・並列・graceful stop）** — 自動

各シナリオは「起動 → 観測 → 判定」の 3 段。観測は `scripts/` が担うので、**手で `gh` や
`curl` を組み立て直さない**（今回の検証で同じコマンドを何度も書き直して時間を溶かした）。

## 3. 結果を報告する

```bash
bash .claude/skills/live-e2e/scripts/report.sh
```

自動で判定できるものは pass/fail が出る。**目視項目は「未確認」として残る**ので、
人間に何を見てほしいかを具体的に伝える（どのチャンネルの、どのメッセージの、何を）。

報告は表で出す。「動きました」ではなく、**どの機能要件が通ったか**を書く
（F-07 / F-84 / F-86 のような ID があるものは併記すると、仕様との対応が追える）。

## 手動が必要な操作（ここだけは代行できない）

| 操作 | なぜ代行できないか |
|---|---|
| `tt run --watch` の起動 | `op read` がデスクトップ承認を要求する。非対話シェルではタイムアウトする |
| Slack にメンションを打つ | **API 投稿には必ず `bot_id` が付く**（user token でも）。プラグインの判定表①が除外するので、タスク化されない |
| リアクション経路の対象メッセージを打つ | 同上。反応先が人間の投稿である必要がある |
| 承認 / 却下ボタンを押す | Slack に block_actions を発火させる API は無い。Socket Mode は Slack → アプリの一方向 |
| Slack アプリの作成・再インストール | ブラウザ操作。スコープ変更時は `xoxp-` と `xoxb-` が**両方**再発行される |

**依頼するときは、貼れる形で渡す。** 「メンションしてください」ではなく、打つべき文面を書く。

## 目視でしか確認できないもの

| 項目 | なぜ API で読めないか |
|---|---|
| スレッド内エフェメラルの中身 | **どの API でも読めない**。到着の証拠は self-DM の記録と bot ナッジ DM で取る |
| 承認ボタンの confirm ダイアログ | 同上 |
| 押下後のエフェメラル削除 | `delete_original` の結果は API から見えない |
| herdr の pane レイアウト | `[layout]` の見た目（分割方向・比率） |
| plan モードの設計プレビュー | 画面表示 |
| macOS 通知センターの通知 | notifier の配送結果 |

## GitHub のレート制限

**GraphQL は 5000 points/時。** 使い切ると 1 時間止まる（実際にやった）。実測値:

| 操作 | コスト | 備考 |
|---|---|---|
| task_source の poll | **2 points**（Project #7 / 62 items / 2 ページ） | 60s 間隔で 120 points/h ＝ 2.4% |
| `github.sh` の `seed`（`set_status`） | 初回 212 / **新しい issue では 103** / 同じ item の 2 回目以降 1 | project・field id はプロジェクト単位、item id は **item 単位** |
| `github.sh` の `verify` | **102 points**（Status は可変なのでキャッシュ不可） | |

**poll を詰めても割に合わない。** 15s にすると 480 points/h（9.6%）を払って、縮まる
待ち時間は 1 回あたり平均 22 秒しかない。`poll_interval_secs` は**既定の 60s のまま**にする。

**S1 を 1 周する実際の消費**は、初回 314 points（seed 212 + verify 102）、2 周目以降は
205（seed 103 + verify 102）。S1 の手順が使う `prime-item` で seed 側の 102 を消せば
**103 まで下がる**。「1 point になる」のは同じ item へ繰り返し打ったときだけで、
毎回新しい issue を作る S1 では当たらない。

キャッシュを消すべきとき（Status option を**編集または追加**した／item を Project から
外して入れ直した／同じ owner で Project を作り直した）。**迷ったら消してよい** —
初回の 200 points を払い直すだけ:

```bash
rm -rf "$E2E_HOME/state/live-e2e/cache"
```

残量はいつでも見られる:

```bash
gh api graphql -f query='{ rateLimit { remaining limit } }' --jq .data.rateLimit
```

## 後始末

**既定は「残す」。** 次の検証で state DB とタスク履歴が手掛かりになる。片付けるときは:

```bash
bash .claude/skills/live-e2e/scripts/report.sh --cleanup-hints
```

worktree・herdr workspace・サンドボックスのブランチ/PR が列挙される。
**`rm -rf $E2E_HOME` は最後の手段** — 消すとトークン以外の全設定を作り直すことになる。

## 詰まったら

[references/troubleshooting.md](references/troubleshooting.md) に、実際に踏んだ失敗モードと
その読み方がある。**症状から原因を引ける表**にしてあるので、推測で直す前にそこを見る。

特に効くのは次の 3 つ。どれも「エラーが出ない」ので、知らないと原因に辿り着けない:

- **Slack のスコープ欠落は無症状**（イベントが配送されないだけ。エラーも出ない）
- **herdr CLI はエラーを stderr に出す**（stdout だけ見ると失敗を成功と誤読する）
- **`agent.start` の成功は「起動を受理した」であって「プロンプトを受けられる」ではない**
