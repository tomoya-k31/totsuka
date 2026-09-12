---
type: Runbook
title: Slack セットアップ Quickstart（task-source-slack）
description: 受信方式（Socket Mode / Event Gateway）の選択から始まり、manifest からの Slack アプリ作成 → トークン発行 → トークン保管 → totsuka setup → doctor → run --watch までの導入手順と、手で書く場合のフォールバック、トークン失効・スコープ変更時の対処。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-slack
tags: [slack, setup, runbook, secrets, doctor]
generated: { by: claude-code/opus-5, at: 2026-09-14T01:00:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/slack-setup.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/slack-setup.md docs/slack-setup.ja.md -->

# ゴール

自分宛の Slack メンションがタスク化され、エージェントの返信案を承認すると本人名義でスレッド返信される状態（[task-source-slack](/components/task-source-slack.md)）。所要 15 分。事前に [トークン取り扱いポリシー](/security/slack-user-token.md) に目を通すこと（社用ワークスペースは特に）。

# 0. 受信方式を選ぶ（アプリを作る**前**に）

**Slack アプリは Socket Mode と Request URL を同時には持てない。** どちらで受けるかは
アプリ単位の排他な設定で、後から変えるには**もう一方の manifest でアプリを作り直す**ことになる
（トークンも全部再発行される）。だからこれが最初の手順である。

| | **Socket Mode**（既定） | **Event Gateway** |
|---|---|---|
| 用意するもの | 無し | GCP プロジェクト 1 つ。月 1 ドル程度 |
| totsuka を止めている間 | **メンションは失われる。取り戻す手段は無い** | Pub/Sub に溜まり、起動後に拾う |
| 長く止めたとき | **Slack が購読を自動で無効化する**（60 分の配信試行の 95% 超が失敗したアプリ）。復旧は Slack の設定画面での手作業で、無効化されたことを totsuka から知る方法は API に無い | 起きない。配信は常に成功する |
| 設定 | `event_source = "socket"`（既定なので書かなくてよい） | `event_source = "gateway"` + `[slack.gateway]` |
| manifest | `manifest.yml` | `manifest.gateway.yml` |
| チャンネル監視の遅延 | 即時 | `watch_poll_interval_secs`（既定 60 秒）。**遅くなるのは監視だけ**で、メンション・リアクション・承認ボタンは 1〜2 秒差に収まる |

選び方は**その機械が止まるかどうか**に尽きる。

- **常時起動のデスクトップなら Socket Mode。** 用意するものが無く、遅延も無い
- **ノート PC なら Event Gateway。** 夜間・週末・出張のあいだプロセスが止まり、その間のメンションが
  失われるだけでなく、**止まっている時間が長いと購読そのものを止められる**。免除枠（1 時間 1,000
  イベント未満）はこの構成を守らない —— 購読しているのは参加している全チャンネルの全メッセージで、
  平日日中は容易に超える

設計の背景は [ADR-0072](/decisions/adr-0072-slack-event-gateway.md)。**Gateway を選ぶ場合、
GCP 側の構築は [Event Gateway 構築手順](/operations/event-gateway-setup.md) にある**ので、
先にそちらを通してから手順 1 に戻ること（Request URL が要る）。

# 1. Slack アプリを作成（manifest 貼り付け）

1. <https://api.slack.com/apps> → **Create New App** → **From a manifest** → 対象ワークスペースを選択。
2. **手順 0 で選んだ方式の manifest** を YAML タブに貼り付けて作成する。会話に見える投稿はすべて user scopes = 本人名義で、bot user は通知ナッジ DM 専用（[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)・#305）。

   | 方式 | manifest |
   |---|---|
   | Socket Mode | [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) |
   | Event Gateway | [`plugins/task-source-slack/manifest.gateway.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.gateway.yml)。`<gateway-host>` と `<opaque-token>` を自分のものに置き換える |

3. **Install App**(OAuth & Permissions → Install to Workspace)を実行し、**User OAuth Token**（`xoxp-…`）と **Bot User OAuth Token**（`xoxb-…`、同じページ）を控える。
4. **方式によってここが分かれる。**
   - **Socket Mode**: **Basic Information → App-Level Tokens → Generate Token and Scopes** で `connections:write` スコープのトークン（`xapp-…`）を生成して控える。
   - **Event Gateway**: App-Level Token は**要らない**（WebSocket を開かないので用途が無い）。代わりに **Basic Information → App Credentials → Signing Secret** を控える。**これは `config.toml` には書かない** —— ゲートウェイ側の Secret Manager に入る値で、totsuka の手元には残らない。

   **Gateway ではさらに、Request URL を 2 箇所に設定する。** manifest を貼って作った場合は両方入っているので確認だけでよい:

   | Slack アプリの設定 | 運ぶもの |
   |---|---|
   | Event Subscriptions → Request URL | メンション、リアクション |
   | Interactivity & Shortcuts → Request URL | 承認・リポジトリ選択のボタン |

   **片方だけだと、メンションは動いたまま承認ボタンだけが一切届かない。** Socket Mode ではどちらも同じ WebSocket で届いていたので、区別が要らなかった箇所である。保存時に Slack が URL 検証（`url_verification`）を投げるので、ゲートウェイが先に動いていること。

# 2. トークンを保管する

**通常は 1Password に置く。** 手順 3 で `[slack]` に書かれるのは**参照であって、トークンの値ではない**。

**`setup` が書く参照に合わせること。** 1Password バックエンドを選ぶと `setup` は vault `Dev` / item `totsuka` の固定形（`SecretBackend::reference`）を書くので、別の item に入れると**設定が指す先と実際の保管先が食い違い、プラグインが起動できない**:

```text
op://Dev/totsuka/slack-user   ← xoxp-…
op://Dev/totsuka/slack-app    ← xapp-…（Socket Mode のみ。Gateway では不要）
op://Dev/totsuka/slack-bot    ← xoxb-…（通知ナッジを使う場合）
```

**Signing Secret はここに入れない。** Gateway 方式で控えた signing secret は totsuka が読む値では
なく、ゲートウェイ側の Secret Manager にある登録表に入る（[Event Gateway 構築手順](/operations/event-gateway-setup.md)）。

```sh
op item edit totsuka slack-user='xoxp-…'   # item が無ければ先に作る
op item edit totsuka slack-app='xapp-…'
op item edit totsuka slack-bot='xoxb-…'    # 通知ナッジを使う場合
```

**vault 名 `Dev` も固定である。** 別の vault を使っているなら、`setup` の生成後に `[slack]` の参照を手で書き換える（手で書く場合は下記のとおり任意の参照でよい）。

macOS でしか使わないなら Keychain でもよい（参照は `keychain:totsuka/slack-user` の形になり、こちらも `setup` の生成と一致する）:

```sh
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-app  -w 'xapp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'   # 通知ナッジを使う場合
```

自分の Slack ユーザー ID（`U…`）も控える: Slack のプロフィール → **…** → **メンバー ID をコピー**。

# 3. `totsuka setup` で設定を作る

```bash
totsuka setup
```

レシピの選択で **「Slack — reply as yourself」** を選ぶ。聞かれるのはリポジトリと、手順 2 で控えたメンバー ID、リポジトリ分類用の LLM だけで、`[slack]` の生成・プラグインの install + enable・`doctor` の実行までこの 1 コマンドで済む。トークンの**値**は聞かれない（[ADR-0028](/decisions/adr-0028-setup-wizard.md)）。

手順 2 のトークン保管がまだなら、`setup` が登録コマンドのチェックリストを印字するので、それから登録する。

**登録が済んでも `state-db` チェックだけは fail のままで、`doctor` は exit 3 で終わる。** これは状態 DB がまだ無いというだけで、作るのは次の手順の `totsuka run` だけ。緑になるのは 1 回走らせたあと。

通しの導入手順（新マシン・開発機・復旧）は [セットアップ Playbook](/operations/setup-playbook.md)。

## 手で書く場合（フォールバック）

`setup` は**既存ファイルを上書きしない**ので、すでに config がある環境で Slack だけ足すときや、レシピが表現していない構成にしたいときは手で書く。

```bash
totsuka plugin install --bundled slack --enable
```

> リリース tarball ではなくチェックアウトから入れるなら `totsuka plugin install --from-source slack --enable`。`./plugins/task-source-slack` のような**ディレクトリ指定は、そこにビルド済みバイナリを自分で置いた場合にだけ**動く（[ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md)）。

`~/.config/totsuka/config.toml`（キーの意味は [設定リファレンス](/development/config-reference.md)）:

```toml
[plugins.slack]
enabled = true
kind = "task_source"

# 任意: 自分が :eyes: を付けたらタスクにする（#396）。どの workflow が
# 選ばれるかはプラグインが決める（0.6.0 / #554）: リアクションは絵文字で、
# メンションは「reaction を持たない workflow」で選ぶ。並び順は関係ない。
# 同じ絵文字を 2 つの workflow に書く／reaction 無しの workflow を 2 つ書くと
# `initialize`（= `totsuka config validate` の online パート）が拒否する。
# 他人が同じ絵文字を付けても起動しない（緩和する設定は無い）。
# 名前はコロン有無どちらでも可。👀 は eyes、👁 は eye で別物。
[[projects]]
name = "slack"
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
mode = "plan"            # 返信起案は plan（push/PR なし）で十分
agent = "herdr"
output = "source"        # result/publish → 承認フローへ
```

`~/.config/totsuka/config.toml` の `[slack]` テーブル:

```toml
[slack]
app_token = "op://Dev/totsuka/slack-app"
user_token = "op://Dev/totsuka/slack-user"
bot_token = "op://Dev/totsuka/slack-bot"  # 任意: 返信案/ピッカー到着の通知 DM（#305）。
                                            # 省略するとナッジなし（それ以外は同じ動作）
target_user_id = "U012AB3CD"        # 自分のメンバー ID
reply_style = "丁寧語で簡潔に"      # 任意

# リアクション起動は config.toml の [[workflows]].trigger.reaction で設定する（上記）。

# リポジトリ候補は config.toml の [[repositories]]（name/summary/path）が
# そのまま使われる（#109）。候補を絞る・summary を上書きするときだけ
# [[repos]] を明示する:
# [[repos]]
# name = "web-app"                  # config.toml の [[repositories]].name と一致させる
# summary = "顧客向け Web アプリ"   # 候補が複数あるときの LLM 分類の材料

# 候補が 2 件以上なら分類用 LLM が必要。config.toml の [llm]（api_key_ref 付き）が
# あれば initialize で供給され default になるため省略可（#119）。プラグイン専用の
# モデル・閾値を使いたいときだけ明示する（明示時はこちらが優先）:
# [llm]
# base_url = "https://openrouter.ai/api/v1"
# model = "…"
# api_key = "op://Dev/Openrouter/api_key"
```

# 4. 検証 → 常駐実行

```sh
totsuka config validate   # 静的検証（オフライン）
totsuka doctor            # TokenGuard: auth.test（本人一致）
                          # + bot_token 設定時は auth.test（xoxb）も probe
totsuka run --watch
```

**`doctor` が見るものは方式で変わる。**

| | Socket Mode | Event Gateway |
|---|---|---|
| `apps.connections.open`（`xapp-`） | probe する | **しない**（開く接続が無いので、使わないトークンで起動が落ちることになる） |
| Pub/Sub サブスクリプション | — | 起動時に各サブスクリプションへ `pull` を 1 回投げる。ADC の identity 違い・`roles/pubsub.subscriber` の欠落・名前の打ち間違いは、**どれも「`doctor` は緑なのにイベントが 1 件も来ない」形で失敗する**ので、ここで落とす |

Gateway 方式では `gcloud auth application-default login` が済んでいること。totsuka は
**各利用者自身の Google アカウント**でキューを引く（サービスアカウントキーは配られない）。

動作確認: 別アカウント（または同僚）に自分宛メンションをしてもらう → エージェント完了後、スレッド内エフェメラル + self-DM に返信案が届く（`bot_token` 設定時は bot からの通知 DM も届く — エフェメラル/self-DM 自体は Slack 通知を発生させないため、これが唯一の push） → **承認して返信** で本人名義のスレッド返信、**却下** で破棄（[エフェメラル承認フロー](/glossary/ephemeral-approval.md)）。

# トラブルシューティング

| 症状 | 原因と対処 |
|---|---|
| `doctor` が `invalid_auth` / `token_revoked` | トークン失効。エラーメッセージ内の再発行手順に従い、保管先（1Password / Keychain）を更新（→ [Revoke 手順](/security/slack-user-token.md)） |
| `doctor` が identity mismatch（`target_user_id`） | 他人のトークン、または `target_user_id` の誤記。なりすまし防止で意図的に拒否している |
| メンションがタスク化されない | ①メンション形式が `@自分` か（`user_events` は本人参加チャンネルのみ）②`run --watch` が起動中か ③subtype 付き（編集・bot 投稿）は対象外 |
| リアクションを付けてもタスク化されない | ①`[[workflows]]` に `trigger = { reaction = "…" }` を持つ workflow があるか（**定義順は関係ない** — #554 以降はメンションとリアクションが別のイベント経路なので、catch-all より後ろに書いても隠れない）②絵文字名が一致しているか（👀 は `eyes`、👁 は `eye`。カスタム絵文字の alias は「実際に押された名前」で届くので alias を使うなら両方列挙）③**付けたのが自分か**（他人のリアクションでは起動しない。緩和する設定は無い — [ADR-0025](/decisions/adr-0025-reaction-task-trigger.md)）④`reactions:read` を含む manifest で再インストール済みか（スコープが無いとイベント自体が届かず、**エラーにもならない**）⑤同じメッセージを既に mention 経由で処理していないか（dedup は共有） |
| リアクションを付け直しても再実行されない | 意図した挙動。dedup キーが `{channel}:{メッセージの ts}` なので、**成功したものは付け直しても再実行しない**（誤って外して付け直しただけで二重にエージェントが走る方が事故が大きい）。ただし**取得に失敗した場合は付け直しで再試行できる**（失敗時はキーを消費しない）。強制的に再実行するならプロセス再起動で LRU が消える |
| 返信案は届くがボタンが失効 | TTL 24h 超過、または FIFO 追い出し（上限 1024 件）。self-DM 記録のテキストから手動返信するか、再メンションで再実行（#122 以降、下書きは `~/.local/state/totsuka/plugins/{source_name}/drafts.json` に永続化されるため再起動ではボタンは失効しない） |
| グループメンション（`@team-name`）がタスクにならない | `usergroups:read` を含む manifest で再インストール済みか（#658）。**このスコープが無いと、起動時の `usergroups.list` が失敗して所属グループが空になり、グループ宛のメンションは 1 件もタスクにならない** —— 個人宛メンションは影響を受けないので、「一部だけ動かない」形で気づきにくい。totsuka は起動時に WARN を 1 回出すので、そこを見る。所属は**起動時に 1 回だけ**解決するので、グループに追加された直後は再起動が要る。`@here` / `@channel` / `@everyone` は**仕様として対象外**（名指しではないため、[ADR-0072](/decisions/adr-0072-slack-event-gateway.md) 決定 8） |
| **Gateway** メンションが 1 件も来ない | ①`gcloud auth application-default login` が済んでいるか（`doctor` の起動時プローブが落ちていないか）②Slack 側の Request URL が保存できているか（保存時に URL 検証が走るので、ゲートウェイが動いていないと保存自体が失敗する）③`[slack.gateway]` の `project` / `subscription` が `tofu output totsuka_config` と一致しているか |
| **Gateway** メンションは動くのに承認ボタンだけ届かない | **Interactivity & Shortcuts の Request URL** が未設定。Event Subscriptions とは別の設定項目で、Socket Mode ではどちらも同じ WebSocket で届いていたので見落としやすい |
| **Gateway** 復帰してもメンションが起票されない | `drain_max_age_hours`（既定 24）の窓の外。Pub/Sub 側は 7 日保持しているので、**設定を一時的に上げれば拾える**（クラウドの再デプロイは要らない） |
| **Gateway** 監視チャンネルの反応が遅い | 仕様。Gateway 方式では監視は `conversations.history` のポーリング（`watch_poll_interval_secs`、既定 60 秒）で、**メンション・リアクション・ボタンは遅くならない**。監視対象への投稿はゲートウェイを通らないため（publish 対象は「自分に関係しうるもの」だけ） |
| **Gateway** の切り替えを config だけでやろうとした | できない。Socket Mode と Request URL は**Slack アプリ単位で排他**なので、もう一方の manifest でアプリを作り直す（トークンも全部再発行される）。`event_source` を変えるだけでは、Slack 側が何も変わらない |
| スコープを変更した | アプリ再インストールが必要 → **`xoxp-` と `xoxb-` の両方が再発行される**ので保管先の値を両方更新 → `doctor` で確認（[manifest 雛形](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) のコメント参照）。既存アプリへ bot user を後から足す場合（#305）も同じ — `slack-bot` を追加するだけだと再発行済みの `xoxp-` が死んだままになる |
| 通知ナッジ（bot DM）が届かない | ① `bot_token` が未設定/失効（`doctor` の bot probe を確認）② 起動ログに bot DM 解決失敗の WARN がないか ③ Slack 側でこのアプリの DM をミュートしていると push は出ない（コードでは解決不能） |
| prefix ルール（`[[channel_groups]]`）が効かず常に LLM/エフェメラル選択になる | `conversations.info` が `missing_scope` で失敗しチャンネル名が取れていない（ログ WARN 参照）。`channels:read` / `groups:read` を含む manifest でアプリを再インストール → 保管先の値を更新（上の「スコープを変更した」と同手順） |

# 関連

- [Event Gateway 構築手順](/operations/event-gateway-setup.md) — `event_source = "gateway"` を選んだ場合の GCP 側
- [運用ガイド（doctor / worktree 掃除 / FAQ）](operations-guide.md)
- [ADR-0003 設計判断](/decisions/adr-0003-slack-reply-assistant.md)
