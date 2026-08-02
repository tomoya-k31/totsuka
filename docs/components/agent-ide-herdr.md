---
type: Component
title: agent-ide-herdr プラグイン
description: herdr を Agent IDE として接続する公式 agent_ide プラグイン（v1 参照実装）。Orchestrator の JSON-RPC ↔ herdr Socket API（NDJSON）のアダプタで、dispatch/セッション管理/状態ストリーム/plan モード/pane レイアウトを担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [rust, crate, plugin, agent-ide, herdr, socket-api, streaming, hook, deadman, layout]
generated: { by: claude-code/opus-5, at: 2026-08-03T08:10:00+09:00 }
status: stable
owner: tomoya-k31
---

# 必要な herdr のバージョン

**herdr 0.7.5 (protocol 17) 以降が必要**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md)）。
`initialize` が `ping` の `protocol` を検査し、17 未満は `CONFIG_INVALID` で拒否する。
0.7.4 までとの二重実装は持たない — herdr は CI に入っていないので、2 経路のうち片方は
誰も走らせないコードになる。

# 責務

herdr を totsuka の Agent IDE として接続する公式プラグイン（F-30〜F-38）。v1 の参照実装。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、「**Orchestrator 側 JSON-RPC 2.0（NDJSON, stdio）↔ herdr 側 Socket API（NDJSON, Unix ソケット）**」のアダプタとして機能する。詳細設計は一次情報ミラー [herdr Socket API](/references/herdr-socket-api.md) に準拠する。

