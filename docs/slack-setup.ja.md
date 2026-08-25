> 🌐 [English](slack-setup.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/slack-quickstart.md sha256:a186d905c6d5772826c66779a22220f93c7edf7e2ce7c7b9dc739899d2fba410 -->

# Slack ソースのセットアップ

所要 15 分。終わると、Slack で自分宛のメンションが totsuka のタスクになり、エージェントの返信案を承認するとスレッドに**本人名義で**投稿される。

会話に見える投稿はすべてユーザートークンで行われる。アプリの bot ユーザーは通知 DM を送るためだけに存在する — エフェメラルメッセージと self-DM は、それ自体では Slack の通知を発生させないためである。

> **社用アカウントなら、先にワークスペースの規約を確認すること。** ユーザートークンは本人として振る舞い、そこから投稿されたものは本人が打ったものと区別できない。ユーザートークンのアプリを制限・禁止している組織もある。

## 1. manifest から Slack アプリを作る

1. <https://api.slack.com/apps> → **Create New App** → **From a manifest**、対象ワークスペースを選ぶ。
2. [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) を YAML タブに貼り付けてアプリを作成する。
3. **OAuth & Permissions → Install to Workspace** を実行し、同じページから **User OAuth Token**（`xoxp-…`）と **Bot User OAuth Token**（`xoxb-…`）を控える。
4. **Basic Information → App-Level Tokens → Generate Token and Scopes** で `connections:write` スコープのトークンを生成し、控える（`xapp-…`）。

自分のメンバー ID（`U…`）も控える: Slack のプロフィール → **⋯** → **メンバー ID をコピー**。

## 2. トークンを保管する

totsuka がシークレットの値を保存することはない。設定に書くのは**参照**で、値は実行時に取得される。

**`setup` が書く参照に合わせること。** 1Password バックエンドを選ぶと `setup` は vault `Dev` / item `totsuka` の固定形を書くので、別の場所に保管すると設定が実在しないものを指したままになり、**プラグインが起動できない**:

```text
op://Dev/totsuka/slack-user   ← xoxp-…
op://Dev/totsuka/slack-app    ← xapp-…
op://Dev/totsuka/slack-bot    ← xoxb-…（通知 DM を使う場合のみ）
```

```sh
op item edit totsuka slack-user='xoxp-…'   # item が無ければ先に作る
op item edit totsuka slack-app='xapp-…'
op item edit totsuka slack-bot='xoxb-…'    # 任意
```

**vault 名 `Dev` も固定である。** 別の vault を使っているなら、`setup` の生成後に `[slack]` の参照を手で書き換える（下記のとおり手で書く場合は任意の参照でよい）。

macOS なら Keychain でもよい。参照は `keychain:totsuka/slack-user` の形になり、これは `setup` が書くものと一致する:

```sh
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-app  -w 'xapp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'   # 任意
```

## 3. 設定を作る

```bash
totsuka setup
```

レシピは **「Slack — reply as yourself」** を選ぶ。聞かれるのはリポジトリ、手順 1 で控えたメンバー ID、そしてメンションがどのリポジトリの話かを判定する LLM だけである。`[slack]` の生成、プラグインの install と enable、`doctor` の実行まで、この 1 コマンドで済む。**トークンの値は一切聞かれない。**

トークンをまだ保管していなければ、`setup` が実行すべきコマンドのチェックリストを印字する。

**すべてのトークンを保管しても、`state-db` チェックだけは fail のままで `doctor` は exit 3 で終わる。** これは状態データベースがまだ無いというだけで、作るのは `totsuka run` だけである。1 回走らせれば緑になる。

### 設定を手で書く場合

`setup` は**既存ファイルを上書きしない**ので、すでに設定がある環境に Slack を足すときや、レシピが表現していない構成にしたいときは手で書く。

```bash
totsuka plugin install --bundled slack --enable
```

> ソースのチェックアウトから入れるなら `totsuka plugin install --from-source slack --enable` を使う。`./plugins/task-source-slack` のようなディレクトリ指定は、そこにビルド済みバイナリを自分で置いた場合にだけ動く。

`~/.config/totsuka/config.toml`:

```toml
[plugins.slack]
enabled = true
kind = "task_source"

# 任意: 自分が :eyes: を付けるとメッセージがタスクになる。
# どの workflow が選ばれるかはプラグインが決める: リアクションは絵文字が
# 一致する workflow、素のメンションは `reaction` トリガを持たない唯一の
# workflow へ行く —— このファイル内の並び順は関係ない。同じ絵文字を
# 2 つの workflow に書く／reaction 無しの workflow を 2 つ書くと、起動時
#（と `totsuka config validate`）に拒否される。
# 他人が付けても起動せず、それを緩和する設定は無い。
# 名前はコロン有無どちらでもよい。👀 は `eyes`、👁 は `eye` で別の絵文字。
[[workflows]]
name = "slack-reaction"
source = "slack"
trigger = { reaction = "eyes" }
mode = "plan"
agent = "herdr"
output = "source"

[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
mode = "plan"            # 返信の起案に push も PR も要らない
agent = "herdr"
output = "source"        # 結果は承認フローへ渡る
```

`~/.config/totsuka/config.toml`:

```toml
[slack]
app_token = "op://Dev/totsuka/slack-app"
user_token = "op://Dev/totsuka/slack-user"
bot_token = "op://Dev/totsuka/slack-bot"  # 任意: 通知 DM。
                                          # 省略すると DM が来ないだけ
target_user_id = "U012AB3CD"              # 自分のメンバー ID
reply_style = "丁寧語で簡潔に"            # 任意

# リポジトリ候補は config.toml の [[repositories]] がそのまま使われるので、
# 通常ここに [[repos]] は要らない。候補を絞る・summary を上書きするときだけ書く:
# [[repos]]
# name = "web-app"
# summary = "顧客向け Web アプリ"

# 候補が 2 件以上あるときは分類用の LLM が要る。config.toml の [llm] に
# キーがあれば自動的に供給される。このプラグインだけ別のモデルや閾値を
# 使いたいときにだけ書く:
# [llm]
# base_url = "https://openrouter.ai/api/v1"
# model = "…"
# api_key = "op://Dev/Openrouter/api_key"
```

各キーの意味は [設定リファレンス](config-reference.ja.md) にある。

## 4. 検証して常駐させる

```sh
totsuka config validate   # オフラインの検査
totsuka doctor            # Slack に対してトークンを検査する。ユーザートークンの
                          # identity が target_user_id と一致することも確認する
totsuka run --watch       # ソケット接続に常駐する
```

通しで試すには、誰かに自分宛のメンションをしてもらう。エージェントの完了後、スレッド内のエフェメラルメッセージと self-DM に返信案が届く（`bot_token` を設定していれば bot からの DM も届く）。**承認**すると本人名義のスレッド返信として投稿され、**却下**すると破棄される。

## トラブルシューティング

| 症状 | 原因と対処 |
|---|---|
| `doctor` が `invalid_auth` / `token_revoked` を報告する | トークンが失効している。再発行し、保管先の値を更新する |
| `doctor` が identity mismatch を報告する | 他人のトークンか、`target_user_id` の誤記。他人名義での投稿を防ぐため意図的に拒否している |
| メンションがタスクにならない | メンションが `@自分` か（見えるのは自分が参加しているチャンネルだけ）、`run --watch` が動いているか、そして通常の投稿か（編集や bot の投稿は対象外）を確認する |
| リアクションを付けてもタスクにならない | `trigger = { reaction = "…" }` を持つ workflow があるか、それが catch-all の `trigger = {}` より**前**にあるか（`totsuka config validate` が警告し、直し方まで名指しする）、絵文字名が一致しているか（👀 は `eyes`、👁 は `eye`。カスタム絵文字は実際に押された名前で届くので alias も列挙する）、**付けたのが自分か**、`reactions:read` を含む manifest でアプリを再インストールしたか（このスコープが無いとイベント自体が届かず、**しかも何もエラーを出さない**）、そして**そのメッセージを既にメンション経由で処理していないか**を確認する。両経路は処理済みメッセージの集合を共有しているので、すでにタスクになったメッセージにリアクションを付けても何も起きない |
| リアクションを付け直しても再実行されない | 意図した挙動。成功したメッセージは二度と処理されないので、外して付け直してもエージェントが二重に走ることはない。**取得に失敗した**メッセージはこの方法で再試行できる |
| 返信案は届くがボタンが効かない | 24 時間で失効する。または下書きが 1024 件を超えて追い出された。self-DM の控えから手で返信するか、もう一度メンションする。下書きは再起動しても残る |
| アプリのスコープを変更した | スコープ変更にはアプリの再インストールが必要で、**`xoxp-` と `xoxb-` の両方が再発行される**。保管先の値を両方更新してから `doctor` を実行する。片方だけ直すとアプリは半分壊れたままになる |
| チャンネル prefix のルールが効かず、毎回 LLM 分類（LLM 未設定ならピッカー）に落ちる | アプリがチャンネル名を読めていない。`channels:read` と `groups:read` を含む manifest で再インストールし、上と同じ手順でトークンを更新する |
| 通知 DM が届かない | `bot_token` が設定され有効か（`doctor` が probe する）、起動ログに bot DM の解決失敗の警告が無いか、Slack でこのアプリの DM をミュートしていないかを確認する |

---

Slack 以外も含めた新マシンの導入手順は [セットアップ Playbook](setup-playbook.ja.md) にある。

詳細な設計上の判断と、これらの手順の背景はリポジトリの `ai-docs/operations/slack-quickstart.md` にある。
