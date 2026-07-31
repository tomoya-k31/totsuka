---
type: Runbook
title: Slack セットアップ Quickstart（task-source-slack）
description: manifest からの Slack アプリ作成 → トークン発行 → Keychain 登録 → plugin install/enable → doctor → run --watch までの導入手順と、トークン失効・スコープ変更時の対処。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-slack
tags: [slack, setup, runbook, keychain, doctor]
generated: { by: human:tomoya-k31, at: 2026-07-28T00:00:00Z }
status: stable
owner: tomoya-k31
---

# ゴール

自分宛の Slack メンションがタスク化され、エージェントの返信案を承認すると本人名義でスレッド返信される状態（[task-source-slack](/components/task-source-slack.md)）。所要 15 分。事前に [トークン取り扱いポリシー](/security/slack-user-token.md) に目を通すこと（社用ワークスペースは特に）。

# 1. Slack アプリを作成（manifest 貼り付け）

1. <https://api.slack.com/apps> → **Create New App** → **From a manifest** → 対象ワークスペースを選択。
2. リポジトリの [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) を YAML タブに貼り付けて作成（会話に見える投稿はすべて user scopes = 本人名義。bot user は通知ナッジ DM 専用 — [ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)・#305。Socket Mode 有効の構成）。
3. **Install App**(OAuth & Permissions → Install to Workspace)を実行し、**User OAuth Token**（`xoxp-…`）と **Bot User OAuth Token**（`xoxb-…`、同じページ）を控える。
4. **Basic Information → App-Level Tokens → Generate Token and Scopes** で `connections:write` スコープのトークン（`xapp-…`）を生成して控える。

# 2. トークンを Keychain へ登録

```sh
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-app  -w 'xapp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'   # 通知ナッジを使う場合
```

自分の Slack ユーザー ID（`U…`）も控える: Slack のプロフィール → **…** → **メンバー ID をコピー**。

# 3. plugin install / enable と設定

```sh
totsuka plugin install ./plugins/task-source-slack
totsuka plugin enable slack
```

`~/.config/totsuka/config.toml`（キーの意味は [設定リファレンス](/development/config-reference.md)）:

```toml
[plugins.slack]
enabled = true
kind = "task_source"
poll_interval_secs = 5   # Socket Mode バッファの吸い上げ周期（推奨）

[[workflows]]
name = "slack-reply"
source = "slack"
trigger = {}
mode = "plan"            # 返信起案は plan（push/PR なし）で十分
agent = "herdr"
output = "source"        # result/publish → 承認フローへ
```

`~/.config/totsuka/plugins/slack.toml`:

```toml
app_token = "keychain:totsuka/slack-app"
user_token = "keychain:totsuka/slack-user"
bot_token = "keychain:totsuka/slack-bot"    # 任意: 返信案/ピッカー到着の通知 DM（#305）。
                                            # 省略するとナッジなし（それ以外は同じ動作）
target_user_id = "U012AB3CD"        # 自分のメンバー ID
reply_style = "丁寧語で簡潔に"      # 任意

# 任意: 自分が付けるとタスクを起こす絵文字（#319）。省略 = 無効（既定）。
# 自分宛でないメッセージを、会話にノイズを足さずタスク化できる。
# 他人が同じ絵文字を付けても起動しない（緩和する設定は無い）。
# 名前はコロン有無どちらでも可。👀 は eyes、👁 は eye で別物。
trigger_reactions = ["eyes"]

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
# api_key = "keychain:totsuka/openrouter"
```

# 4. 検証 → 常駐実行

```sh
totsuka config validate   # 静的検証（オフライン）
totsuka doctor            # TokenGuard: auth.test（本人一致）+ apps.connections.open（xapp）
                          # + bot_token 設定時は auth.test（xoxb）も probe
totsuka run --watch       # Socket Mode 常駐 + 5 秒周期の吸い上げ
```

動作確認: 別アカウント（または同僚）に自分宛メンションをしてもらう → エージェント完了後、スレッド内エフェメラル + self-DM に返信案が届く（`bot_token` 設定時は bot からの通知 DM も届く — エフェメラル/self-DM 自体は Slack 通知を発生させないため、これが唯一の push） → **承認して返信** で本人名義のスレッド返信、**却下** で破棄（[エフェメラル承認フロー](/glossary/ephemeral-approval.md)）。

# トラブルシューティング

| 症状 | 原因と対処 |
|---|---|
| `doctor` が `invalid_auth` / `token_revoked` | トークン失効。エラーメッセージ内の再発行手順に従い、Keychain を更新（→ [Revoke 手順](/security/slack-user-token.md)） |
| `doctor` が identity mismatch（`target_user_id`） | 他人のトークン、または `target_user_id` の誤記。なりすまし防止で意図的に拒否している |
| メンションがタスク化されない | ①メンション形式が `@自分` か（`user_events` は本人参加チャンネルのみ）②`run --watch` が起動中か ③subtype 付き（編集・bot 投稿）は対象外 |
| リアクションを付けてもタスク化されない | ①`trigger_reactions` が設定されているか（既定は空 = 無効）②絵文字名が一致しているか（👀 は `eyes`、👁 は `eye`。カスタム絵文字の alias は「実際に押された名前」で届くので alias を使うなら両方列挙）③**付けたのが自分か**（他人のリアクションでは起動しない。緩和する設定は無い — [ADR-0025](/decisions/adr-0025-reaction-task-trigger.md)）④`reactions:read` を含む manifest で再インストール済みか（スコープが無いとイベント自体が届かず、**エラーにもならない**）⑤同じメッセージを既に mention 経由で処理していないか（dedup は共有） |
| リアクションを付け直しても再実行されない | 意図した挙動。dedup キーが `{channel}:{メッセージの ts}` なので、**成功したものは付け直しても再実行しない**（誤って外して付け直しただけで二重にエージェントが走る方が事故が大きい）。ただし**取得に失敗した場合は付け直しで再試行できる**（失敗時はキーを消費しない）。強制的に再実行するならプロセス再起動で LRU が消える |
| 返信案は届くがボタンが失効 | TTL 24h 超過、または FIFO 追い出し（上限 1024 件）。self-DM 記録のテキストから手動返信するか、再メンションで再実行（#122 以降、下書きは `~/.local/state/totsuka/plugins/{source_name}/drafts.json` に永続化されるため再起動ではボタンは失効しない） |
| スコープを変更した | アプリ再インストールが必要 → **`xoxp-` と `xoxb-` の両方が再発行される**ので Keychain を両方更新 → `doctor` で確認（[manifest 雛形](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) のコメント参照）。既存アプリへ bot user を後から足す場合（#305）も同じ — `slack-bot` を追加するだけだと再発行済みの `xoxp-` が死んだままになる |
| 通知ナッジ（bot DM）が届かない | ① `bot_token` が未設定/失効（`doctor` の bot probe を確認）② 起動ログに bot DM 解決失敗の WARN がないか ③ Slack 側でこのアプリの DM をミュートしていると push は出ない（コードでは解決不能） |
| prefix ルール（`[[channel_groups]]`）が効かず常に LLM/エフェメラル選択になる | `conversations.info` が `missing_scope` で失敗しチャンネル名が取れていない（ログ WARN 参照）。`channels:read` / `groups:read` を含む manifest でアプリを再インストール → Keychain 更新（上の「スコープを変更した」と同手順） |

# 関連

- [運用ガイド（doctor / worktree 掃除 / FAQ）](operations-guide.md)
- [ADR-0003 設計判断](/decisions/adr-0003-slack-reply-assistant.md)