herdr socket は **JSON-RPC ではなく NDJSON**（1 行 1 メッセージ・`id` 相関）で、メソッドはドット名（`workspace.create` / `pane.split` / `agent.start` / `agent.prompt` / `events.subscribe` / `pane.get` / `pane.read` / `pane.close` 等）。接続モデルは **1 接続 1 リクエスト**（herdr は応答後に接続を閉じる。#124）: 呼び出しごとに接続し、`events.subscribe` だけが持続接続としてイベント封筒 `{event, data}` を push し続ける。JSON-RPC は stdout、診断ログは stderr。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/herdr.toml`（= `InitializeParams.config`）を型付け。`socket_path` / `session`（解決順: `socket_path` > `session` 名 > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > 既定 `~/.config/herdr/herdr.sock`、named session は `sessions/<name>/herdr.sock`）/ `agent_command`（pane で起動する CLI, F-31）/ `plan_args`（plan モードの追加引数, 既定 `--permission-mode plan`）— **`agent_command`/`plan_args`/`launch_command` は 0.2.3（#196）から deprecated フォールバック**（Orchestrator が `tool_launch` を送らない旧世代専用。次の breaking protocol バンプで削除予定）/ **`[kind_map]`**（実行ファイル名 → herdr の `kind`、[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-1）/ `design_preview`（**deprecated・inert**: core もプラグインも読んでおらず、設定しても描画は一切変わらない。pane 配置は `[layout]` が決める。削除は 0.3、[ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)）/ **`[layout]`**（`LayoutConfig { shell, direction, ratio }`。`direction` は herdr の `SplitDirection` を写した enum で `down` / `right` の 2 値のみ、下記）/ `request_timeout_secs`。**`deny_unknown_fields` はネストした `[layout]` にも効く** |
| `state` | herdr `agent_status`→totsuka 正規化状態の写像（`working→running`・`blocked→waiting_input`・`done→done`・`unknown→前値維持`, F-32。**`session/attach` 専用**の写像で、タスク完了はもはやここを通らない — 完了検知はフックが担う）、`(pane_id, agent_session_id)` 復帰ハンドルの `session_id` 文字列へのエンコード（F-37）。**`squash_ws`（折り返し非依存の画面照合ヘルパ）は `agent.prompt` への移行で消滅**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-5）。**質問/回答の画面抽出（旧 `extract_question` / `extract_answer`）は完了判定のフック移行に伴い削除**（#131） |
| `transport` | `HerdrTransport` trait（`call` / `subscribe_events` / `events`）＋ `SocketTransport`。herdr の接続モデルに合わせ **リクエストごとに新規接続**（`call`）+ `events.subscribe` 専用の持続接続（reader タスクが `{event, data}` 封筒を broadcast へ転送）。broadcast はプロセス内の全購読で共有されるため、EOF 時の合成 close イベントは**購読対象 pane ごとに `data.pane_id` 付きで**発行し、他タスクを巻き込まない。`invalid_request` の `id:""` エラーも接続単位で相関。ロジックを fake herdr でテストするための seam |
| `agent` | `HerdrAgent<T: HerdrTransport>`。`dispatch`（`workspace.create`（`env` 付き）→**`apply_layout` = `pane.split`（#356）**→`agent.start {name, kind, pane_id: root_pane, args}`→`agent.prompt`→ハンドル返却, F-31/F-37。呼び出し列と各段の理由は下記）。**プロンプトは `compose_prompt` = `extra_context`（前文）＋ body（無ければ title）**: source が切り詰めた snippet title は body があるとき打たない（ペイン先頭の切れた重複行をなくす）。string の `extra_context`（フック非対応 dispatch 用の可視フォールバック。フック付き dispatch では通常 `None` — 指示文は env `TOTSUKA_PROMPT_CONTEXT` 経由の不可視注入で届く）は JSON 引用符なしの生テキストとして `---` 区切りの**前文**に置く。**protocol 17 で `RetryPolicy` と `submit_prompt` の自己修正手順（#124 / #281）は削除**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-5。`Server::with_retry_policy` / `HerdrAgent::with_retry_policy` も無くなった）。**0.1.3: `hook` 指定時に env を `workspace.create` へ注入し、`args` に `--settings <settings_path>` を付与。`resume_session_id` 指定時は `--resume <id>` も付与**。**0.2.3（#196, [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）: `tool_launch: Option<ToolLaunchSpec>` が Some ならその `args`/`env` をそのまま使い、`program` は**ファイル名を `kind` へ写像**して渡す（`resolve_kind`。protocol 17 が実行ファイルを `kind` から決めるため。CLI フラグ知識は引き続き Orchestrator 側）。None（旧 Orchestrator）のときだけ従来の `launch_command` フォールバック**）/ `attach`（`pane.get` で pane 生存確認・消失（`pane_not_found`）は `attached:false`, F-37）/ `cancel`（`pane.send_keys ["ctrl+c"]`→`pane.close`→**タスクの workspace も close**（pane id `w1:p2` の接頭辞が workspace id。dispatch が workspace を作る以上、pane だけ閉じると空の workspace が残る）, 冪等）/ `release`（**0.2.1: `session/release`**。完了済みセッションの pane + workspace を ctrl+c なしで閉じる。同一性検証つき — 下記 #210 節, [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）/ `snapshot`（**0.1.3: `diagnostics/snapshot`**。`pane.read`（`recent`）で画面テキストを返す。pane 消失は `text: None` でエラーにしない, R-10）/ `focus`（**0.1.4: `session/focus`**。`pane.get` 生存確認 → `workspace.focus`→`tab.focus`→`pane.focus` の外→内チェーン。pane/コンテナ消失は `focused: false` でエラーにしない, F-94）/ `start_state_stream`（`events.subscribe`→**`pane.exited` デッドマン専用**に縮退。異常終了→`Failed`, F-38） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。**`initialize` / `config·validate` が `ping` の `protocol` を検査し、17 未満はバージョンを名指しして拒否**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-6。`protocol` フィールドが無い応答は通す）。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ送出（main が stdout へ、テストはバッファへ排出）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を単独所有し、応答と通知が行途中で交錯しないよう直列化。stdin ループが `SocketFactory`（実ソケット接続）を配線 |

# dispatch の呼び出し列（protocol 17, [ADR-0032](/decisions/adr-0032-herdr-protocol-17.md)）

```text
workspace.create {cwd: worktree, env: hook_env}   → root_pane
pane.split {target_pane_id: root_pane, ...}       → 併設シェル（[layout].shell = true のとき）
agent.start {name, kind, pane_id: root_pane, args, timeout_ms}
agent.prompt {target: root_pane, text, wait}
```

