# 詰まったときの読み方

実際に踏んだ失敗モードと原因。**症状から引ける表**にしてある。推測で直す前にここを見る。

## 症状 → 原因

| 症状 | 原因 | 対処 |
|---|---|---|
| タスクが `pending` のまま | `[[repositories]].name` が GitHub のリポジトリ名と違う。`repo_hint` が一致せず LLM 選択へ落ち、`[llm]` が無いと `pending` | `name` を GitHub 上の名前と一致させる |
| `run` が `authorization timeout` で落ちる | `op read` がデスクトップ承認を要求している。非対話シェルからは不可 | 人間のターミナルから起動してもらう |
| dispatch が `missing field 'kind'` | herdr が 0.7.5 未満、またはプラグインが古い | `herdr update` / プラグインを再 install |
| dispatch が `agent_pane_busy` | 生成直後の pane はシェルがまだ起動中 | プラグインが 60 秒までリトライする。超えるならシェルの rc が重い |
| dispatch が `agent_not_ready` | `agent.start` は受理されたが CLI が実際には起動していない。打鍵が入力可能前のシェルに送られて消えた（#387） | プラグインが `agent.start` を再送する。**待っても回復しない**（120 秒待っても pane は空のまま）ので、古いバイナリでは retry が要る |
| dispatch が `timeout: timed out waiting for agent startup` | 同上。同じレースが `agent.start` 側に出た形 | 同上。リトライで吸収される |
| dispatch が `agent_prompt_stalled` | herdr の 5 秒下限に Claude Code が間に合わなかった。**設定では変えられない** | プラグインが `agent.wait` で確認に回る。それでも駄目なら `tt task retry` |
| dispatch が `is already occupied but is not recorded` | 前回の worktree が残っているのに state DB が消えている | `git worktree remove --force` してから retry |
| dispatch 直後に `escalated` | D-03 の沈黙アンカーが前回実行のもの（#382） | 修正済み。古いバイナリなら入れ直す |
| メンションしてもタスク化されない | **API で投稿した**（`bot_id` が付く）／ `run --watch` が止まっている／編集済み投稿（subtype 付き） | 人間に手で打ってもらう |
| リアクションを付けても無反応 | **`reactions:read` が無い**（無症状）／絵文字名の不一致／既に処理済み（dedup） | スコープを確認 → 現行マニフェストで再インストール |
| prefix ルール（`channel_groups`）が効かない | `channels:read` / `groups:read` が無く `conversations.info` が失敗 | 同上 |
| plan モードなのに PR が生えた | **plan は git を構造的に止めていない**（#378） | `profile` 記法に移行する。#395 の deny が `--settings` 経由で入り、リポジトリの `CLAUDE.md` の指示より**必ず強い**。明示記法（`mode = "plan"`）には deny が付かない |
| `implement` profile のタスクが `Queued` から動かない | #399 の `gh` 検査に落ちている。`tt` が `XDG_CONFIG_HOME` を差し替えるので、`GH_CONFIG_DIR` が無いと**本物の gh 設定を見つけられない** | `_common.sh` の `tt()` に `GH_CONFIG_DIR` があるか確認。無ければ古い雛形。ログに「waiting: gh unavailable」が出る |
| `answer` profile のタスクで `Edit` が拒否される | **正常動作**（#395）。answer は worktree を書かない profile | 実装させたいなら profile を変える。Slack 起点なら本人が ➕ を付けて別タスクを起こす（#397） |
| `design` タスクが pane で座礁する | `gh` 未認証。**design には #399 の検査が効かない**（書き込み先が source 依存で判別できないため意図的に対象外） | 事前に `gh auth status` を確認する。implement と違い自動では待たない |
| 完了申告したのに検収で差し戻される（design/implement） | **URL 実在検収**（#398）。最終メッセージに成果物 URL が無いか、未来形（「これから書く」） | 正常動作。エージェントが実際に書いていない可能性を先に疑う |

## スコープを確認する

トークンの値を出さずにスコープだけ見る:

```bash
curl -sD- -o /dev/null -H "Authorization: Bearer $(op read 'op://Dev/Totsuka - local/user_token')" \
  https://slack.com/api/auth.test | grep -i x-oauth-scopes
```

`.env` に直接置いている場合は `$E2E_SLACK_USER` などに読み替える。

> **スコープを足したら再インストールが必要で、`xoxp-` と `xoxb-` が両方再発行される。**
> 片方だけ更新すると、もう片方が死んだまま無症状で動き続ける。
>
> **スコープと Event Subscriptions は別物。** `reactions:read` を足しても、
> Event Subscriptions の `Subscribe to events on behalf of users` に `reaction_added` が
> 無ければイベントは来ない。

## herdr を調べるとき

**herdr CLI はエラーを stderr に出す。** stdout だけを見ると、失敗を成功と読み違える:

```bash
herdr agent start x --kind claude --pane w1:p1        # 失敗しても stdout は空
# 正しくは
out=$(herdr agent start x --kind claude --pane w1:p1 2>&1)
```

サーバログに一次情報がある:

```bash
tail -50 ~/.config/herdr/herdr-server.log
```

`agent changed pane=NN ... agent=Some(Claude) process=claude` が出ていれば、CLI は実際に
起動している。`agent.start → error` が数回続いてから `ok` になり、その直後にこの行が出るのが
正常なパターン（pane の準備待ち）。

**ワイヤを見たいときはソケットにプロキシを挟む。** `api_url` / `socket_path` を localhost に
向ければ往復を記録できる。`launch_pending: true` のような、応答を読まないと分からない事実は
これで初めて見えた。

## 実行系のログ

```bash
tail -30 "$E2E_HOME/state/totsuka/logs/totsuka.log.$(date +%Y-%m-%d)" \
  | python3 -c 'import sys,json
for l in sys.stdin:
    try: d=json.loads(l); print(d.get("level"),"|",(d.get("message") or "")[:200])
    except Exception: print(l[:200])'
```

JSON Lines なので `grep` より整形して読むほうが速い。

## エージェントが自力で判断できないこと

次は**人間に聞く**。推測で進めると検証全体が無駄になる:

- Slack ワークスペースを新規に作るか、個人のものを流用するか
- トークンをどこに置くか（1Password か `.env` か）
- 実機エージェント（課金が発生する）をどの頻度で回すか
- サンドボックス以外のリポジトリを対象に含めるか
