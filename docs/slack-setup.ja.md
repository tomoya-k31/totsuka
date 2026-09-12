> 🌐 [English](slack-setup.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/slack-quickstart.md sha256:1d984312a60b22cbd824d6805822973604e13db43f0650454201d69c9d9daf33 -->

# Slack ソースのセットアップ

所要 15 分。終わると、Slack で自分宛のメンションが totsuka のタスクになり、エージェントの返信案を承認するとスレッドに**本人名義で**投稿される。

会話に見える投稿はすべてユーザートークンで行われる。アプリの bot ユーザーは通知 DM を送るためだけに存在する — エフェメラルメッセージと self-DM は、それ自体では Slack の通知を発生させないためである。

> **社用アカウントなら、先にワークスペースの規約を確認すること。** ユーザートークンは本人として振る舞い、そこから投稿されたものは本人が打ったものと区別できない。ユーザートークンのアプリを制限・禁止している組織もある。

## 0. 受信方式を選ぶ — アプリを作る**前**に

**Slack アプリは Socket Mode と Request URL を同時には持てない。** アプリ単位の排他な設定で、
後から変えるには**もう一方の manifest でアプリを作り直す**ことになる（トークンも全部再発行される）。
だからこれは最初の手順であって、あとから調整するものではない。

| | **Socket Mode**（既定） | **Event Gateway** |
|---|---|---|
| 用意するもの | 無し | GCP プロジェクト 1 つ、月 1 ドル程度 |
| totsuka を止めている間 | **メンションは失われ、取り戻す手段は無い** | キューに溜まり、起動すると拾われる |
| 長く止めたとき | **Slack が購読を無効化する**（60 分の配信試行の 95% 超が失敗したアプリ）。復旧は Slack の設定画面での手作業で、それが起きたことを totsuka に知らせるものは何も無い | 起きない。配信は常に成功する |
| 設定 | `event_source = "socket"`（既定なので省略可） | `event_source = "gateway"` と `[slack.gateway]` |
| manifest | `manifest.yml` | `manifest.gateway.yml` |
| チャンネル監視の遅延 | 即時 | `watch_poll_interval_secs`（既定 60 秒）。**遅くなるのは監視だけ**で、メンション・リアクション・承認ボタンは 1〜2 秒差に収まる |

判断は**その機械が止まるかどうか**に尽きる。

- **常時起動のデスクトップなら Socket Mode。** 用意するものが無く、遅延も増えない
- **ノート PC なら Event Gateway。** 夜間・週末・出張のあいだ止まり、代償はその間のメンションだけ
  ではなく**購読そのものを止められること**である。低流量アプリの免除（1 時間 1,000 イベント未満）は
  この構成を守らない —— 購読しているのは参加している全チャンネルの全メッセージで、平日なら容易に超える

Gateway を選ぶなら順番がややこしいので、先に書いておく —— Request URL に入れるホスト名は
GCP 側を作った結果で、それを作るのに要る signing secret は Slack アプリの結果である。こう割る:

1. **下の手順 1 の 1〜3 だけを先にやる** —— アプリを作り、トークンと signing secret を控える。
   `manifest.gateway.yml` の `<gateway-host>` はプレースホルダのままでよい
2. [Event Gateway 構築手順](event-gateway-setup.ja.md) を通す
3. **アプリに戻って**、出てきた Request URL を 2 箇所に入れる
4. このページの手順 2 に戻る

## 1. manifest から Slack アプリを作る

1. <https://api.slack.com/apps> → **Create New App** → **From a manifest**、対象ワークスペースを選ぶ。
2. **選んだ方式の manifest** を YAML タブに貼り付けてアプリを作成する。

   | 方式 | manifest |
   |---|---|
   | Socket Mode | [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) |
   | Event Gateway | [`plugins/task-source-slack/manifest.gateway.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.gateway.yml) —— `<gateway-host>` と `<opaque-token>` を自分のものに置き換える |

3. **OAuth & Permissions → Install to Workspace** を実行し、同じページから **User OAuth Token**（`xoxp-…`）と **Bot User OAuth Token**（`xoxb-…`）を控える。
4. **ここは方式で分かれる。**
   - **Socket Mode**: **Basic Information → App-Level Tokens → Generate Token and Scopes** で `connections:write` スコープのトークンを生成し、控える（`xapp-…`）。
   - **Event Gateway**: App-Level Token は**要らない**（WebSocket を開かないため）。代わりに **Signing Secret** を控える（Basic Information → App Credentials）。**これは `config.toml` には書かない** —— ゲートウェイ側のシークレットストアに入る値で、この機械には残らない。

   **Gateway では Request URL を 2 箇所に入れる。** manifest から作れば両方入っているので、確認だけでよい:

   | Slack アプリの設定 | 運ぶもの |
   |---|---|
   | Event Subscriptions → Request URL | メンション、リアクション |
   | Interactivity & Shortcuts → Request URL | 承認・リポジトリ選択のボタン |

   **前者だけだと、メンションは動いたままボタンが一切届かない。** Socket Mode では両方が同じ接続で届いていたので、これまで存在しなかった区別である。保存時に Slack が URL を検証するので、ゲートウェイが先に動いていること。

自分のメンバー ID（`U…`）も控える: Slack のプロフィール → **⋯** → **メンバー ID をコピー**。

## 2. トークンを保管する

totsuka がシークレットの値を保存することはない。設定に書くのは**参照**で、値は実行時に取得される。

**`setup` が書く参照に合わせること。** 1Password バックエンドを選ぶと `setup` は vault `Dev` / item `totsuka` の固定形を書くので、別の場所に保管すると設定が実在しないものを指したままになり、**プラグインが起動できない**:

```text
op://Dev/totsuka/slack-user   ← xoxp-…
op://Dev/totsuka/slack-app    ← xapp-…  （Socket Mode のみ。Gateway では不要）
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
[[projects]]
name = "slack"  # domain を持たないソースもエントリが要る
source = "slack"

[[workflows]]
name = "slack-reaction"
projects = ["slack"]
trigger = { reaction = "eyes" }
mode = "plan"
agent = "herdr"
output = "source"

[[workflows]]
name = "slack-reply"
projects = ["slack"]
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
totsuka run --watch
```

**`doctor` が見るものは方式で変わる。**

| | Socket Mode | Event Gateway |
|---|---|---|
| App-Level Token（`xapp-`） | probe する | **しない** —— 開く接続が無いので、使わないトークンで起動が落ちるのは筋が通らない |
| キュー | — | 起動時に各キューを 1 回読む。Google の identity 違い・権限の欠落・名前の打ち間違いは、放っておくとどれも**「`doctor` は緑で、イベントが 1 件も来ない」**という同じ形で失敗する |

Gateway ではこの機械で 1 回 `gcloud auth application-default login` を実行しておく。
totsuka は**自分の** Google アカウントで**自分の**キューを読む —— サービスアカウントキーは配られない。

通しで試すには、誰かに自分宛のメンションをしてもらう。エージェントの完了後、スレッド内のエフェメラルメッセージと self-DM に返信案が届く（`bot_token` を設定していれば bot からの DM も届く）。**承認**すると本人名義のスレッド返信として投稿され、**却下**すると破棄される。

## トラブルシューティング

| 症状 | 原因と対処 |
|---|---|
| `doctor` が `invalid_auth` / `token_revoked` を報告する | トークンが失効している。再発行し、保管先の値を更新する |
| `doctor` が identity mismatch を報告する | 他人のトークンか、`target_user_id` の誤記。他人名義での投稿を防ぐため意図的に拒否している |
| メンションがタスクにならない | メンションが `@自分` か（見えるのは自分が参加しているチャンネルだけ）、`run --watch` が動いているか、そして通常の投稿か（編集や bot の投稿は対象外）を確認する |
| リアクションを付けてもタスクにならない | `trigger = { reaction = "…" }` を持つ workflow があるか（**ファイル内の並び順は関係ない** — メンションとリアクションは別のイベント経路で届くので、catch-all より後ろに書いた reaction workflow が隠れることはない）、絵文字名が一致しているか（👀 は `eyes`、👁 は `eye`。カスタム絵文字は実際に押された名前で届くので alias も列挙する）、**付けたのが自分か**、`reactions:read` を含む manifest でアプリを再インストールしたか（このスコープが無いとイベント自体が届かず、**しかも何もエラーを出さない**）、そして**そのメッセージを既にメンション経由で処理していないか**を確認する。両経路は処理済みメッセージの集合を共有しているので、すでにタスクになったメッセージにリアクションを付けても何も起きない |
| リアクションを付け直しても再実行されない | 意図した挙動。成功したメッセージは二度と処理されないので、外して付け直してもエージェントが二重に走ることはない。**取得に失敗した**メッセージはこの方法で再試行できる |
| 返信案は届くがボタンが効かない | 24 時間で失効する。または下書きが 1024 件を超えて追い出された。self-DM の控えから手で返信するか、もう一度メンションする。下書きは再起動しても残る |
| グループメンション（`@team-name`）がタスクにならない | `usergroups:read` を含む manifest でアプリを再インストール済みか確認する。**このスコープが無いと、起動時の所属グループ照会が失敗して所属が空になり、グループ宛のメンションは 1 件もタスクにならない** —— 個人宛メンションは動き続けるので、「一部だけ壊れている」ように見える。totsuka は起動時に警告を 1 回出すので、そこを見る。所属は**起動時に 1 回だけ**解決するので、グループに追加された直後は再起動が要る。`@here` / `@channel` / `@everyone` は**仕様として対象外**（誰も名指ししていないため） |
| **Gateway**: メンションが 1 件も来ない | `gcloud auth application-default login` が済んでいるか（起動時の検査が報告する）、保存時に Slack が Request URL を受け付けたか（保存時に URL を検証するので、ゲートウェイが動いていないと保存自体が失敗する）、`[slack.gateway]` がデプロイの出力と一致しているかを確認する |
| **Gateway**: メンションは動くのに承認ボタンだけ届かない | **Interactivity & Shortcuts** の Request URL が未設定。Event Subscriptions とは別の設定項目で、Socket Mode では両方が同じ接続で届いていたため見落としやすい |
| **Gateway**: 復帰しても何も起票されない | イベントが `drain_max_age_hours`（既定 24）より古い。キューは 7 日保持しているので、**設定を一時的に上げれば拾える** —— 再デプロイは要らない |
| **Gateway**: 監視チャンネルの反応が遅い | 仕様。Gateway ではチャンネル履歴のポーリング（`watch_poll_interval_secs`、既定 60 秒）で監視しており、メンション・リアクション・ボタンは遅くならない。ゲートウェイを通るのは自分を名指ししたものだけで、監視チャンネルへの普通の投稿は該当しない |
| 設定を書き換えるだけで方式を切り替えようとした | そうはならない。Socket Mode と Request URL は**Slack アプリ単位で排他**なので、切り替えはもう一方の manifest でアプリを作り直すことを意味する（トークンも全部再発行される）。`event_source` を変えるだけでは Slack 側は何も変わらない |
| アプリのスコープを変更した | スコープ変更にはアプリの再インストールが必要で、**`xoxp-` と `xoxb-` の両方が再発行される**。保管先の値を両方更新してから `doctor` を実行する。片方だけ直すとアプリは半分壊れたままになる |
| チャンネル prefix のルールが効かず、毎回 LLM 分類（LLM 未設定ならピッカー）に落ちる | アプリがチャンネル名を読めていない。`channels:read` と `groups:read` を含む manifest で再インストールし、上と同じ手順でトークンを更新する |
| 通知 DM が届かない | `bot_token` が設定され有効か（`doctor` が probe する）、起動ログに bot DM の解決失敗の警告が無いか、Slack でこのアプリの DM をミュートしていないかを確認する |

---

Slack 以外も含めた新マシンの導入手順は [セットアップ Playbook](setup-playbook.ja.md) にある。

詳細な設計上の判断と、これらの手順の背景はリポジトリの `ai-docs/operations/slack-quickstart.md` にある。