**エージェントは workspace の root pane で動く。** protocol 17 の `agent.start` は pane を作らず、
呼び出し側が用意した pane に起動する。0.7.4 までは `agent.start` が新しい pane を作っていたため、
初期シェル pane が余り、`apply_layout` の最初の仕事がその `pane.close` だった。**その `pane.close` は不要になった**
（実測 23–25 ms、この API 群で最も遅い呼び出し）。

**フック環境変数の注入先は `workspace.create` の `env`。** `agent.start` は `env` を受け付けなくなったが、
herdr は workspace の env を root pane に適用し、そこがエージェントの pane なので `TOTSUKA_JOB_ID` /
`TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` は従来どおり届く。`pane.split` で作る併設シェルは
env を継承しないので、人間が叩くシェルにトークンは載らない（[hook のセキュリティ](/security/hook-security.md)）。

**分割はエージェント起動の前。** 0.7.4 までとは逆順である。エージェント pane が分割元になるので、
先に割っておけば CLI は最終サイズで 1 度だけ描画される。

## `program` → `kind` の写像

`agent.start` は実行ファイルを `kind`（21 値の enum）から決めるため、`ToolLaunchSpec.program` を
そのままは使えない。**ファイル名**で引き、`plugins/herdr.toml` の `[kind_map]` があればそちらを優先する。
値の検証はしない（未知の `kind` は herdr が拒否する。enum を複製すると上流と食い違う）。

これは [ADR-0014](/decisions/adr-0014-tool-abstraction.md) の破棄ではない。核である「CLI フラグの知識を
core の `[tools]` に集める」は保たれ、`args` は不透明なまま渡る。プラグインが負うのは
「program の同一性を herdr の語彙へ翻訳する」責務だけで、これは herdr プロトコルの詳細そのものである。

## `name` の生成

protocol 17 の `name` は表示ラベルではなく**識別子**（`[a-z][a-z0-9_-]{0,31}`、生存中のエージェント間で一意）。
`t-<可読プレフィクス>-<task_id の sha256 先頭 8 桁>` を生成する。ハッシュが要るのは、切り詰めの衝突が
**別タスクとの取り違え**になるからで、可読プレフィクスが要るのは `herdr agent list` を人間が読んで
切り分けられるようにするため。

`agent_name_taken`（同名の生存エージェントがある）は**別名で回避せず dispatch を失敗させる**。
決定論的な名前が衝突するのは、孤児 pane が残っているか `session/release` が失敗したという異常であり、
自動回避すると孤児が積み上がったまま成功し続けて気づく機会が消える。

# プロンプト投入（`agent.prompt`, protocol 17）

**`argv` 末尾にプロンプトを渡す方式は使えない**（複数行だと CLI が投入しない = タスク本文は常に複数行なので必ずハングする）。
これは 17 でも変わらない。

変わったのは投入手段で、**`agent.prompt {target, text, wait}` 1 回で入力と送信が完了する**。
`wait` は `{until: ["working", "blocked", "done"], timeout_ms}` — `working` だけだと、
極端に短いターンが観測される前に settle して取り逃す。herdr 自身が「非 working 状態からの投入では
5 秒以内の状態変化を要求し、無ければ `agent_prompt_stalled` を返す」ので、
**0.7.4 まで自前で組んでいた確認手順はまるごと不要になった**:

- `agent.send` → 画面末尾の空白除去マッチ → 未着弾なら再送
- `agent_status` が動くまでの `enter` 再押下ループと `ENTER_SETTLE` のポーリング（#281）
- `RetryPolicy`（`send_attempts` / `enter_attempts` 等）と、その `Default` を実機値に固定していた unit test（[ADR-0018](/decisions/adr-0018-ci-test-time.md)）

**契約は変わらない**: 送信を確認できなければ dispatch をエラーにする（無言で永久ハングするセッションを作らない）。
`agent_prompt_stalled` がその失敗に写像される。

# dispatch のフック起動（0.1.3, #131）

`TaskDispatchParams.hook`（`HookLaunchSpec = { settings_path, env }`）が Some のとき、dispatch は完了判定フックを載せた Claude Code を起動する:

