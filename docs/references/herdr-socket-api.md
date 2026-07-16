---
type: Reference
title: herdr Socket API / 統合エージェント capability（外部一次情報ミラー）
description: herdr の Socket API（NDJSON・1接続1リクエストの接続モデル・workspace/pane/agent メソッド・events.subscribe・agent_status）と統合エージェント capability マトリクスの要約。agent_ide プラグイン（#60/#124）設計の根拠。Claude Code は lifecycle authority を持たず状態は screen manifest 由来（done は発火しない）という制約を含む。
resource: https://herdr.dev/docs/socket-api/
tags: [herdr, socket-api, integration, agent-ide, external]
timestamp: 2026-07-17T00:00:00Z
status: active
owner: tomoya-k31
---

# このドキュメントについて

herdr 公式ドキュメント（[Socket API](https://herdr.dev/docs/socket-api/) / [Integrations](https://herdr.dev/docs/integrations/)）の要約ミラー。
[agent-ide-herdr プラグイン（#60）](/product/orchestrator-spec.ja.md) の詳細設計は、当初 herdr Socket API を推測ベースで書いていたが、
本ドキュメントの一次情報で複数の前提が食い違うことが判明したため、設計の**単一の根拠**として整備した。

**2026-07-17 改訂（#124）**: 実機一気通貫検収（#123）で旧記載と herdr 実機（0.7.1 protocol 14 → 検収中に 0.7.4 protocol 16 へ更新）の
乖離を多数検出したため、実機プローブ（socket 直叩き）+ `herdr api schema --json`（0.7.4 で追加）で確認した値へ全面改訂した。
旧版が正としていた「単一接続の多重化」「`pane.exited` の `exit_code`」は **0.7.1 / 0.7.4 とも存在しない**。
`session.snapshot` は 0.7.1 に無く **0.7.4 で復活**（バージョン依存につき利用は避ける）。

> ⚠️ herdr は活発に更新される外部ソフトウェア。依存する前に `herdr status` / `ping` でプロトコル版を確認し、
> 正確なスキーマは `herdr api schema --json` で取得すること（**0.7.1 以前の CLI に `api` サブコマンドは無い**。
> 未知フィールドは寛容に扱う）。本書は 0.7.4 (protocol 16) 実機で検証済み。

# トランスポート・接続モデル（重要）

| 項目 | 内容 |
|---|---|
| プロトコル | **NDJSON**（1 行 1 メッセージ）。`id`（**文字列必須**）で request/response を相関。**JSON-RPC ではない** |
| トランスポート | Unix ドメインソケット（Unix）/ 名前付きパイプ（Windows） |
| **接続モデル** | **1 接続 1 リクエスト**: 応答（正常・エラーとも）を 1 つ返すとサーバが接続をクローズする。呼び出しごとに接続し直すこと。**例外は `events.subscribe`**（ACK 後も接続が維持され、イベントが push され続ける） |
| `params` | **必須フィールド**。省略すると `invalid_request: missing field 'params'` |
| エラー時の `id` | `invalid_request`（デコード失敗）のエラー応答は **`id: ""`** で返る（リクエストの `id` をエコーしない）。1 接続 1 リクエストなので接続単位で相関するしかない |
| ソケット解決順 | `--session <name>` > `HERDR_SOCKET_PATH` > `HERDR_SESSION=<name>` > 既定 `~/.config/herdr/herdr.sock` |
| named session | `~/.config/herdr/sessions/<name>/herdr.sock`（セッションごとに別ソケット） |
| ブートストラップ | `session.snapshot` は **0.7.1 に存在せず 0.7.4 で復活**（`result.snapshot.{workspaces, focused_*}`）。バージョン非依存にするには `workspace.list` / `pane.list` / `pane.current` / `pane.get` を使う |

Request 例 `{"id":"req_1","method":"ping","params":{}}` / Success `{"id":"req_1","result":{"type":"pong","version":"0.7.1","protocol":14,"capabilities":{...}}}` /
Error `{"id":"req_1","error":{"code":"pane_not_found","message":"pane w1:p9 not found"}}`。
not-found 系のエラーコードは対象別: **`pane_not_found` / `agent_not_found`**（旧記載の汎用 `not_found` ではない）。

herdr が管理プロセスへ注入する環境変数: `HERDR_SOCKET_PATH` / `HERDR_ENV=1` / `HERDR_WORKSPACE_ID` / `HERDR_TAB_ID` /
`HERDR_PANE_ID` / `HERDR_AGENT`（統合別）/ プラグイン向け `HERDR_PLUGIN_ID` 他。herdr 管理変数は呼び出し側指定より優先。

# 主要メソッド（0.7.4 実機で確認済みの形）

有効メソッド一覧（実機の `unknown variant` エラー列挙より）: `ping`, `server.*`, `notification.show`, `client.window_title.*`,
`workspace.create|list|get|focus|rename|close`, `worktree.list|create|open|remove`, `tab.*`,
`agent.list|get|read|explain|send|rename|focus|start`, `pane.split|swap|move|zoom|layout|process_info|neighbor|edges|focus_direction|resize|list|current|get|rename|send_text|send_keys|send_input|read|report_agent|report_agent_session|report_metadata|clear_agent_authority|release_agent|close|wait_for_output`,
`events.subscribe|wait`, `layout.export|apply`, `integration.*`, `plugin.*`。

| メソッド | params（実測） | result（実測） |
|---|---|---|
| `workspace.create` | `{cwd, label?, env?, focus?}`（**`command`/`args` は無い** — 旧記載は誤り） | `{type:"workspace_created", workspace:{workspace_id, number, label, ...}, tab:{tab_id, ...}}`。初期 pane（シェル）1 枚付き |
| `agent.start` | `{name, argv, cwd?, workspace_id?, tab_id?, split?, env?, focus?}`（`name`/`argv` 必須）。**エージェント CLI の起動はこれ** | `{type:"agent_started", agent:{pane_id, terminal_id, workspace_id, tab_id, agent_status, cwd, ...}, argv}` |
| `agent.send` | `{target, text}`。target は terminal id / agent 名 / pane id。**literal text の書き込みのみで Enter は押されない** — 送信確定には `pane.send_keys` で `enter` を送る | ok（不在 target は `agent_not_found`） |
| `pane.send_keys` | `{pane_id, keys}`。**`keys` は配列**（例 `["ctrl+c"]`、`["enter"]`） | ok |
| `pane.get` | `{pane_id}` | `{type:"pane_info", pane:{pane_id, terminal_id, workspace_id, tab_id, focused, cwd, foreground_cwd, agent_status, revision, agent_session?, label?, ...}}`。**`scrollback` フィールドは無い**（旧記載は誤り）。不在は `pane_not_found` |
| `pane.read` | `{pane_id, source, lines?, format?, strip_ansi?}`。`source` ∈ `visible` / `recent` / `recent-unwrapped` / `detection` | `{type:"pane_read", read:{pane_id, workspace_id, tab_id, source, format, text, revision, truncated}}`。**scrollback の代替はこれ**。`strip_ansi: true` で装飾除去（`agent.read {target, ...}` も同形） |
| `pane.list` / `workspace.list` | `{}` | `{type:"pane_list", panes:[...]}` / `{type:"workspace_list", workspaces:[...]}` |
| `pane.close` / `workspace.close` | `{pane_id}` / `{workspace_id}` | `{type:"ok"}` |
| `pane.report_agent_session` | `{pane_id, source, agent, seq, agent_session_id, agent_session_path?}`（公式統合 hook が使用） | ok |

# agent_status と totsuka 正規化状態の対応

herdr の `agent_status` 語彙は **`idle` / `working` / `blocked` / `unknown`**（`agent.wait --status` の受理値）。
公式ドキュメントは `done` にも言及するが、**screen manifest 検出のエージェント（Claude Code 等）では `done` は発火しない**
（完了を報告する native 経路が無い）。totsuka 側の正規化（[spec F-32](/product/orchestrator-spec.ja.md)）は以下。

| herdr `agent_status` | totsuka 正規化 | 備考 |
|---|---|---|
| `working` | `running` | |
| `blocked` | `waiting_input` | 人間の入力待ち。質問検知（F-35）は `pane.read`（`visible`）からの best-effort 抽出 |
| `idle` | `idle`、ただし **`running` からの遷移は実質の完了シグナル** | Claude Code は `done` を報告しないため、`working → idle`（応答終了）を完了として扱うしかない（#124。誤検知緩和に再確認推奨） |
| `done` | `done` | native 報告できる統合（Kimi/OMP 等）のみ |
| `unknown` | 前値維持 / 縮退 | 直接対応する totsuka 状態は無い |
| （native 状態なし） | `failed` | herdr に `failed` は**無い**。完了前の `pane_exited`（下記）等から導出する |

# イベント購読（events.subscribe）

`{"id":"sub_1","method":"events.subscribe","params":{"subscriptions":[{"type":"pane.agent_status_changed","pane_id":"w1:p1"}]}}`
→ ACK `{"id":"sub_1","result":{"type":"subscription_started"}}`。以降は**同一接続**へイベントが push され続ける（この接続だけは維持される）。

**イベント配送形式（実測）**: 購読エントリの `type` はドット名だが、配送は
`{"event":"pane_exited","data":{"pane_id":"w1:p1","workspace_id":"w1","type":"pane_exited", ...}}` という
**`{event, data}` 封筒 + アンダースコア名**。旧記載のトップレベル `{"type":"pane.exited", ...}` 形ではない。

購読可能な type（実機列挙）: `workspace.created|updated|renamed|closed|focused`, `worktree.created|opened|removed`,
`tab.created|closed|focused|renamed`, `pane.created|closed|focused|moved|exited|agent_detected|output_matched|agent_status_changed`。

注意点（実測）:
- **`pane_exited` に `exit_code` は無い**（`data` は `pane_id` / `workspace_id` / `type` のみ）。終了の成否分類はできないため、
  「完了（`working → idle`）前の exit = 異常」のように**状態履歴から導出**する。
- **購読直後に過去イベントの replay が届くことがある**（他 pane・購読前に終了した pane の `pane_exited` を観測）。
  購読側は必ず `data.pane_id` で自衛フィルタすること。
- 存在しない pane の購読はエラー（`internal_error: failed to decode pane get error`、応答 `id` は `<id>:sub:<n>:probe` 形式）。

> **ログ断片（F-38）の限界**: イベントは**生 stdout 全文を運ばない**（状態変化・検出・output match が主）。
> 実行ログ・最終出力は `pane.read`（`recent` 等）で別途取得するしかない。pane 終了後は読めないため、**取得は pane 生存中に行う**。

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

- **状態の権威を持たない（Lifecycle Authority ✗）**: idle/working/blocked は herdr の **screen manifest 検出（画面スクレイピング）由来**。
  native 報告（Kimi/Pi/OMP/OpenCode/Kilo/Hermes/MastraCode）より**信頼性が低く、`done` は発火しない**。
- **hook が報告するのは session identity のみ**（integration v7 実機確認済み）: `herdr integration install claude` が
  `hooks/herdr-agent-state.sh` を書き、`pane.report_agent_session`（session ID + transcript path）**だけ**を送る。
  lifecycle 状態は一切報告しない。SubagentStop は idle pane を復活させないため意図的に無視される。
- **完了検知**: `done` が来ない + `pane_exited` に exit_code が無い + 対話モードの Claude は回答後も終了しない。
  よって完了は **`working → idle` 遷移**で判定し、最終出力は pane 生存中に `pane.read` で回収する（#124 の設計）。
- **waiting_input（F-35）**: 「質問中」という構造化 native シグナルは無い。`blocked` 検知＋ `pane.read`（`visible`）からの
  **best-effort 抽出**になる。質問本文は画面抜粋であり、構造化フィールドではない。
- **session/attach（F-37）は問題なし**: Session Identity ✓ / Resume ✓。hook が session ID を報告し（`pane.get` の
  `pane.agent_session` に反映）、`claude --resume <id>` で会話再開可能。
- **design_preview / pane_control（F-34）**: Claude 統合自体は提供しないが、**herdr の pane/tab/layout API 経由**で totsuka プラグインが実現できる（エージェント非依存の herdr 機能）。
- **plan モード（F-36）は herdr socket の機能ではない**: herdr はプロセスのホストのみ。plan は Claude CLI 側の plan/permission-mode を pane 起動時に付与して実現する。

# Citations

1. herdr — Socket API. https://herdr.dev/docs/socket-api/ （2026-07-13 / 2026-07-17 参照）
2. herdr — Integrations. https://herdr.dev/docs/integrations/ （2026-07-13 参照）
3. herdr 0.7.1 (protocol 14) / 0.7.4 (protocol 16) 実機プローブ記録（socket 直叩き + `herdr api schema --json`）: #123 / #124 のコメント（2026-07-17）
