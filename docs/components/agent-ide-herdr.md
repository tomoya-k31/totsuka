---
type: Component
title: agent-ide-herdr プラグイン
description: herdr を Agent IDE として接続する公式 agent_ide プラグイン（v1 参照実装）。Orchestrator の JSON-RPC ↔ herdr Socket API（NDJSON）のアダプタで、dispatch/セッション管理/状態ストリーム/plan モード/pane レイアウトを担う。
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/agent-ide-herdr
tags: [rust, crate, plugin, agent-ide, herdr, socket-api, streaming, hook, deadman, layout]
generated: { by: claude-code/opus-5, at: 2026-08-11T23:10:00+09:00 }
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
| `config` | `plugins/herdr.toml`（= `InitializeParams.config`）を型付け。`socket_path` / `session`（解決順: `socket_path` > `session` 名 > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > 既定 `~/.config/herdr/herdr.sock`、named session は `sessions/<name>/herdr.sock`）— **`agent_command` / `plan_args` / `launch_command` はプロトコル 0.4.0 で削除**（#411、[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)。0.2.3 以降は Orchestrator が `tool_launch` で argv を完全解決しており、manifest 下限を `>=0.2.3` に上げたことでフォールバックは到達不能になった。`deny_unknown_fields` なので**残っていると `initialize` が `CONFIG_INVALID`** になる — キー名と代替を挙げる `removed_keys_in` のメッセージが出る）/ **`[kind_map]`（実行ファイル名 → herdr の `kind`、[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-1）/ `design_preview`（**0.4.0 で削除**。core もプラグインも読んでおらず設定しても描画は変わらなかった。pane 配置は `[layout]` が決める。[ADR-0030](/decisions/adr-0030-herdr-pane-layout.md) は「削除は 0.3」と書いたが実際に消えたのは 0.4.0）/ **`[layout]`**（`LayoutConfig { shell, direction, ratio }`。`direction` は herdr の `SplitDirection` を写した enum で `down` / `right` の 2 値のみ、下記）/ `request_timeout_secs`。**`deny_unknown_fields` はネストした `[layout]` にも効く** |
| `state` | herdr `agent_status`→totsuka 正規化状態の写像（`working→running`・`blocked→waiting_input`・`done→done`・`unknown→前値維持`, F-32。**`session/attach` 専用**の写像で、タスク完了はもはやここを通らない — 完了検知はフックが担う）、`(pane_id, agent_session_id)` 復帰ハンドルの `session_id` 文字列へのエンコード（F-37）。**`squash_ws`（折り返し非依存の画面照合ヘルパ）は `agent.prompt` への移行で消滅**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-5）。**質問/回答の画面抽出（旧 `extract_question` / `extract_answer`）は完了判定のフック移行に伴い削除**（#131） |
| `transport` | `HerdrTransport` trait（`call` / `subscribe_events` / `events`）＋ `SocketTransport`。herdr の接続モデルに合わせ **リクエストごとに新規接続**（`call`）+ `events.subscribe` 専用の持続接続（reader タスクが `{event, data}` 封筒を broadcast へ転送）。broadcast はプロセス内の全購読で共有されるため、EOF 時の合成 close イベントは**購読対象 pane ごとに `data.pane_id` 付きで**発行し、他タスクを巻き込まない。`invalid_request` の `id:""` エラーも接続単位で相関。ロジックを fake herdr でテストするための seam |
| `agent` | `HerdrAgent<T: HerdrTransport>`。`dispatch`（`workspace.create`（`env` 付き）→**`apply_layout` = `pane.split`（#356）**→`agent.start {name, kind, pane_id: root_pane, args}`→`agent.prompt`→ハンドル返却, F-31/F-37。呼び出し列と各段の理由は下記）。**プロンプトは `compose_prompt` = `extra_context`（前文）＋ body（無ければ title）**: source が切り詰めた snippet title は body があるとき打たない（ペイン先頭の切れた重複行をなくす）。string の `extra_context`（フック非対応 dispatch 用の可視フォールバック。フック付き dispatch では通常 `None` — 指示文は env `TOTSUKA_PROMPT_CONTEXT` 経由の不可視注入で届く）は JSON 引用符なしの生テキストとして `---` 区切りの**前文**に置く。**protocol 17 で `RetryPolicy` と `submit_prompt` の自己修正手順（#124 / #281）は削除**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-5。`Server::with_retry_policy` / `HerdrAgent::with_retry_policy` も無くなった）。**0.2.3（#196, [ADR-0014](/decisions/adr-0014-tool-abstraction.md)）以降は `tool_launch: ToolLaunchSpec` が唯一の起動元**: `args`/`env` をそのまま使い、`program` は**ファイル名を `kind` へ写像**して渡す（`resolve_kind`。protocol 17 が実行ファイルを `kind` から決めるため。CLI フラグ知識は Orchestrator 側）。env は `workspace.create` へ注入され、`--settings` / `--resume` は既に `args` に焼かれている。**0.4.0（#411）で `tool_launch` 不在は `INVALID_PARAMS` エラー**（旧 `launch_command` フォールバックは削除。argv を自作すると `--settings` 無しで起動して完了報告が来ないペインになるため、黙って代替しない））/ `attach`（`pane.get` で pane 生存確認・消失（`pane_not_found`）は `attached:false`, F-37）/ `cancel`（`pane.send_keys ["ctrl+c"]`→`pane.close`→**タスクの workspace も close**（pane id `w1:p2` の接頭辞が workspace id。dispatch が workspace を作る以上、pane だけ閉じると空の workspace が残る）, 冪等）/ `release`（**0.2.1: `session/release`**。完了済みセッションの pane + workspace を ctrl+c なしで閉じる。同一性検証つき — 下記 #210 節, [ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）/ `snapshot`（**0.1.3: `diagnostics/snapshot`**。`pane.read`（`recent`）で画面テキストを返す。pane 消失は `text: None` でエラーにしない, R-10）/ `focus`（**0.1.4: `session/focus`**。`pane.get` 生存確認 → `workspace.focus`→`tab.focus`→`pane.focus` の外→内チェーン。pane/コンテナ消失は `focused: false` でエラーにしない, F-94）/ `start_state_stream`（`events.subscribe`→**`pane.exited` デッドマン専用**に縮退。異常終了→`Failed`, F-38） |
| `server` | JSON-RPC ディスパッチ `Server<F: TransportFactory>`。**`initialize` / `config·validate` が `ping` の `protocol` を検査し、17 未満はバージョンを名指しして拒否**（[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-6。`protocol` フィールドが無い応答は通す）。応答と push 通知（`state/notification`）を mpsc ラインチャネルへ送出（main が stdout へ、テストはバッファへ排出）。未初期化メソッドは拒否 |
| `main` | `#[tokio::main]`。専用 writer タスクが stdout を単独所有し、応答と通知が行途中で交錯しないよう直列化。stdin ループが `SocketFactory`（実ソケット接続）を配線 |

# dispatch の呼び出し列（protocol 17, [ADR-0032](/decisions/adr-0032-herdr-protocol-17.md)）

```text
workspace.create {cwd: worktree, env: hook_env}   → root_pane
workspace.report_metadata {workspace_id, source: "totsuka", tokens}   ┐ #417
pane.report_metadata {pane_id: root_pane, source: "totsuka", tokens}  ┘ 失敗しても続行
pane.split {target_pane_id: root_pane, ...}       → 併設シェル（[layout].shell = true のとき）
agent.start {name, kind, pane_id: root_pane, args, timeout_ms}
agent.prompt {target: root_pane, text, wait}
```

**identity の報告は `agent.start` の前**（#417、[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D1）。
`agent.start` は最大 180 秒のリトライループなので、後に回すと**運用者が一番サイドバーを見ている時間帯だけ
行が無名になる**。ソケット 1 往復 ≒ 25ms なので `agent.start` に比べれば誤差。workspace と root pane の
**両方**に送るのは、`$name` の解決先が spaces 行では workspace、agents 行では pane だから。
`source` は定数 `"totsuka"` — 異なる `source` は 1 コンテナにつき生涯 32 個しか受け付けず、
clear でも expiry でもスロットが戻らないため、タスク毎の `source` は使えない。
`[identity] enabled = false` で報告ごと止まる。

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

**`agent.start` はシェル未準備の間リトライする**（予算 180 秒・間隔 500ms・1 回あたり `timeout_ms` 15 秒、[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-7）。
`workspace.create` が返した root pane はシェルがまだ起動中で、herdr はそこへ起動コマンドを
打ち込んでしまう。プロンプトへ達するまでの時間は運用者の rc ファイル次第で予測できず、読める
readiness シグナルも無い（`pane.process_info` は「シェルが起動したか」しか答えず、実測で
`workspace.create` の +0.01 秒から埋まっている。知りたい「入力を受け付けられるか」との隙間が
このレース本体）。`agent.start` 自体が herdr の readiness 検査なので、その判断を再実装せず
同じ問いを繰り返す。

**打鍵は消える。待っても回復しない。** 入力を読んでいないシェルへ送られた起動コマンドは
キューされない。実測（#387）では `timeout_ms: 120000` で 120 秒フル待っても失敗し、その間
pane はずっと空だった。**同じ pane への `agent.start` 再送は 3 秒で成功する**ので、長く待つのでは
なく短い試行を重ねる。

**同じレースが 4 つの姿を取る**（実機 E2E で 15 回中 6 回＝40% が失敗、#387）:
`agent.start` の `agent_pane_busy` と `timeout`、そして **`agent.start` が成功を返しつつ
エージェントが検出されず `agent.prompt` が拒否する**形が 2 つ（`agent_not_ready` と
`agent_not_found`）。3 つ目が実機で支配的で、これは「起動が遅い」のではなく
**CLI がそもそも起動していない**。したがって `agent_not_ready` に付き合う窓は 15 秒に区切り、
超えたら `agent.prompt` を撃ち続けず **`agent.start` の再送に戻る**。検出に失敗した start は
herdr にエージェントを登録しないので、**同名での再送は安全**である。

**`agent_not_found` は初回 dispatch のときだけ再送する**（#391）。resume 付き dispatch では
pane がセッションごと死んだ形なので `SESSION_UNRESUMABLE` として上げねばならない（#261）が、
**初回 dispatch には死ぬべきセッションが存在しない** — そこで届く `agent_not_found` は
「`agent.start` が何も登録しなかった」以外に読みようがなく、同じレースである。
実機 2026-08-07 に連続 2 回踏み、いずれも単純な retry で解消した。

**再送は回数で上限を切る**（`MAX_AGENT_RESTARTS = 3`）。`agent_not_ready` は
15 秒窓を挟むので時間で頭打ちになるが、`agent_not_found` は即座に返るため、時間だけで
縛ると 180 秒の予算いっぱい CLI を起動し直し続けてしまう。

**リトライするのはこれらだけ**で、未知の `kind`・`agent_name_taken` は放っておいても直らない
ため即座に失敗させる。`pane_not_found` も含めない — pane が無ければ起動先が無く、再送しても
同じ失敗を繰り返したうえで最初の情報量のあるエラーを潰すだけである。

**`agent_prompt_stalled` は本文を再送しない。代わりに Enter を送る**（#380 / #391、[ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) D-7）。
herdr の 5 秒下限（設定不可）に Claude Code が間に合わないと、**成功した投入が失敗として返る**。
実測（2026-08-08）では、stall したペインはプロンプトを**入力欄に抱えたまま `agent_status: idle`**
で、25 秒後も idle のままだった — **typed されているが submit されていない**ので `agent.wait` は
タイムアウトするしかない。同じペインに Enter を送ると約 10 秒で `done` に達した。
そこで `idle` のときだけ `agent.send_keys {keys: ["enter"]}` で既に入っているものを送信してから
`agent.wait` で確認する。**本文の再送は禁止**（入力欄の既存文字列に追記されてタスクが壊れる）。
`working` / `done` / `blocked` のペインには Enter を送らない。到達しなければ**元の stall を**報告する
（`agent.wait` が `agent_not_found` を返したときだけそちらを通す — `SESSION_UNRESUMABLE` を埋もれさせないため）。

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

# dispatch のフック起動（0.1.3, #131 → 0.4.0, #411）

フック環境と `--settings` は **`TaskDispatchParams.tool_launch`（`ToolLaunchSpec = { program, args, env }`）だけ**で届く。0.1.3〜0.3 系にあった専用の `hook` フィールドはプロトコル 0.4.0 で削除された（#411、[ADR-0034](/decisions/adr-0034-protocol-0-4-0-removals.md)）— `ToolLaunchSpec` は同じ情報を、プラグインが解釈しなくてよい形で運んでいるため。

- **`workspace.create` の params にだけ** `tool_launch.env` を付与（protocol 17 の `agent.start` は `env` を取らない。root pane が継承するので、フック環境変数 `TOTSUKA_JOB_ID` / `TOTSUKA_HOOK_ENDPOINT` / `TOTSUKA_HOOK_TOKEN` 等はエージェントに届く）。空の env はキーごと送らない
- `agent.start` の `args` は `tool_launch.args` そのまま。`--settings <path>`（workflow ごとの orchestrator-*.json、Stop/SessionEnd フックを有効化）も `--resume <id>`（Slack スレッド会話継続。`--resume` はフックを引き継がないため `--settings` は resume でも必須, H-03）も **Orchestrator 側で既に焼かれている**
- `resume_session_id` は依然として送られてくるが、**プラグインはこれを読んで argv を組み立てない**（フラグは既に `args` にある）

内容は**プラグインにとって不透明**（Orchestrator 側が生成・解釈する。プラグインは値を配線するだけ）。`tool_launch` が無い dispatch は `INVALID_PARAMS` で**失敗させる**: 代替の argv を組めば `--settings` 無しで起動し、走るが完了を報告しないペインになり、タイムアウトのエスカレーションまで気づけないため。

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

**`session/list` への影響**: 所有判定が workspace 単位になった（#416）ので、split で作るシェル pane も**当たる**。`agent` を持つ pane を優先することで 1 タスク 1 件に保っている（下記）。`workspace.close` で巻き添えに閉じる点は変わらない。

# 状態ストリーム — デッドマン縮退（F-38, #131）

**完了検知はフック機構へ全面移行した**（R-07）。Claude Code の Stop/SessionEnd フックが UDS 経由で Orchestrator へ決定的に完了を通知するため、herdr の screen-manifest（画面パターン認識）由来の完了判定は**廃止**した。旧実装の「`working → idle` 確定」「2 秒デバウンス + `pane.get` 再確認」「`done` 導出」「scrollback からの質問抽出（旧 F-35）」「transcript / detection からの回答回収」は**すべて削除**。

`state/subscribe` は ACK 後に `events.subscribe` を **`pane.exited` のみ**（+ 購読断の合成 close イベント）へ縮退購読する。デッドマンとして働き、**異常終了→`Failed`** を通知して終端する:

- `pane.exited` の `exit_code` が非 0、または**コード無し**（herdr 0.7.x は exit_code を運ばないため clean と確認できない。対話モードの Claude は完了で終了しないので、説明のつかない exit は異常）→ `Failed`
- `exit_code == 0`（clean exit）→ **通知なし**でストリームを終える（正常終了はフック SessionEnd が既報）
- 購読接続の EOF（当該 pane 向け合成 close イベント）→ `Failed`（pane 自体は生きている可能性があるため復旧は `session/attach` 側に委ねる）

`data.pane_id` の自衛フィルタ・イベント区切り文字の正規化（`pane.agent_status_changed` はドット、`pane_exited` はアンダースコア）は継続。この縮退は**無条件**（`tool_launch.env` が空でも同じ）。かつては旧 Orchestrator との組合せを `initialize` の警告ログで知らせていたが、0.4.0 で manifest 下限が `>=0.2.3` になったため、そもそも起動時点で拒否される（F-54）— 到達しないコードだったので削除した。

# diagnostics/snapshot（R-10, 0.1.3）

`diagnostics/snapshot`（O→P、`diagnostics_snapshot` capability）はタイムアウト/エスカレーション診断のために pane 画面をキャプチャする。`pane.read`（`source = recent`）で画面テキストを返し、pane 消失（や読み取り失敗）は `text: None` で返す — **取得失敗はエラーにしない**ため、Orchestrator のエスカレーション経路がスナップショット不能で失敗することはない。

# session/focus（F-94, 0.1.4）

`session/focus`（O→P、`pane_control` capability。[ADR-0005](/decisions/adr-0005-click-to-focus.md)）は通知 click-to-focus のために対象セッションの pane を herdr 内で前面化する。`pane.get` で生存確認し、pane record の `workspace_id` / `tab_id`（workspace は pane id 接頭辞へフォールバック）を使って **`workspace.focus` → `tab.focus` → `pane.focus` を外→内の順**に呼ぶ（3 メソッドとも herdr 0.7.4 の `herdr api schema --json` で実在確認済み。params は各 id のみ）。pane・コンテナの消失（`*_not_found`）はどの段でも `focused: false` で返し**エラーにしない**（タスク終了後の通知クリックは正常系）。GUI ターミナル自体の前面化は notifier（terminal-notifier `-activate`）の責務で、このプラグインは herdr 内フォーカスのみを担う。

# session/release（#210, 0.2.1）

`session/release`（O→P、`pane_control` capability。[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）は worktree 掃除が「削除する」と判定した**完了済み**セッションの pane + workspace を閉じる。`cancel` との差は 2 点:

1. **ctrl+c を送らない** — 完了済みで中断すべきものが無い。close の対（`pane.close` → workspace 接頭辞の `workspace.close`）は `cancel` と共通の `close_pane_and_workspace` に抽出。
2. **同一性を検証してから閉じる** — `cancel` の盲目クローズは dispatch 直後に走るが、release は保持ポリシー次第で完了から日単位で後に走り、位置ベースの pane id（`w34:p2`）が別の pane に再発番されている窓がある。`pane.get` で live pane を取得し、`expect_cwd`（= worktree パス）を `PaneInfo.cwd` と突き合わせる。identity については **token があれば token だけを見る**（#417）: `expect_label` から `totsuka ` を剥がした task id と `tokens.totsuka_task` を比較し、**label は一切参照しない**。これは好みではなく必要で、[ADR-0039](/decisions/adr-0039-herdr-sidebar-identity.md) D4 の rename 後は workspace label が `{repo}: {title}` になり、同じタスクを指しているのに `expect_label` と一致しなくなるためである（比較すると rename 済み workspace の release が全部拒否される）。token が無い場合だけ、`expect_label` を **`WorkspaceInfo.label`**（`workspace.list` から `PaneInfo.workspace_id` で引く）および `PaneInfo.label` と突き合わせる — rename は**両方の報告が成功したときだけ**行うので、token が無い container は rename もされておらず `totsuka {task}` の label を持っている。**この workspace label の比較が #416 で入るまで、label 側は比較可能になったことが一度も無かった**（totsuka は pane に label を書かないため常に degrade-open へ落ちていた）。workspace label の取得は所有フィルタを**かけない** — `totsuka ` 前置で絞ると「他人の workspace である」が「判定不能」に化け、degrade-open で他人の pane を閉じてしまう。**比較可能なペアが1つでも不一致 → 閉じずに `released: false` + warn。全ペア比較不能（nullable フィールドが全部欠落）→ 閉じる（degrade-open）**。pane が既に無い（cancel 済みタスク等）は `released: false` で、**workspace も閉じない**（同一性未検証のまま閉じない）。

# session/list（#211, 0.2.2）

`session/list`（O→P、`pane_control` capability。[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）は `doctor` の孤児 pane 検出のための**自分の所有物の列挙**: `pane.list` と `workspace.list` を呼び、`PaneInfo.workspace_id` で結合して、**その workspace の `label` が `totsuka ` で始まる pane**を返す。この label フィルタが所有権境界 — herdr はユーザーが手で開いた pane も持ち、それを列挙・解放候補にしてはならない。label は `dispatch` が `workspace.create` に設定する `totsuka {task.id}`（`task.id` はプロトコル `Task.id` = **source task id**。Slack のスレッドキー等の文字列で、orchestrator DB の行 id ではない）で、doctor はこの source task id を `source_task_id` と文字列照合して DB と突き合わせる。返す `session_id` は pane_id + **空 agent_session** の縮退形（`pane.list` は中の Claude セッションを知らないが、`SessionHandle::decode` は bare 形式を受け付け `session/release` は pane さえ分かれば良い）。`SessionInfo.label` にはその **workspace の label** を入れるので、doctor 側の `strip_prefix` → `source_task_id` 照合は無改修で通る。

**token を先に見る（#417）。** pane が自分のものである条件は 4 つのいずれかで、直接的な順に:
`PaneInfo.tokens.totsuka_task` がある → その workspace の同トークンがある → その workspace の `label` が
`totsuka ` で始まる → pane 自身の `label` が始まる。token は `report_identity` が dispatch 時に付けた
機械識別子で、label が人間可読になっても所有の根拠が消えないようにするためのもの。
`SessionInfo.label` は token があれば `totsuka {task}` を**合成**して返すので、`doctor` の
`strip_prefix` → `source_task_id` 照合は無改修で通る。label 経路は「報告が失敗した dispatch」と
「#417 以前が残した pane」の回収として残す。

**pane の label は見ない（#416）。** 0.2.2 から 2026-08-11 まで、この列挙は `PaneInfo.label` を見ており、**実機 herdr に対して常に空配列を返していた** — herdr は workspace の label と pane の label を別フィールドとして持ち、前者は `workspace.create { label }` / `workspace.rename`、後者は **`pane.rename` だけ**が書く。totsuka は `pane.rename` を呼ばないので、pane の label は一度も設定されていなかった。結果として ADR-0013 の孤児 pane 検出は実機で一度も発火していない。`pane.rename` を呼ぶ案は、`show_agent_labels_on_pane_borders = true` の環境で不透明な ID が pane 枠に出るため採らなかった。pane 自身の label 判定は無害なので残してある（将来 herdr が label を伝播しても正しい）。

**1 workspace = 1 セッション。** totsuka の workspace には agent pane と伴走シェルの 2 枚があり、workspace 単位の判定では両方が当たる。**`agent_status` が `unknown` 以外か `agent_session` を報告している** pane を優先し、1 枚も無いとき（= エージェントが終了済み。まさに孤児のケース）だけ先頭の pane を代表にする。判定材料をこの 2 つに限るのは、実機プローブが `pane.list` のレコードに載ると示しているのがこれらだけだからである（`agent.start` の**レスポンス**には `agent` オブジェクトが載るが、pane レコードには載らない — 混同すると実機で常に false になり、この dedup が「herdr が最初に返した pane」＝伴走シェルへ黙って退化する）。これをしないと doctor が 1 タスクにつき 2 回聞き、2 回目の release が `released: false` を返す。

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