- **`workspace.create` の params にだけ** `env` を付与（protocol 17 の `agent.start` は `env` を取らない。root pane が継承するので、フック環境変数 `TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` 等はエージェントに届く）
- `agent.start` の `args` に `--settings <settings_path>` を付与（workflow ごとの orchestrator-*.json を読ませ、Stop/SessionEnd フックを有効化）
- `resume_session_id` が Some なら `args` に `--resume <id>` も付与（Slack スレッド会話継続。`--resume` はフックを引き継がないため `--settings` は resume でも必須, H-03）

env 注入・フックの内容は**プラグインにとって不透明**（Orchestrator 側が生成・解釈する。プラグインは値を配線するだけ）。`hook` が None（旧 Orchestrator）でも dispatch は動くが、その場合 env・`--settings` は付かず**完了検知が働かない**（後述）。

# pane レイアウト（`apply_layout`, #356, 0.2.x）

`[layout]`（`shell` / `direction` / `ratio`）で dispatch 時の pane 配置を決める。決定の経緯・不採用案・実測値は [ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)。

**protocol 17 で手順が 1 手減った。** 0.7.4 までは `agent.start` が新しい pane を作っていたので、
`apply_layout` はまず初期シェル pane を `pane.close` する必要があった。17 ではエージェントが
その初期 pane で動くため、余る pane が存在しない。

`agent.start` の**前**に、`shell = true` のときだけ 1 回:

```text
pane.split {target_pane_id: <root pane = これからエージェントが入る pane>,
            direction, ratio, cwd: <worktree>, focus: false}   ← env は渡さない
```

順序・タイミングの理由:

- **エージェント起動の前**: 分割はエージェント pane をリサイズする。先に割っておけば CLI は最終サイズで
  1 度だけ描画される。0.7.4 までは起動の後に割るしかなく、しかも `submit_prompt` が画面照合で
  プロンプト着弾を確認していたため、投入中の折り返し変化がその照合を壊す危険があった（`agent.prompt` への
  移行で照合自体が無くなり、この危険も消えた）
- **`ratio` はそのまま渡る**: herdr の `ratio` は**分割元**（上 / 左）の取り分で、分割元はエージェント pane。
  設定した「エージェント側の取り分」が変換なしで一致する。**17 でも意味は変わらないので、既存の
  `plugins/herdr.toml` を書き換える必要はない**
- **追加の `pane.focus` は呼ばない**: `pane.split` はフォーカスを分割元＝エージェント pane に残す

**split の失敗は `tracing::warn!` で握り潰し dispatch は成功させる**（レイアウトは装飾）。
落ちる先は「エージェント全画面」＝ `shell = false` と同じ正当なレイアウトである。

**`root_pane` が応答に無い場合は dispatch を失敗させる。** 0.7.4 まではレイアウトを諦めるだけで済んだが、
17 ではそれがエージェントを起動する pane そのものなので、代替が無い（pane id を推測して起動すると、
運用者が開いていた別の pane にタスクを打ち込みかねない）。

**セキュリティ上の性質は維持される**: `pane.split` で作るシェルは env を継承しないので、
人間が直接叩くシェルに `TOTSUKA_HOOK_TOKEN` は載らない（[hook-security](/security/hook-security.md)）。
エージェント pane には root pane 経由で載る。

**`session/list` への影響**: split で作るシェル pane は `label = null` なので `totsuka ` 前置フィルタに掛からない。`workspace.close` で巻き添えに閉じるため実害はなく、孤児検出の対象が 1 タスク 1 件に保たれる。

# 状態ストリーム — デッドマン縮退（F-38, #131）

**完了検知はフック機構へ全面移行した**（R-07）。Claude Code の Stop/SessionEnd フックが UDS 経由で Orchestrator へ決定的に完了を通知するため、herdr の screen-manifest（画面パターン認識）由来の完了判定は**廃止**した。旧実装の「`working → idle` 確定」「2 秒デバウンス + `pane.get` 再確認」「`done` 導出」「scrollback からの質問抽出（旧 F-35）」「transcript / detection からの回答回収」は**すべて削除**。

