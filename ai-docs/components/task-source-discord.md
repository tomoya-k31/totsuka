---
type: Component
title: task-source-discord プラグイン
description: "Discord のチャンネル監視をタスクソースとして接続する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。Gateway WebSocket で MESSAGE_CREATE を受け、監視チャンネルへのトップレベル投稿を Task へ正規化し、結果を bot 名義でその投稿のスレッドへ返す。self-bot 禁止により本人名義投稿・承認フローは持たない薄い設計。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-discord
tags: [rust, crate, plugin, task-source, discord, gateway, websocket, channel-watch]
generated: { by: claude-code/opus-5, at: 2026-09-06T06:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: discord-gateway
    resource: https://docs.discord.com/developers/events/gateway
    title: Discord — Gateway（接続・intent・RESUME）
  - id: discord-self-bots
    resource: https://support.discord.com/hc/en-us/articles/115002192352-Automated-User-Accounts-Self-Bots
    title: Discord — Automated User Accounts (Self-Bots)
  - id: discord-rate-limits
    resource: https://docs.discord.com/developers/topics/rate-limits
    title: Discord — Rate Limits
stale_after: 2027-03-06
---

# 責務

Discord の[チャンネル監視トリガ](/glossary/channel-watch.md)を totsuka のタスクソースとして接続する公式プラグイン。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、stdio JSON-RPC 2.0（NDJSON）サーバとして起動する。トークンは `initialize` の config で解決済みのものを受領し（F-65）、プラグイン自身は Keychain に触れない。

# なぜ Slack より薄いのか

[task-source-slack](/components/task-source-slack.md) の重心は「自分宛のメンションに、**本人名義で**、下書きと承認ゲートを挟んで返す」ことにある。**Discord ではその全部が成立しない** —— 通常ユーザーアカウントの自動化（self-bot）は Terms of Service で禁止され[^discord-self-bots]、アプリが投稿できるのは bot 名義だけである。bot の声のまわりに承認フローを組み直すと、機構だけ残って理由が消える。

したがってこのプラグインは **チャンネル監視の取り込みと結果投稿だけ**を行う。承認ゲート・下書きストア・エフェメラル・リポジトリの LLM 分類はいずれも持たない（リポジトリは `trigger.repo` で固定される）。決定と不採用案は [ADR-0068](/decisions/adr-0068-channel-watch-trigger.md)。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `[discord]` の設定スキーマ。`bot_token` と `operator_user_id` が必須で、後者は**起動者ゲートの比較対象**そのもの。**全桁が数字であることを検査する** —— ここに username を書くのはよくある間違いで、id と違って誰にも一致しないため、放置すると「誰も使っていない監視」と見分けがつかなくなる。`watch_backfill_limit` / `watch_backfill_max_age_hours` は[起動時バックフィル](/glossary/startup-backfill.md)の窓 |
| `error` | `DiscordError` と**復旧手順つきの文言**。`is_credential()` が 2 箇所で効く: `initialize` が `CONFIG_INVALID` と `INTERNAL_ERROR` を分ける判定と、Gateway ループが**再接続せず止まる**判定。直らない失敗を再接続し続けると、ログ上は回線不調と区別がつかなくなる。`gateway_close_failure` は close code ごとの案内を持ち、特に **4014 は「Developer Portal → Bot → MESSAGE CONTENT INTENT を on」**まで名指しする（1 万ユーザー未満はトグルのみで審査不要） |
| `transport` | REST の継ぎ目（`DiscordTransport` trait）。テストは記録済みレスポンスで全経路を回す。`retry_after_delay` は Discord が返す `retry_after` をそのまま尊重しつつ上限で刈る。`classify_status` が 401/403 を資格情報クラスへ寄せる |
| `http` | 本番トランスポート（reqwest）。認証スキームは **`Bearer` ではなく `Bot`** —— 間違えると 401 になるだけで、語が原因だとは分からない。**429 は常に再送可**（要求は拒否されたのであって実行されていない）だが、**送信失敗した非冪等呼び出しは再送しない**（届いていた場合に二重投稿になる）。5xx のみ指数バックオフ、4xx は即返す |
| `discord_api` | 使う 4 ルートの型付きラッパ: `GET /users/@me`（トークンガード兼 bot 自身の id）、`GET /channels/{id}`（改名検知用の実名）、`GET /channels/{id}/messages`（バックフィル）、`POST …/messages` と `POST …/messages/{id}/threads`（結果投稿）。`is_human_post()` が bot・webhook・システムメッセージを落とす —— **webhook 投稿の author には `bot` フラグが無い**ので、そこだけ見ると通り抜ける。`snowflake_for()` は「now − max_age」の合成 snowflake を作る（snowflake は生成時刻を内包するので、余分な往復なしで `after` の下限になる） |
| `gateway` | Gateway プロトコルの純粋な部分[^discord-gateway]: intent ビット（`GUILD_MESSAGES \| MESSAGE_CONTENT` の 2 つだけ。増やすとこのプロセスに流れ込む範囲が広がる）、`IDENTIFY` / `RESUME` / `HEARTBEAT` のペイロード、`close_code_is_permanent()`、`step()` によるフレーム分類。**`step()` が seq を進めるのは読まないイベントでも**行う —— 古い seq から resume すると、それ以降が全部再配送される |
| `watch` | 判定表と `Task` 生成。**「スレッド返信を除く」行が無い**のは、Discord ではスレッドがチャンネルであり、返信はスレッド自身の `channel_id` を持つため構造的に弾かれるから。表は ①監視チャンネル ②人間の投稿のみ ③bot 自身の id を除外 ④起動者ゲート。`task_id` は `{prefix:}{channel}:{message}`、**`message_key` は付けない**（id が既にその 1 投稿を名指すので、2 回目の配送は id で止まる = at-most-once） |
| `pipeline` | 起動時のチャンネル名照合（改名を warn、失敗しても続行）、バックフィル、1 メッセージの submit、結果投稿。`SharedState` は `PendingPost`（channel / message / author）を task id で持つ。座標は task id から**導出可能**だが記録する —— `result/publish` に id の書式を解析させると、prefix を足した日に壊れる |
| `run` | 常駐 Gateway ループ。接続 → `HELLO` → `IDENTIFY` か `RESUME` → heartbeat → フレーム消費。終わり方を 3 つに分類する: **Permanent**（止まる）・**Resumable**（resume して再接続）・**Restart**（session を捨て、再接続してバックフィル）。4007 / 4009 は再接続してよいが session は無効なので Restart に倒す |
| `server` | JSON-RPC dispatch。`initialize` で設定検査 → trigger 解決 → **監視が 0 本ならエラー**（このソースには他のトリガ種別が無く、何もしないまま起動するのは常に間違い）→ トークンガード → 常駐ランタイム起動。`config/validate` は**意図的にオフライン**で、`doctor` がネットワークもトークンも要らない。`task/update_status` は no-op だが**成功を返す**（失敗にすると全タスクが失敗に見える） |

# capabilities（F-83）

`outputs = ["source"]` のみ。`claimed_options` は空 —— このソースが読む設定は全部 `trigger` の中にあり、`[[workflows]]` の自前キーは持たない。読まないキーを claim するとタイポが沈黙に変わる。

# プロトコル下限

`protocol_version = ">=0.6.0, <0.7"`。監視トリガは `InitializeParams.workflows`（0.6.0、#554）で届き、このプラグインには他に監視対象を知る手段が無い。それより古いホストでは**何もしないまま起動してしまう**ので、F-54 のゲートで起動拒否に倒している。

# レート制限

REST はルート別バケット + グローバル 50 req/s[^discord-rate-limits]。このプラグインの平常時の REST 呼び出しは「起動時のチャンネル名照合 + バックフィル 1 回」と「結果投稿 1〜2 回」だけで、常時トラフィックは Gateway 側にある。

# テスト

- ユニット: 設定検査・エラー分類と文言・トランスポートのバックオフ/429・メッセージ解析（webhook とシステムメッセージ）・snowflake の時刻符号化・Gateway のフレーム分類と close code・判定表・pending index
- CLI レベル（`tests/integration.rs`）: 記録済みトランスポートで `initialize`（正常・トークン拒否・監視 0 本・未知 repo・trigger キーのタイポ）、`config/validate` がオフラインであること、`result/publish` の座標欠落、`task/update_status` の no-op

**実機検証は未了**。Gateway・intent・スレッド作成は記録済みレスポンスまでしか確認していない。

# 依存

- `plugin-protocol` / `plugin-sdk` / `serde` / `serde_json` / `reqwest` / `tokio`（io-std, net）/ `tokio-tungstenite` / `futures-util` / `thiserror` / `tracing`

# 関連

- [ADR-0068 チャンネル監視トリガ](/decisions/adr-0068-channel-watch-trigger.md)
- [チャンネル監視トリガ](/glossary/channel-watch.md) / [起動時バックフィル](/glossary/startup-backfill.md)
- [task-source-slack](/components/task-source-slack.md) — 同じトリガの Slack 側
- [Discord セットアップ Quickstart](/operations/discord-quickstart.md)

[^discord-self-bots]: Discord — Automated User Accounts (Self-Bots)
[^discord-gateway]: Discord — Gateway（接続・intent・RESUME）
[^discord-rate-limits]: Discord — Rate Limits
