---
type: Runbook
title: Slack セットアップ Quickstart（task-source-slack）
description: manifest からの Slack アプリ作成 → トークン発行 → Keychain 登録 → plugin install/enable → doctor → run --watch までの導入手順と、トークン失効・スコープ変更時の対処。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-slack
tags: [slack, setup, runbook, keychain, doctor]
timestamp: 2026-07-15T16:00:00Z
status: active
owner: tomoya-k31
---

# ゴール

自分宛の Slack メンションがタスク化され、エージェントの返信案を承認すると本人名義でスレッド返信される状態（[task-source-slack](/components/task-source-slack.md)）。所要 15 分。事前に [トークン取り扱いポリシー](/security/slack-user-token.md) に目を通すこと（社用ワークスペースは特に）。

# 1. Slack アプリを作成（manifest 貼り付け）

1. <https://api.slack.com/apps> → **Create New App** → **From a manifest** → 対象ワークスペースを選択。
2. リポジトリの [`plugins/task-source-slack/manifest.yml`](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) を YAML タブに貼り付けて作成（Bot ユーザーなし・user scopes のみ・Socket Mode 有効の構成）。
3. **Install App**（OAuth & Permissions → Install to Workspace）を実行し、**User OAuth Token**（`xoxp-…`）を控える。
4. **Basic Information → App-Level Tokens → Generate Token and Scopes** で `connections:write` スコープのトークン（`xapp-…`）を生成して控える。

# 2. トークンを Keychain へ登録

```sh
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-app  -w 'xapp-…'
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
target_user_id = "U012AB3CD"        # 自分のメンバー ID
reply_style = "丁寧語で簡潔に"      # 任意

# リポジトリ候補は config.toml の [[repositories]]（name/summary/path）が
# そのまま使われる（#109）。候補を絞る・summary を上書きするときだけ
# [[repos]] を明示する:
# [[repos]]
# name = "web-app"                  # config.toml の [[repositories]].name と一致させる
# summary = "顧客向け Web アプリ"   # 候補が複数あるときの LLM 分類の材料

# 候補が 2 件以上なら分類用 LLM が必須
# [llm]
# base_url = "https://openrouter.ai/api/v1"
# model = "…"
# api_key = "keychain:totsuka/openrouter"
```

# 4. 検証 → 常駐実行

```sh
totsuka config validate   # 静的検証（オフライン）
totsuka doctor            # TokenGuard: auth.test（本人一致）+ apps.connections.open（xapp）
totsuka run --watch       # Socket Mode 常駐 + 5 秒周期の吸い上げ
```

動作確認: 別アカウント（または同僚）に自分宛メンションをしてもらう → エージェント完了後、スレッド内エフェメラル + self-DM に返信案が届く → **承認して返信** で本人名義のスレッド返信、**却下** で破棄（[エフェメラル承認フロー](/glossary/ephemeral-approval.md)）。

# トラブルシューティング

| 症状 | 原因と対処 |
|---|---|
| `doctor` が `invalid_auth` / `token_revoked` | トークン失効。エラーメッセージ内の再発行手順に従い、Keychain を更新（→ [Revoke 手順](/security/slack-user-token.md)） |
| `doctor` が identity mismatch（`target_user_id`） | 他人のトークン、または `target_user_id` の誤記。なりすまし防止で意図的に拒否している |
| メンションがタスク化されない | ①メンション形式が `@自分` か（`user_events` は本人参加チャンネルのみ）②`run --watch` が起動中か ③subtype 付き（編集・bot 投稿）は対象外 |
| 返信案は届くがボタンが失効 | プラグイン再起動で下書きが消えた（in-memory、TTL 24h）。self-DM 記録のテキストから手動返信するか、再メンションで再実行 |
| スコープを変更した | アプリ再インストールが必要 → `xoxp-` が再発行されるので Keychain 更新 → `doctor` で確認（[manifest 雛形](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) のコメント参照） |

# 関連

- [運用ガイド（doctor / worktree 掃除 / FAQ）](operations-guide.md)
- [ADR-0003 設計判断](/decisions/adr-0003-slack-reply-assistant.md)
