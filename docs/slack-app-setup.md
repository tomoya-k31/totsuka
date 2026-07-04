# Slack アプリ作成手順(qa-service 用)

`qa-service` は Slack **Socket Mode** ボットとして動作する。動かすにはワークスペースに
Slack アプリを 1 つ作成し、2 種類のトークンを `~/.config/totsuka/secrets.toml` に設定する:

| トークン | プレフィックス | 用途 | secrets.toml のキー |
|---|---|---|---|
| App-Level Token | `xapp-` | Socket Mode の WebSocket 接続 | `[qa_service] slack_app_token` |
| Bot User OAuth Token | `xoxb-` | Web API 呼び出し(投稿・DM・リアクション等) | `[qa_service] slack_bot_token` |

## 1. アプリを作成する(マニフェスト推奨)

1. <https://api.slack.com/apps> → **Create New App** → **From a manifest**
2. 対象ワークスペースを選択
3. 以下の YAML を貼り付けて作成:

```yaml
display_information:
  name: totsuka
  description: Local agent orchestration QA bot
features:
  bot_user:
    display_name: totsuka
    always_online: true
oauth_config:
  scopes:
    bot:
      - chat:write        # chat.postMessage / chat.postEphemeral(回答投稿)
      - im:write          # conversations.open(Delegated 回答の DM コピー)
      - reactions:write   # reactions.add(受付リアクション)
      - reactions:read    # reaction_added イベント受信(GitHub issue 起票トリガー)
      - channels:history  # conversations.history / replies + message.channels 受信
      - groups:history    # ↑の private チャンネル版(使わないなら削除可)
settings:
  event_subscriptions:
    bot_events:
      - message.channels  # public チャンネルの発言(質問トリガー)
      - message.groups    # private チャンネルの発言(使わないなら削除可)
      - reaction_added    # reaction_trigger(既定 "memo")による issue 起票
  socket_mode_enabled: true
  interactivity:
    is_enabled: false
```

UI から手作業で作る場合は、**OAuth & Permissions → Bot Token Scopes** に上記スコープを、
**Event Subscriptions → Subscribe to bot events** に上記イベントを追加し、
**Socket Mode** を有効化する(内容は同じ)。

## 2. App-Level Token を発行する(`xapp-`)

1. アプリ設定の **Basic Information → App-Level Tokens** → **Generate Token and Scopes**
2. 名前は任意(例: `socket-mode`)、スコープに **`connections:write`** を追加して生成
3. 表示された `xapp-1-...` を控える(この画面でしか全文表示されない)

## 3. ワークスペースにインストールする(`xoxb-`)

1. **Install App**(または **OAuth & Permissions**)→ **Install to Workspace** → 許可
2. **Bot User OAuth Token**(`xoxb-...`)を控える

> **重要: スコープを後から追加・変更した場合は再インストールが必要。**
> 再インストールするまで新スコープは有効にならず、該当 API が `missing_scope` で失敗する。
> 例: `im:write` 未反映の間、qa-service は回答自体は届けるが DM コピーだけを
> `DM copy failed ... missing_scope` の WARN ログに落とす(best-effort 設計)。

## 4. secrets.toml に設定する

`totsukactl init` が生成した `~/.config/totsuka/secrets.toml`(`chmod 0600`)に記載する:

```toml
[qa_service]
slack_app_token = "xapp-1-..."
slack_bot_token = "xoxb-..."
```

平文の代わりに 1Password Secret Reference(`op://vault/item/field`)も使える。
その場合は各バイナリのプロセス環境で `op` CLI が認証済みであること
(例: `OP_SERVICE_ACCOUNT_TOKEN`)。

## 5. チャンネルにボットを招待する

質問を拾わせたい各チャンネルで:

```
/invite @totsuka
```

ボットが参加していないチャンネルのイベントは届かない(履歴 API も読めない)。

## 6. config.toml 側の設定

`~/.config/totsuka/config.toml` の `[qa_service]`:

```toml
[qa_service]
allowed_user_ids = ["U08XXXXXXXX"]   # 質問を受け付けるユーザー(必須)
catchup_channels = ["C0XXXXXXXXX"]   # 起動時 catch-up で遡るチャンネル(任意)
reaction_trigger = "memo"            # このリアクションで GitHub issue 起票
default_mode     = "delegated"       # auto(公開回答)| delegated(エフェメラル+DM コピー)
```

- **ユーザー ID の調べ方**: Slack でプロフィールを開く → `⋮` → 「メンバー ID をコピー」
- ボット自身のユーザー ID は設定不要(起動時に `auth.test` で自動解決)

## 7. 動作確認

1. スタックを起動: `./target/release/totsukactl up`
2. ログで接続を確認:

   ```bash
   tail -f ~/.local/state/totsuka/logs/qa-service.log
   # "resolved bot user id via auth.test" と "socket-mode hello received" が出れば OK
   ```

3. 招待済みチャンネルで `allowed_user_ids` のユーザーから `@totsuka <質問>` を投稿
   - `default_mode = "delegated"` の場合: スレッドにエフェメラル回答 + ボット DM に
     永続コピー(質問抜粋 + スレッドリンク + 回答全文)が届く
   - `default_mode = "auto"` の場合: スレッドに公開回答が付く

## スコープと機能の対応(トラブルシュート用)

| 症状 | 不足しているもの |
|---|---|
| 起動直後に `auth.test` 失敗で異常終了 | `slack_bot_token` が不正(スコープ以前の問題) |
| Socket Mode が繋がらない | `slack_app_token` 不正、または `connections:write` なし |
| メンションに全く反応しない | イベント購読(`message.channels` 等)漏れ / チャンネル未招待 / `allowed_user_ids` 不一致 |
| 回答は付くが DM コピーが来ない | `im:write` なし(WARN ログに `missing_scope`)→ 追加して再インストール |
| リアクションで issue が起票されない | `reactions:read`(イベント)/ `reaction_added` 購読漏れ |
| 受付リアクションが付かない | `reactions:write` なし |
| catch-up がチャンネルを読めない | `channels:history`(private は `groups:history`)なし |
