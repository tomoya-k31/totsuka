---
type: Playbook
title: Discord セットアップ Quickstart（task-source-discord）
description: "専用サーバーの用意 → Developer Portal でのアプリ/bot 作成 → MESSAGE CONTENT INTENT の有効化 → bot 招待 → id の取得 → config.toml の記述 → doctor → run --watch までの導入手順と、詰まりやすい 4 箇所の対処。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-discord
tags: [operations, playbook, discord, setup, channel-watch]
generated: { by: claude-code/opus-5, at: 2026-09-06T06:00:00+09:00 }
status: stable
owner: tomoya-k31
stale_after: 2027-03-06
---

このファイルは人間向けページの生成元である。編集したら `human-docs` スキルで作り直すこと。

<!-- generates: docs/discord-setup.md docs/discord-setup.ja.md -->

# 前提

- [チャンネル監視トリガ](/glossary/channel-watch.md) を Discord で使う手順。Slack 側は [Slack セットアップ Quickstart](/operations/slack-quickstart.md)
- **専用の Discord サーバーを用意すること**（[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md)）。`MESSAGE_CONTENT` intent は「bot が見えるチャンネル全部」の本文をこのプロセスに流すので、日常会話のサーバーに入れると設定ミス 1 行の被害半径がそこまで広がる

# 手順

## 1. サーバーとチャンネル【人間】

専用サーバーを作り、監視用チャンネル（例 `clip`）を 1 つ作る。

## 2. アプリと bot【人間】

1. <https://discord.com/developers/applications> → New Application
2. **Bot** タブ → Reset Token でトークンを発行して控える（**この画面を離れると再表示できない**。失くしたら再発行になり、再発行すると前のトークンは即座に無効になる）
3. 同じ **Bot** タブの **Privileged Gateway Intents** で **MESSAGE CONTENT INTENT を on にする**

   > **ここが一番詰まる。** off のまま起動すると、Discord は**ハンドシェイクを失敗させずにソケットを閉じる**（close code 4014）。totsuka はこれを恒久エラーとして扱い、再接続せずに案内つきで止まる。1 万ユーザー未満のアプリはトグルするだけでよく、審査も申請も要らない。

4. **OAuth2 → URL Generator** で `bot` スコープ、権限は **View Channels / Read Message History / Send Messages / Send Messages in Threads / Create Public Threads** を選び、生成された URL で専用サーバーに招待する

## 3. id を 2 つ控える【人間】

Discord の **設定 → 詳細設定 → 開発者モード** を on にすると、右クリックメニューに「ID をコピー」が出る。

- **自分のユーザー ID**（自分の名前を右クリック）→ `operator_user_id`
- **監視チャンネルの ID**（チャンネルを右クリック）→ `trigger.channel`

> どちらも**全部数字**になる。ユーザー名やチャンネル名を貼ると、`operator_user_id` は起動時に弾かれるが、`trigger.channel` は**弾かれずに何にも一致しない** —— 誰も使っていない監視と区別がつかないので、必ず ID をコピーすること。

## 4. `config.toml`

```toml
[[repositories]]
name = "my-docs"
path = "~/Workspace/my-docs"

[plugins.discord]
enabled = true
command = "discord"

[discord]
bot_token = "op://Dev/Discord/bot_token"   # 必須
operator_user_id = "111111111111111111"    # 必須。自分のユーザー ID（全部数字）

# 取りこぼし回収の幅（省略時 100 件 / 24 時間。どちらも 0 は拒否される）
# watch_backfill_limit = 100
# watch_backfill_max_age_hours = 24

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

## 5. 起動と確認

```bash
totsuka config validate      # オフライン検査（トークンもネットワークも要らない）
totsuka doctor               # トークンの実解決を含む
totsuka run --watch
```

起動ログに `discord gateway ready` が出れば接続できている。監視チャンネルに URL を貼ると、タスクが起票され、結果はその投稿から生えたスレッドへ **bot 名義で** 返る。

# 詰まりやすい 4 箇所

| 症状 | 原因 | 対処 |
|---|---|---|
| 起動直後に `discord gateway closed with 4014` で止まる | MESSAGE CONTENT INTENT が off | Developer Portal → Bot でトグルを on にして再起動。**これは無症状ではなく、恒久エラーとして止まる** —— 再接続を繰り返して回線不調に見えることはない |
| 起動しても `discord gateway ready` が出ない | intent 以外の理由でトークンが拒否されている（4004）か、ネットワーク | ログの close code を見る。4004 ならトークン、それ以外は再接続を待つ |
| 投稿しても**何も起きない** | ①チャンネル ID ではなく名前を書いた ②bot がそのチャンネルを見られない ③投稿者が `from` に居ない（既定は自分だけ） | ①は ID をコピーし直す。②はチャンネルの権限の上書きを確認。③は `from` に足す |
| 結果が投稿されない（タスクは完了する） | bot に Send Messages in Threads / Create Public Threads が無い | ロール権限を追加。エラーは `result/publish` の失敗としてログに出る |

# トークンを再発行したとき

Reset Token を押すと**前のトークンは即座に無効**になる。保管先の値を更新して totsuka を再起動する。無効なトークンで起動すると `initialize` が `CONFIG_INVALID` で止まり、案内に「Reset Token は新しいトークンを発行する」ことが出る。

# 関連

- [task-source-discord](/components/task-source-discord.md) — 実装
- [ADR-0068](/decisions/adr-0068-channel-watch-trigger.md) — 起動者ゲートと bot 名義投稿の決定
- [起動時バックフィル](/glossary/startup-backfill.md) — 停止中の取りこぼしがどこまで戻るか