`state/subscribe` は ACK 後に `events.subscribe` を **`pane.exited` のみ**（+ 購読断の合成 close イベント）へ縮退購読する。デッドマンとして働き、**異常終了→`Failed`** を通知して終端する:

- `pane.exited` の `exit_code` が非 0、または**コード無し**（herdr 0.7.x は exit_code を運ばないため clean と確認できない。対話モードの Claude は完了で終了しないので、説明のつかない exit は異常）→ `Failed`
- `exit_code == 0`（clean exit）→ **通知なし**でストリームを終える（正常終了はフック SessionEnd が既報）
- 購読接続の EOF（当該 pane 向け合成 close イベント）→ `Failed`（pane 自体は生きている可能性があるため復旧は `session/attach` 側に委ねる）

`data.pane_id` の自衛フィルタ・イベント区切り文字の正規化（`pane.agent_status_changed` はドット、`pane_exited` はアンダースコア）は継続。この縮退は**無条件**（`hook` None でも同じ）: 旧 Orchestrator + 新プラグインの組合せは `^0.1` 互換上は成立するが完了を検知しなくなるため、`initialize` の `protocol_version` が 0.1.3 未満なら**警告ログ**を出す（orchestrator 側 0.1.3 以上必須）。

# diagnostics/snapshot（R-10, 0.1.3）

`diagnostics/snapshot`（O→P、`diagnostics_snapshot` capability）はタイムアウト/エスカレーション診断のために pane 画面をキャプチャする。`pane.read`（`source = recent`）で画面テキストを返し、pane 消失（や読み取り失敗）は `text: None` で返す — **取得失敗はエラーにしない**ため、Orchestrator のエスカレーション経路がスナップショット不能で失敗することはない。

# session/focus（F-94, 0.1.4）

`session/focus`（O→P、`pane_control` capability。[ADR-0005](/decisions/adr-0005-click-to-focus.md)）は通知 click-to-focus のために対象セッションの pane を herdr 内で前面化する。`pane.get` で生存確認し、pane record の `workspace_id` / `tab_id`（workspace は pane id 接頭辞へフォールバック）を使って **`workspace.focus` → `tab.focus` → `pane.focus` を外→内の順**に呼ぶ（3 メソッドとも herdr 0.7.4 の `herdr api schema --json` で実在確認済み。params は各 id のみ）。pane・コンテナの消失（`*_not_found`）はどの段でも `focused: false` で返し**エラーにしない**（タスク終了後の通知クリックは正常系）。GUI ターミナル自体の前面化は notifier（terminal-notifier `-activate`）の責務で、このプラグインは herdr 内フォーカスのみを担う。

# session/release（#210, 0.2.1）

