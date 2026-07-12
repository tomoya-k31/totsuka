---
type: Reference
title: herdr Socket API / 統合エージェント capability（外部一次情報ミラー）
description: herdr の Socket API（NDJSON トランスポート・workspace/pane/agent メソッド・events.subscribe・agent_status）と統合エージェント capability マトリクスの要約。agent_ide プラグイン（#60）設計の根拠。Claude Code は lifecycle authority を持たず状態は screen manifest 由来という制約を含む。
resource: https://herdr.dev/docs/socket-api/
tags: [herdr, socket-api, integration, agent-ide, external]
timestamp: 2026-07-13T00:00:00Z
status: active
owner: tomoya-k31
---

# このドキュメントについて

herdr 公式ドキュメント（[Socket API](https://herdr.dev/docs/socket-api/) / [Integrations](https://herdr.dev/docs/integrations/)）の要約ミラー。
[agent-ide-herdr プラグイン（#60）](/product/orchestrator-spec.ja.md) の詳細設計は、当初 herdr Socket API を推測ベースで書いていたが、
本ドキュメントの一次情報で複数の前提が食い違うことが判明したため、設計の**単一の根拠**として整備した。

> ⚠️ herdr は活発に更新される外部ソフトウェア。依存する前に `herdr status` / `ping` でプロトコル版を確認し、
> 正確な request/response/event スキーマは `herdr api schema --json` で取得すること（未知フィールドは寛容に扱う）。

# トランスポート・接続

| 項目 | 内容 |
|---|---|
| プロトコル | **NDJSON**（1 行 1 メッセージ）。`id` フィールドで request/response を相関。**JSON-RPC ではない** |
| トランスポート | Unix ドメインソケット（Unix）/ 名前付きパイプ（Windows） |
| ソケット解決順 | `--session <name>` > `HERDR_SOCKET_PATH` > `HERDR_SESSION=<name>` > 既定 `~/.config/herdr/herdr.sock` |
| named session | `~/.config/herdr/sessions/<name>/herdr.sock`（セッションごとに別ソケット） |
| ブートストラップ | `session.snapshot` で現在状態（workspace/tab/pane レコード・focused ID・protocol メタ）を一括取得。購読ではないため再接続後に再度呼ぶ |

Request 例 `{"id":"req_1","method":"ping","params":{}}` / Success `{"id":"req_1","result":{"type":"pong"}}` /
Error `{"id":"req_1","error":{"code":"not_found","message":"pane not found"}}`。

herdr が管理プロセスへ注入する環境変数: `HERDR_SOCKET_PATH` / `HERDR_ENV=1` / `HERDR_WORKSPACE_ID` / `HERDR_TAB_ID` /
`HERDR_PANE_ID` / `HERDR_AGENT`（統合別）/ プラグイン向け `HERDR_PLUGIN_ID` 他。herdr 管理変数は呼び出し側指定より優先。

# 主要メソッド

| 分類 | メソッド | 用途 |
|---|---|---|
| workspace | `workspace.create` / `.list` / `.get` / `.close` | ワークスペース（暗黙のセッションコンテナ）。**明示的な「セッション開始」メソッドは無い** |
| pane 情報 | `pane.get` / `pane.current` / `pane.layout` | pane 情報・focused pane・レイアウト取得。pane ID は `w<ws>:p<pane>`（例 `w1:p1`）、tab ID は `w<ws>:t<tab>` |
| 入力 | `pane.send_text` / `pane.send_keys` / `agent.send` | テキスト入力 / キー列（`ctrl+c` `enter` `esc` 等）/ エージェントへのメッセージ |
| 停止 | `pane.close` / `workspace.close` / `server.stop` | pane / ワークスペース / サーバ停止 |
| 状態報告 | `pane.report_agent` / `pane.report_agent_session` | カスタム状態報告 / native セッション ID 報告（公式統合のみ） |
| 購読 | `events.subscribe` | イベントストリーム購読（下記） |

# agent_status と totsuka 正規化状態の対応

herdr の `agent_status` 値は **`idle` / `working` / `blocked` / `done` / `unknown`** の 5 種。totsuka 側の正規化状態
（`idle` / `running` / `waiting_input` / `done` / `failed`、[spec F-32](/product/orchestrator-spec.ja.md)）へは以下で写像する。

| herdr `agent_status` | totsuka 正規化 | 備考 |
|---|---|---|
| `idle` | `idle` | |
| `working` | `running` | |
| `blocked` | `waiting_input` | 人間の入力待ち。質問検知（F-35）の起点 |
| `done` | `done` | |
| `unknown` | 前値維持 / 縮退 | 直接対応する totsuka 状態は無い |
| （native 状態なし） | `failed` | herdr に `failed` は**無い**。`pane.exited`（非 0 終了）等から導出する |

# イベント購読（events.subscribe）

`{"method":"events.subscribe","params":{"subscriptions":[{"type":"pane.agent_status_changed","pane_id":"w1:p1"}]}}`。
初回応答が ACK、以降は同一接続へイベントが push される。

主なイベント種別:
- pane: `pane.agent_status_changed` / `pane.output_matched` / `pane.agent_detected` / `pane.exited` / `pane.created` / `pane.closed` / `pane.scroll_changed` ほか
- workspace / tab / layout / worktree 系（`workspace.created`、`tab.created`、`layout.updated`、`worktree.created` 等）

> **ログ断片（F-38）の限界**: これらイベントは**生 stdout 全文を運ばない**（状態変化・検出・output match が主）。
> 実行ログ断片は pane scrollback / `pane.output_matched` 由来に限定され、herdr socket だけでは全ログを賄えない。

# 統合エージェント capability マトリクス

| Agent | Session Identity | Lifecycle Authority | State Authority | Resume |
|---|---|---|---|---|
| **Claude Code** | ✓ | **✗** | **Screen manifest** | ✓ |
| Codex | ✓ | ✗ | Screen manifest | ✓ |
| GitHub Copilot CLI | ✓ | ✗ | Screen manifest | ✓ |
| Devin CLI | ✓ | ✗ | Screen manifest + OSC | ✓ |
| Kimi Code CLI | ✓ | ✓ | Hook reporting | ✓ |
| Pi | ✗ | ✓ | Extension reporting | ✓ |
| OMP | ✓ | ✓ | Extension + socket API | ✓ |
| Droid | ✓ | ✗ | Screen manifest | ✓ |
| OpenCode | ✓ | ✓ | Plugin reporting | ✓ |
| Kilo Code CLI | ✓ | ✓ | Plugin reporting | ✓ |
| Hermes Agent | ✓ | ✓ | Plugin reporting | ✓ |
| Qoder CLI | ✓ | ✗ | Screen manifest | ✓ |
| Cursor Agent CLI | ✓ | ✗ | Screen manifest | ✓ |
| MastraCode | ✓ | ✓ | Hook reporting only | ✓ |

- **Lifecycle Authority ✓** のエージェントのみ、`idle`/`working`/`blocked` を native に権威報告する。
  ✗ のエージェントは herdr の screen manifest（画面解析）フォールバックに依存し、リアルタイム状態の捕捉が不確実。

# Claude Code 固有の制約（要注意）

agent-ide-herdr は v1 の参照実装だが、対象エージェント Claude Code には次の制約がある。

- **状態の権威を持たない（Lifecycle Authority ✗）**: idle/working/blocked/done は herdr の **screen manifest 検出（画面スクレイピング）由来**。
  Kimi/Pi/OMP/OpenCode/Kilo/Hermes/MastraCode のような native 報告より**信頼性が低い**。
- **hook が報告するのは session identity のみ**: `herdr integration install claude` が `hooks/herdr-agent-state.sh` を書き `settings.json` を更新。
  session start 時に **session ID を報告する**が、lifecycle 状態は報告しない。
- **waiting_input（F-35）**: 「質問中」という構造化 native シグナルは無い。`blocked` 検知＋画面 scrollback からの
  **best-effort 抽出**になる。質問本文は scrollback 抜粋であり、構造化フィールドではない。
- **session/attach（F-37）は問題なし**: Session Identity ✓ / Resume ✓。hook が session ID を報告し、`claude --resume <id>` で会話再開可能。
- **design_preview / pane_control（F-34）**: Claude 統合自体は提供しないが、**herdr の pane/tab/layout API 経由**で totsuka プラグインが実現できる（エージェント非依存の herdr 機能）。
- **plan モード（F-36）は herdr socket の機能ではない**: herdr はプロセスのホストのみ。plan は Claude CLI 側の plan/permission-mode を pane 起動時に付与して実現する。

# Citations

1. herdr — Socket API. https://herdr.dev/docs/socket-api/ （2026-07-13 参照）
2. herdr — Integrations. https://herdr.dev/docs/integrations/ （2026-07-13 参照）
