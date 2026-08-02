---
type: Component
title: agent-ide-herdr プラグイン
description: herdr を Agent IDE として接続する公式 agent_ide プラグイン（v1 参照実装）。Orchestrator の JSON-RPC ↔ herdr Socket API（NDJSON）のアダプタで、dispatch/セッション管理/状態ストリーム/plan モード/pane レイアウトを担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [rust, crate, plugin, agent-ide, herdr, socket-api, streaming, hook, deadman, layout]
generated: { by: claude-code/opus-5, at: 2026-08-01T20:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# ⚠️ 既知の非互換: herdr 0.7.5 (protocol 17) では dispatch が動かない

本ドキュメントが記述する実装は **herdr 0.7.4 (protocol 16) までを前提**にしている。
0.7.5 (protocol 17) では `agent.start` が manifest 駆動へ、プロンプト投入が `agent.prompt` へ
破壊的に変わっており、`task/dispatch` は
`invalid_request: missing field 'kind'` で失敗する（2026-08-03 の実機検証で検出）。

差分と 17 での正しい呼び出し列は [herdr Socket API](/references/herdr-socket-api.md) の
2026-08-03 改訂節を参照。以下の「プロンプト投入」「dispatch のフック起動」「pane レイアウト」の
各節は **0.7.4 までの記述**であり、17 対応の実装が入るまでこの注記が正である。

# 責務

herdr を totsuka の Agent IDE として接続する公式プラグイン（F-30〜F-38）。v1 の参照実装。[plugin-protocol](/components/plugin-protocol.md) を実装する単体バイナリで、「**Orchestrator 側 JSON-RPC 2.0（NDJSON, stdio）↔ herdr 側 Socket API（NDJSON, Unix ソケット）**」のアダプタとして機能する。詳細設計は一次情報ミラー [herdr Socket API](/references/herdr-socket-api.md) に準拠する。

herdr socket は **JSON-RPC ではなく NDJSON**（1 行 1 メッセージ・`id` 相関）で、メソッドはドット名（`workspace.create` / `agent.start` / `events.subscribe` / `pane.get` / `pane.read` / `pane.close` 等）。接続モデルは **1 接続 1 リクエスト**（herdr は応答後に接続を閉じる。#124）: 呼び出しごとに接続し、`events.subscribe` だけが持続接続としてイベント封筒 `{event, data}` を push し続ける。JSON-RPC は stdout、診断ログは stderr。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `config` | `plugins/herdr.toml`（= `InitializeParams.config`）を型付け。`socket_path` / `session`（解決順: `socket_path` > `session` 名 > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > 既定 `~/.config/herdr/herdr.sock`、named session は `sessions/<name>/herdr.sock`）/ `agent_command`（pane で起動する CLI, F-31）/ `plan_args`（plan モードの追加引数, 既定 `--permission-mode plan`）— **`agent_command`/`plan_args`/`launch_command` は 0.2.3（#196）から deprecated フォールバック**（Orchestrator が `tool_launch` を送らない旧世代専用。次の breaking protocol バンプで削除予定）/ `design_preview`（**deprecated・inert**: core もプラグインも読んでおらず、設定しても描画は一切変わらない。pane 配置は `[layout]` が決める。削除は 0.3、[ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)）/ **`[layout]`**（`LayoutConfig { shell, direction, ratio }`。`direction` は herdr の `SplitDirection` を写した enum で `down` / `right` の 2 値のみ、下記）/ `request_timeout_secs`。**`deny_unknown_fields` はネストした `[layout]` にも効く** |
| `state` | herdr `agent_status`→totsuka 正規化状態の写像（`working→running`・`blocked→waiting_input`・`done→done`・`unknown→前値維持`, F-32。**`session/attach` 専用**の写像で、タスク完了はもはやここを通らない — 完了検知はフックが担う）、`(pane_id, agent_session_id)` 復帰ハンドルの `session_id` 文字列へのエンコード（F-37）、`squash_ws`（`submit_prompt` の着弾確認に使う折り返し非依存の照合ヘルパ）。**質問/回答の画面抽出（旧 `extract_question` / `extract_answer`）は完了判定のフック移行に伴い削除**（#131） |
| `transport` | `HerdrTransport` trait（`call` / `subscribe_events` / `events`）＋ `SocketTransport`。herdr の接続モデルに合わせ **リクエストごとに新規接続**（`call`）+ `events.subscribe` 専用の持続接続（reader タスクが `{event, data}` 封筒を broadcast へ転送）。broadcast はプロセス内の全購読で共有されるため、EOF 時の合成 close イベントは**購読対象 pane ごとに `data.pane_id` 付きで**発行し、他タスクを巻き込まない。`invalid_request` の `id:""` エラーも接続単位で相関。ロジックを fake herdr でテストするための seam |
| `agent` | `HerdrAgent<T: HerdrTransport>`。`dispatch`（`workspace.create`→`agent.start`（プロンプトなし）→**`apply_layout`（#356、下記）**→`submit_prompt`→ハンドル返却, F-31/F-37。**プロンプトは `compose_prompt` = `extra_context`（前文）＋ body（無ければ title）**: source が切り詰めた snippet title は body があるとき打たない（ペイン先頭の切れた重複行をなくす）。string の `extra_context`（フック非対応 dispatch 用の可視フォールバック。フック付き dispatch では通常 `None` — 指示文は env `TOTSUKA_PROMPT_CONTEXT` 経由の不可視注入で届く）は JSON 引用符なしの生テキストとして `---` 区切りの**前文**に置く — `submit_prompt` の着弾確認はプロンプト**末尾**の画面照合（`prompt_marker`）。**#281: Enter 確認ループのポーリング化** — `ENTER_SETTLE`（1.2 秒）は固定スリープではなく `POLL_INTERVAL` 刻みの**締切**として扱い、pane が `working` を報告した時点で即座に返す（従来は成功した押下も必ず 1.2 秒寝てから次周回の先頭で成功を観測していたため、成功する dispatch が毎回 1.2 秒を無駄に払っていた）。押下回数・最悪時の所要時間・諦め時のエラーは不変。**#281: リトライのタイミングは `RetryPolicy`** （`agent.rs` の `pub struct`、`Default` が本番値）へ切り出され、`HerdrAgent::with_retry_policy` / `Server::with_retry_policy` で注入できる。本番経路は `Server::new` のままで既定値を使い、既定値以外を構築するのは結合テストだけ（待ち時間のみ縮め、`send_attempts`/`enter_attempts` の**回数は本番値を維持**する — 諦め系テストが回数そのものを検証しているため）。詳細は [ADR-0018](/decisions/adr-0018-ci-test-time.md)であり、繰り返され得る指示文を末尾に置くと dispatch の末尾が同一化して `--resume` ペインの前ターン描画に誤マッチするため、一意なタスク本文を末尾に保つ。**0.1.3: `hook` 指定時に env を `workspace.create`/`agent.start` へ注入し、argv に `--settings <settings_path>` を付与。`resume_session_id` 指定時は `--resume <id>` も付与**。**0.2.3（#196, [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）: `tool_launch: Option<ToolLaunchSpec>` が Some なら、その `program`/`args`/`env` を**そのまま**起動（`resolve_launch`）— CLI フラグ知識は Orchestrator の `[tools]` レジストリ側に移り、本プラグインは組み立てない。None（旧 Orchestrator）のときだけ従来の `launch_command` フォールバック**）/ `attach`（`pane.get` で pane 生存確認・消失（`pane_not_found`）は `attached:false`, F-37）/ `cancel`（`pane.send_keys ["ctrl+c"]`→`pane.close`→**タスクの workspace も close**（pane id `w1:p2` の接頭辞が workspace id。dispatch が workspace を作る以上、pane だけ閉じると空の workspace が残る）, 冪等）/ `release`（**0.2.1: `session/release`**。完了済みセッションの pane + workspace を ctrl+c なしで閉じる。同一性検証つき — 下記 #210 節, [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）/ `snapshot`（**0.1.3: `diagnostics/snapshot`**。`pane.read`（`recent`）で画面テキストを返す。pane 消失は `text: None` でエラーにしない, R-10）/ `focus`（**0.1.4: `session/focus`**。`pane.get` 生存確認 → `workspace.focus`→`tab.focus`→`pane.focus` の外→内チェーン。pane/コンテナ消失は `focused: false` でエラーにしない, F-94）/ `start_state_stream`（`events.subscribe`→**`pane.exited` デッドマン専用**に縮退。異常終了→`Failed`, F-38） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ送出（main が stdout へ、テストはバッファへ排出）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を単独所有し、応答と通知が行途中で交錯しないよう直列化。stdin ループが `SocketFactory`（実ソケット接続）を配線 |

# プロンプト投入（`submit_prompt`, #124）

**プロンプトを argv で渡す方式は使えない**（複数行だと CLI が投入しない = タスク本文は常に複数行なので必ずハングする）。`agent.send` でテキストを入力欄へ書き、`pane.send_keys ["enter"]` で送信する。ただし CLI は「入力を受け取れる状態」と「入力に反応できる状態」がずれており、早すぎる送信はテキストが失われ、早すぎる Enter は飲み込まれるため、**どちらも撃ちっぱなしにせず確認する**:

1. 着弾を画面で確認（プロンプト**末尾**の空白除去マッチ — 入力欄はカーソル側を表示し、CJK が語中で折り返されるため）し、未着弾なら再送
2. `agent_status ∈ {working, blocked, done}` になるまで Enter を再押下（空入力への Enter は no-op なので冪等）

どちらも確定できなければ**エラーで dispatch を失敗させる**（無言で永久ハングするセッションを作らない）。

# dispatch のフック起動（0.1.3, #131）

`TaskDispatchParams.hook`（`HookLaunchSpec = { settings_path, env }`）が Some のとき、dispatch は完了判定フックを載せた Claude Code を起動する:

- `workspace.create` / `agent.start` の params に `env` を付与（herdr 0.7.1+ は両メソッドとも `env?` 対応。フック環境変数 `TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` 等をプロセスへ注入）
- argv に `--settings <settings_path>` を付与（workflow ごとの orchestrator-*.json を読ませ、Stop/SessionEnd フックを有効化）
- `resume_session_id` が Some なら argv に `--resume <id>` も付与（Slack スレッド会話継続。`--resume` はフックを引き継がないため `--settings` は resume でも必須, H-03）

env 注入・フックの内容は**プラグインにとって不透明**（Orchestrator 側が生成・解釈する。プラグインは値を配線するだけ）。`hook` が None（旧 Orchestrator）でも dispatch は動くが、その場合 env・`--settings` は付かず**完了検知が働かない**（後述）。

# pane レイアウト（`apply_layout`, #356, 0.2.x）

`[layout]`（`shell` / `direction` / `ratio`）で dispatch 後の pane 配置を決める。決定の経緯・不採用案・実測値は [ADR-0030](/decisions/adr-0030-herdr-pane-layout.md)。

**なぜ `agent.start` に任せられないか**: `agent.start` は `split` 未指定でも分割し（既定 `right` / 0.5）、その `split` は**方向しか取らず `ratio` を取らない**。比率を指定できる唯一の経路は `pane.split` なので、レイアウトは起動の後から被せるしかない。

`agent.start` の直後・`submit_prompt` の**前**に、以下を実行する:

1. `pane.close <workspace.create 応答の root_pane.pane_id>` — herdr が workspace とともに開く初期シェル pane を落とす。この pane 以外に掴む手段は無い（`pane.list` では split pane の `label` がエージェント側もシェル側も `null` で区別できない）
2. `shell = true` のとき `pane.split {target_pane_id: <agent pane>, direction, ratio, cwd: <worktree>, focus: false}` — **`env` は渡さない**

順序・タイミングの理由:

- **close → split**: split が失敗した中間状態が「エージェント全画面」＝ `shell = false` と同じ正当なレイアウトに落ちる。逆順は 3 枚 pane の中途半端な状態を残しうる
- **`submit_prompt` の前**: split はエージェント pane をリサイズし、`submit_prompt` はその pane の画面でプロンプト着弾を照合する（`prompt_marker`）。投入中に折り返しが変わると、起動レースを守っている照合そのものが無効化される
- **`ratio` はそのまま渡る**: herdr の `ratio` は**分割元**（上 / 左）の取り分で、分割元はエージェント pane。設定した「エージェント側の取り分」が変換なしで一致する
- **追加の `pane.focus` は呼ばない**: close で唯一残った pane（エージェント）へフォーカスが移り、`pane.split` はフォーカスを分割元に残す

**失敗は全て `tracing::warn!` で握り潰し dispatch は成功させる**（レイアウトは装飾）。ただし **`pane.close` が失敗したとき・応答に `root_pane` が無いときは split も省略する** — 初期シェルが残った上に足すと 3 枚になるため、herdr の既定配置のまま放置する。

**セキュリティ上の副次効果**: 初期シェル pane は `workspace.create` の `env` を継承するため `TOTSUKA_HOOK_TOKEN` が載っていた。それを close し、`env` を継承しない `pane.split` でシェルを作り直すので、**人間が直接叩くシェルからトークンが消える**（[hook-security](/security/hook-security.md)）。エージェント pane には従来どおり載る。

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

- 状態写像・復帰ハンドル・`squash_ws`・プロンプト末尾マーカー・exit 分類は純関数として単体テスト。
- **実 Unix ソケットの fake herdr サーバ**に対する結合テスト（`tests/integration.rs`）。fake は実機を模す: **応答後に接続を閉じる**接続モデル、`{event, data}` 封筒（**ドット/アンダースコア混在**の実イベント名）、そして**入力に反応できるまで `agent.send` / Enter を落とす CLI**（= 実機の起動レース）。dispatch がその race を自己修正して完走すること・始動しない CLI では**エラーで失敗する**こと・**フック env が `workspace.create`/`agent.start` に乗り `--settings`/`--resume` が argv に入ること**・**`pane.agent_status_changed` を送っても通知が出ないこと（縮退の固定化）**・`pane.exited` 非 0/コード無し→`Failed`・clean exit（0）は通知なし・`diagnostics/snapshot` の正常/pane 消失（`text: null`）両応答・**`session/focus` のフォーカスチェーン（`pane.get` 先行 + workspace→tab→pane の順序と params）と pane 消失（`focused:false`・フォーカス呼び出しゼロ）**・**`session/release` の各分岐（ctrl+c を送らず close 対を送る正常系・`expect_cwd`/`expect_label` 不一致で両 close をスキップ・`cwd` 欠落時の degrade-open・pane 消失で `released:false` かつ close ゼロ）**・他 pane の replay と close 通知を無視すること・`id:""` エラーの即時相関・session/attach の成功と pane 消失（`pane_not_found`→`attached:false`）・`config/validate` の疎通（ping）を検証。**#261 で `SESSION_UNRESUMABLE` 写像の 3 分岐**（resume 指定 + pane 消失 → `-32006`／resume なし + pane 消失 → 従来の `-32603`／resume 指定 + pane 生存の別エラー → `-32603`）を固定した — 後ろ 2 本は「新コードを返しすぎない」ための負の固定で、写像が広がると会話文脈を落とす。
- **`[layout]`（#356）の固定**: 既定（`pane.close` が **root pane を** 対象にすること・`pane.split` の `direction`/`ratio`/`cwd`/`focus`・`agent.start` に `split` を渡さないこと・`agent.start`→`pane.close`→`pane.split` の順序）／`shell = false` で split ゼロ／`direction = "right"` と任意 `ratio` がそのまま届くこと／**split 失敗でも dispatch は成功しプロンプトも投入されること**／**close 失敗時は split を出さないこと**／`root_pane` 無しの応答で両方スキップすること／**`pane.split` に `env` が付かないこと**（フック env はエージェント側にだけ乗る）。既存の cancel / release テストの `pane.close` アサーションは **pane_id で絞る**形へ直した — dispatch 自身が close を呼ぶようになったため、メソッド名だけの検査では layout 由来の close で通ってしまい cancel を検証しなくなる。
- **実機手動チェック**（受け入れ #2）: 実 herdr + 実 Claude Code で `--settings` 付き pane 起動 → フック発火 → env（`TOTSUKA_JOB_ID`）がフックスクリプトから見えること（#123 検収環境）は issue #139 のコメントにチェックリストとして整理。

# 依存

- `plugin-protocol`（プラグイン境界）、`tokio`（`net`/`io-std` 追加）、`serde` / `serde_json` / `semver` / `thiserror` / `tracing`。新規外部クレートなし。

# 関連

- [plugin-protocol](/components/plugin-protocol.md)
- [herdr Socket API / 統合エージェント capability（外部一次情報ミラー）](/references/herdr-socket-api.md)
- [Spec §4.3 Agent IDE 連携 / F-30〜F-38・§4.11 F-100〜F-107](/product/orchestrator-spec.ja.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md) / [フックシグナルフロー](/architecture/hook-signal-flow.md)
- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