`session/release`（O→P、`pane_control` capability。[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）は worktree 掃除が「削除する」と判定した**完了済み**セッションの pane + workspace を閉じる。`cancel` との差は 2 点:

1. **ctrl+c を送らない** — 完了済みで中断すべきものが無い。close の対（`pane.close` → workspace 接頭辞の `workspace.close`）は `cancel` と共通の `close_pane_and_workspace` に抽出。
2. **同一性を検証してから閉じる** — `cancel` の盲目クローズは dispatch 直後に走るが、release は保持ポリシー次第で完了から日単位で後に走り、位置ベースの pane id（`w34:p2`）が別の pane に再発番されている窓がある。`pane.get` で live pane を取得し、`expect_cwd`（= worktree パス）/ `expect_label` と `PaneInfo.cwd` / `label` を突き合わせる。**比較可能なペアが1つでも不一致 → 閉じずに `released: false` + warn。全ペア比較不能（nullable フィールドが全部欠落）→ 閉じる（degrade-open）**。pane が既に無い（cancel 済みタスク等）は `released: false` で、**workspace も閉じない**（同一性未検証のまま閉じない）。

# session/list（#211, 0.2.2）

`session/list`（O→P、`pane_control` capability。[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）は `doctor` の孤児 pane 検出のための**自分の所有物の列挙**: herdr の `pane.list`（本プラグイン初使用）を呼び、`label` が **`totsuka ` で始まる pane だけ**を返す。この label フィルタが所有権境界 — herdr はユーザーが手で開いた pane も持ち、それを列挙・解放候補にしてはならない。label は `dispatch` が `workspace.create` に設定する `totsuka {task.id}`（`task.id` はプロトコル `Task.id` = **source task id**。Slack のスレッドキー等の文字列で、orchestrator DB の行 id ではない）で、doctor はこの source task id を `source_task_id` と文字列照合して DB と突き合わせる。返す `session_id` は pane_id + **空 agent_session** の縮退形（`pane.list` は中の Claude セッションを知らないが、`SessionHandle::decode` は bare 形式を受け付け `session/release` は pane さえ分かれば良い）。label / cwd は pane record から取れる範囲でそのまま添える。

# エラー写像 — `SESSION_UNRESUMABLE`（#261, 0.2.4）

herdr 固有のエラー語彙を**プロトコルの語彙へ翻訳するのはこのプラグインの責務**である。herdr は `agent_not_found` と言い、プロトコルは「そのセッションは再開できない」と言う。Orchestrator は後者だけを見て `resume_session_id` なしで 1 回だけ再送する（[orchestrator-core](/components/orchestrator-core.md) #259、[ADR-0015](/decisions/adr-0015-conversation-task-identity.md)）ので、マルチプレクサや `--resume` というフラグの存在を知らずに済む（#196 でツール知識を追い出した設計を保つ）。将来 herdr 以外の agent プラグインが増えても、同じ写像責務をそれぞれが負えば core は無改修で動く。

判定条件は**意図的に狭い**（`resume_failure`）:

- `agent.start` が成功した**後**の失敗に限る。そもそも起動していない pane が resume のせいで死ぬことはなく、それは herdr 側の問題として自分のエラーコードのまま返す
- pane が**消えた**場合に限る（`is_missing()` = `pane_not_found` / `agent_not_found` / `not_found`）。これは実機で観測したバグそのものの形で、`claude --resume <消えた id>` が「該当セッション無し」で即終了し pane ごと落ちた結果 herdr が `agent_not_found` を返していた

厳密にはヒューリスティック（即死の原因が resume だと断定はできない）だが、**誤検知の代償は resume なしの起動 1 回**に限られる。逆に条件を広げる（例: pane は生きているがプロンプトが着弾しない `gave_up` 系も含める）と、単に遅いだけの CLI に対して resume を捨てることになり、**resume が守るはずだった会話文脈を落とす**——狭さはこの損失を避けるためであって、無駄な起動を惜しんでいるのではない。

なお `SESSION_UNRESUMABLE` を返す側は「再送が成功しうる状態」を残す義務がある（プロトコルの契約）。dispatch 失敗時に `abandon` が workspace を畳んでいるため、この条件は既に満たされている。

# capabilities（F-33）

manifest（`plugins/agent-ide-herdr/plugin.toml`）と `initialize` 応答で `kind = agent_ide`・`plan_mode` / `design_preview` / `pane_control` / `state_stream` に加え、**0.1.3 で `resume_session`（`--resume` セッション再開）/ `diagnostics_snapshot`（`diagnostics/snapshot`）**を宣言する（両者は一致させる）。

`[layout]`（#356）は**ケイパビリティを新設せず既存の `pane_control` に相乗り**する（[ADR-0005](/decisions/adr-0005-click-to-focus.md) / [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md) と同じパターン）。`design_preview` は**宣言だけが残っている**状態で、実体は無い（[ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)）。

# Claude Code 固有の制約

対象エージェント Claude Code は **Lifecycle Authority を持たない**（herdr の screen manifest 由来の状態は遅延・取りこぼし・誤検知が構造的に避けられない, #124/#130）。そのため完了判定は**フック（Stop/SessionEnd の command 型 + curl で UDS へ POST）へ移行**し、このプラグインの状態ストリームは `pane.exited` デッドマンに縮退した。plan モードは herdr socket の機能ではなく、CLI 側 permission-mode を pane 起動時に付与して実現する（F-36）。詳細は [herdr Socket API リファレンス](/references/herdr-socket-api.md) 参照。

# テスト

- 状態写像・復帰ハンドル・exit 分類・**`agent_name` の書式と衝突耐性**・**`resolve_kind` の写像**は純関数として単体テスト。`agent_name` は herdr が実際に課す規則（小文字始まり・`[a-z0-9_-]`・32 文字以内）を Slack / GitHub 双方の task id と退化ケース（空文字・記号のみ）で検査し、先頭 21 文字が同じ 2 つの id が別名になること・同じ id が常に同じ名前になることを固定する。
- **実 Unix ソケットの fake herdr サーバ**に対する結合テスト（`tests/integration.rs`）。fake は **herdr 0.7.5 (protocol 17)** を模す: **応答後に接続を閉じる**接続モデル、`{event, data}` 封筒（**ドット/アンダースコア混在**の実イベント名）、`ping` が返す `protocol`、`agent.start {name, kind, pane_id}`、そして入力と送信を 1 回で行う `agent.prompt`。0.7.4 までモデルしていた「入力に反応できるまで `agent.send` / Enter を落とす CLI」は `agent.send` ごと消え、herdr 側の `agent_prompt_stalled` に置き換わった。
- **protocol 17 の固定**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md)）: `initialize` が protocol 16 を**バージョン名指しで拒否**し以後の dispatch も受け付けないこと／`protocol` フィールドの無い `ping` は**通す**こと（未知の形に対して落とさない）／`agent.start` に `argv`/`cwd`/`env` を**送らない**こと・`pane_id` が root pane であること・`kind` が `program` のファイル名から解決されること（絶対パスでも）・`name` が herdr の識別子規則を満たすこと／`agent.prompt` の `wait.until` が `working` だけでなく `blocked`/`done` も含むこと（短いターンの取り逃し防止）／**`agent_name_taken` を別名で回避せず失敗させ、workspace を畳むこと**／`root_pane` の無い応答が **dispatch を失敗させ**、`agent.start` をどこにも撃たないこと。
- 従来からの検証は維持: 始動しない CLI（`agent_prompt_stalled`）で**エラーで失敗する**こと・**フック env が `workspace.create` に乗り `agent.start` には乗らないこと**・`--settings`/`--resume` が `args` に入ること・**`pane.agent_status_changed` を送っても通知が出ないこと（縮退の固定化）**・`pane.exited` 非 0/コード無し→`Failed`・clean exit（0）は通知なし・`diagnostics/snapshot` の正常/pane 消失（`text: null`）両応答・**`session/focus` のフォーカスチェーン**と pane 消失・**`session/release` の各分岐**・他 pane の replay と close 通知を無視すること・`id:""` エラーの即時相関・session/attach の成功と pane 消失・`config/validate` の疎通（ping）。**#261 の `SESSION_UNRESUMABLE` 写像 3 分岐**も維持（resume 指定 + pane 消失 → `-32006`／resume なし + pane 消失 → `-32603`／resume 指定 + pane 生存の別エラー → `-32603`）。
- **`[layout]`（#356）の固定**: 既定（`pane.split` の `direction`/`ratio`/`cwd`/`focus`・**`pane.close` が 1 度も呼ばれないこと**・`pane.split`→`agent.start`→`agent.prompt` の順序）／`shell = false` で split ゼロ／`direction = "right"` と任意 `ratio` がそのまま届くこと／**split 失敗でも dispatch は成功しプロンプトも投入されること**／**`pane.split` に `env` が付かないこと**（フック env は root pane 経由でエージェント側にだけ乗る）。
- **実機手動チェック**（受け入れ #2）: 実 herdr + 実 Claude Code で `--settings` 付き pane 起動 → フック発火 → env（`TOTSUKA_JOB_ID`）がフックスクリプトから見えること（#123 検収環境）は issue #139 のコメントにチェックリストとして整理。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`net`/`io-std` 追加）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](/references/herdr-socket-api.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38・§4.11 F-100〜F-107](/product/orchestrator-spec.ja.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md) / [フックシグナルフロー](/architecture/hook-signal-flow.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
