> 🌐 [English](discord-setup.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/discord-quickstart.md sha256:623e032b56554be9e211ec6b91a9994e373d9ac8d2f8ffb2dac58a6af3f04132 -->

# Discord ソースのセットアップ

所要 10 分。終わると、Discord の特定チャンネルに投稿したものがそのまま totsuka のタスクになり、結果がその投稿のスレッドに bot 名義で返る。

> **専用の Discord サーバーを用意すること。** totsuka が使う権限は「bot が見えるチャンネル全部」の本文を読めるもので、チャンネル単位で絞ることは Discord 側ではできない。日常会話のサーバーに入れると、設定を 1 行間違えたときの影響がそこまで広がる。

投稿は bot 名義になる。Discord は通常のユーザーアカウントを自動で操作することを禁じているので、あなたの名前で投稿する方法は無い。

## 1. サーバーとチャンネル

専用サーバーを作り、監視用チャンネル（例 `clip`）を 1 つ作る。

## 2. アプリと bot を作る

1. <https://discord.com/developers/applications> → New Application
2. **Bot** タブ → Reset Token でトークンを発行して控える

   > **この画面を離れると再表示できない。** 失くしたら再発行することになり、再発行すると前のトークンは即座に使えなくなる。

3. 同じ **Bot** タブの **Privileged Gateway Intents** で **MESSAGE CONTENT INTENT を on にする**

   > **ここが一番詰まる。** off のままだと Discord は接続を拒否ではなく**切断**で返し、totsuka は再接続せずに案内を出して止まる。10,000 ユーザー未満のアプリはトグルするだけでよく、審査も申請も要らない。

4. **OAuth2 → URL Generator** で `bot` スコープを選び、権限は **View Channels / Read Message History / Send Messages / Send Messages in Threads / Create Public Threads**。生成された URL を開いて専用サーバーに招待する

## 3. ID を 2 つ控える

Discord の **設定 → 詳細設定 → 開発者モード** を on にすると、右クリックに「ID をコピー」が出る。

- **自分のユーザー ID**（自分の名前を右クリック）
- **監視チャンネルの ID**（チャンネルを右クリック）

> どちらも**全部数字**になる。名前を貼ってはいけない —— ユーザー ID の側は起動時に弾かれるが、**チャンネル ID の側は弾かれずに何にも一致しない**ので、「誰も使っていない監視」と見分けがつかなくなる。

## 4. `config.toml` を書く

```toml
[[repositories]]
name = "my-docs"
path = "~/Workspace/my-docs"

[plugins.discord]
enabled = true
command = "discord"

[discord]
bot_token = "op://Dev/Discord/bot_token"
operator_user_id = "111111111111111111"

[[workflows]]
name = "discord-clip"
source = "discord"
agent = "herdr"
profile = "implement"
output = "source"
initial_prompt = "/clip-doc 本文中の URL の記事を読み、ai-docs/references/ に要約として残してください。URL が無ければ何もせず終了してください。"
trigger = { channel = "222222222222222222", channel_name = "clip", repo = "my-docs" }
# from = ["333333333333333333"]   # 任意。既定では自分の投稿しかトリガにならない
```

`channel` が ID、`channel_name` は照合用。名前は変えられるので ID を正とし、起動時に実際の名前と突き合わせて食い違えば警告が出る。

## 5. 起動して確かめる

```bash
totsuka config validate
totsuka doctor
totsuka run --watch
```

`discord gateway ready` が出れば接続できている。監視チャンネルに URL を貼ると、タスクが起票され、結果はその投稿から生えたスレッドに返る。

## 詰まりやすい 4 箇所

| 症状 | 原因 | 対処 |
|---|---|---|
| 起動直後に `discord gateway closed with 4014` で止まる | MESSAGE CONTENT INTENT が off | Developer Portal → Bot でトグルを on にして再起動。**再接続を繰り返して回線不調に見えることはなく、その場で止まる** |
| 起動しても `discord gateway ready` が出ない | トークンが拒否されている（`4004`）か、ネットワーク | ログの close code を見る。`4004` ならトークンを確認する |
| 投稿しても何も起きない | ①チャンネル ID ではなく名前を書いた ②bot がそのチャンネルを見られない ③投稿者が `from` に居ない（既定は自分だけ） | ①は ID をコピーし直す ②はチャンネル個別の権限設定を確認する ③は `from` に足す |
| タスクは終わるのに結果が投稿されない | bot に Send Messages in Threads / Create Public Threads が無い | ロールに権限を足す。エラーはログに出る |

## トークンを再発行したとき

Reset Token を押すと**前のトークンは即座に無効**になる。保管先の値を更新して totsuka を再起動する。古いトークンのままだと起動時に止まり、案内が出る。

---

設計上の判断や、この機能がなぜこの形なのかは、リポジトリの `ai-docs/operations/discord-quickstart.md` を参照。
