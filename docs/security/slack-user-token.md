---
type: Policy
title: Slack ユーザートークンの取り扱いポリシー
description: task-source-slack が使う User OAuth Token（xoxp）/ App-Level Token（xapp）の保管・権限・漏えい時の Revoke 手順・社用ワークスペースでの確認事項。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-slack
tags: [security, slack, token, keychain, incident]
timestamp: 2026-07-15T15:00:00Z
status: active
owner: tomoya-k31
---

# 前提: このトークンで何ができてしまうか

task-source-slack は Bot ユーザーを持たず、**User OAuth Token（`xoxp-`）で本人として** 動く。このトークンを持つ者は、付与スコープの範囲で **本人になりすませる**:

- `channels:history` / `groups:history` — 本人が参加する公開/プライベートチャンネルの読み取り
- `users:read` — ワークスペースのユーザー情報読み取り
- `chat:write` / `im:write` — **本人名義での投稿**・DM 開始

Bot トークンより影響半径が大きい前提で扱うこと。App-Level Token（`xapp-`）は Socket Mode 接続専用（`connections:write`）で単体では読み書きできないが、イベントの受信（= 傍受）が可能になるため同様に秘匿する。

# 保管ルール

- トークンは **macOS Keychain のみ** に保存し、`plugins/slack.toml` からは `keychain:<service>/<account>` 参照で解決する（F-64/F-65）。設定ファイル・リポジトリ・環境変数ファイルへの平文書き込みは禁止。
- プラグインは Keychain に直接触れない（orchestrator が解決して `initialize` で渡す）。ログは redact 規約（[ログ規約](/development/logging-conventions.md)）に従い、トークンを出力しない。
- トークンローテーションは無効（長命トークン、[ADR-0003](/decisions/adr-0003-slack-reply-assistant.md)）。失効させる手段は下記の Revoke のみ。

# 漏えい時の Revoke 手順

疑いが出た時点で即座に無効化する（順序が重要 — 先に止める、原因調査は後）:

1. **止める**: <https://api.slack.com/apps> → 対象アプリ → **OAuth & Permissions → Revoke All Tokens**（`xoxp-` を即失効）。`xapp-` は **Basic Information → App-Level Tokens** から該当トークンを削除。
2. **確認**: `totsuka run --watch` / `doctor` が `invalid_auth` / `token_revoked` ガイダンスで失敗することを確認（= 旧トークンが死んでいる）。
3. **再発行**: アプリを再インストールして新しい `xoxp-` を取得、`xapp-` を再生成。
4. **差し替え**: Keychain のエントリを更新（`security add-generic-password -U …`）→ `totsuka doctor` で TokenGuard（`auth.test` + `apps.connections.open`）が green になることを確認。
5. **監査**: 漏えい期間中の投稿を Slack の Audit Logs（Business+ 以上）または自分の投稿履歴で確認する。

# 社用ワークスペースでの確認事項

導入前にワークスペース管理者のポリシーを確認する:

- アプリ導入が承認制の場合、[manifest 雛形](https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/manifest.yml) のスコープ一覧で申請する（Bot なし・user scopes のみ・Socket Mode で外部公開エンドポイントなし、が説明ポイント）。
- メッセージの読み取り範囲は「本人が参加しているチャンネル」に限られるが、その内容がローカルのタスク本文・ログ（`log_prompts`）・LLM 分類リクエスト（`plugins/slack.toml` の `[llm]` 先）へ流れることを理解した上で、社外 LLM エンドポイントの利用可否を確認する。
- 返信は承認フロー（[エフェメラル承認フロー](/glossary/ephemeral-approval.md)）を通るため自動送信はないが、承認した投稿の責任は本人名義で発生する。

# 関連

- [Slack セットアップ Runbook](/operations/slack-quickstart.md)
- [task-source-slack コンポーネント](/components/task-source-slack.md)
