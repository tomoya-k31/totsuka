---
type: Library
title: plugin-sdk クレート
description: task_source / agent_ide プラグイン作成用のヘルパークレート。単一 writer タスクの stdio ランタイム・JSON-RPC dispatch ボイラープレート（TaskSourceHandler / AgentIdeHandler）・{placeholder} 置換（template）とエージェント向けプロンプト組み立て（compose_prompt）・task/submit クライアント（バックオフ再送）・ポーリング型ソース向け poll_loop・trigger キーの未知検査・trigger.assignee 条件の解釈・チャンネル監視トリガ（trigger.channel）の解釈とバックフィル窓の定義を提供する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-sdk
tags: [rust, crate, plugin, sdk, task-source, agent-ide, push]
generated: { by: claude-code/opus-5, at: 2026-09-26T14:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

サードパーティが task_source / agent_ide プラグインを実装する際の共通機構（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）。**公式の task_source 4 本（github / notion / slack / discord）と agent_ide 2 本（herdr / orca）はすべてこのハンドラの上で動いている**（#759）—— 外部に薦める道を自分たちも通ることで、抽象が実際の要件で検証される。作者はソース固有ロジック（イベント受信 / API フェッチ / Task 変換）や IDE 固有ロジック（ペイン操作・状態の写像）だけを書けばよい。**範囲外**: HTTP クライアント・LLM ヘルパー・config スキーマ（ソース固有のまま）。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `runtime` | stdio NDJSON ランタイム。**`init_tracing()` が全プラグイン共通のログ初期化**（#639）—— stderr 出力。**stderr がパイプ（本体の下）なら全レベルの JSON Lines** を書き、本体が元のレベル・target・フィールドで出し直して `[log] level` で判定する（`RUST_LOG` は読まない。[ADR-0096](/decisions/adr-0096-plugin-log-relay.md)）。**端末なら人間向けの表示**で `RUST_LOG` 準拠（未設定なら `info`）。以下は #639 の経緯:各プラグインが手書きしていた `tracing_subscriber::fmt().with_writer(stderr).init()` は 2 点で黙って壊れていた: `RUST_LOG` を読むのは*自由関数*の `fmt::init()` だけで**ビルダーの `.init()` は INFO 固定**（`debug!` が全プラグインで到達不能だった）、かつ ANSI が常時 on なので**ホストがパイプ経由で拾って JSON ログにエスケープ列（`\u001b[2m` 等）を埋め込んでいた**。どちらもエラーも警告も出ないので、ログを見て調べようとした人が静かに空振りする。**単一 writer タスク（mpsc）が stdout を専有**し、返信行とバックグラウンドの `task/submit` リクエスト行が部分行で交錯しないことを構造的に保証（従来の read ループ内 inline 書き込みの恒久修正）。`serve()` は response 行（`id` + result/error、`method` なし）を `SubmitClient` へ、それ以外を `LineHandler` へ配路。`Writer::from_channel` でテスト/カスタムトランスポートにも載る |
| `dispatch` | `Reply` / `request_id` / `parse_params` / **`not_initialized()`**（`initialize` 前の呼び出しへの `INVALID_REQUEST`。全 kind 共通の文言）と、型付き **`TaskSourceHandler`** trait（initialize / config_validate / update_status / result_publish、`task_claim` は既定で `METHOD_NOT_FOUND`）。**`handle_line(&mut handler, line)`** が wire protocol 全体（PARSE_ERROR・notification 無応答・shutdown・METHOD_NOT_FOUND・params 不正は handler に届く前に `INVALID_PARAMS`）を実装し、`TaskSourceServer` はそれを `LineHandler` に包むだけ。server 自身が handler を兼ねるプラグインは `LineHandler` を `handle_line(self, line)` で実装すればよい（#759）。**`initialized()`**（既定 `true`）を上書きすると、`initialize` 前に params が解析できない request へ `INVALID_PARAMS` でなく `not_initialized()` を返す（`initialize` / `config/validate` / 未知メソッドは対象外）—— 手書き server は params より先に session を見ていたので、その順序の応答コードを保つため。params が解析できれば handler に届き、未初期化の拒否は handler 自身が行う。agent_ide 側も同じ。**0.2.0（#190）**: `tasks_fetch` は trait・dispatch とも削除済み — 全 task_source は push（`task/submit`）専用 |
| `agent_ide` | **`AgentIdeHandler`** trait と **`AgentIdeServer`**（#759）。必須はホストが無条件に呼ぶ initialize / config_validate / task_dispatch / session_attach / task_cancel / state_subscribe。能力で守られた `session/focus`・`session/release`・`session/list`（`pane_control`）と `diagnostics/snapshot`（`diagnostics_snapshot`）は既定で `METHOD_NOT_FOUND`（`task_claim` と同じ規則: 上書きするならフラグを宣言し、宣言するなら上書きする）。`session/list` は params を読まない。**`state_subscribe` は通知の受信チャネルを返すだけで、ACK → `state/notification` の順序（F-38）は `AgentIdeServer` が持つ**: ACK を `Reply` として返さず共有 writer へ自分で書いてから転送タスクを起こす。`Reply` で返すと `serve` が書くのは `handle_line` が戻った後なので、転送タスクが先に通知を書けてしまう |
| `template` | **`render(template, vars)`** / **`scan(template)`**: `{placeholder}` の**単一パス**置換（#759 で github / notion / slack の同一コピー 3 本から昇格）。単一パスは安全上の性質 —— 置換値やその隣はソースの外部入力（Issue 本文・Notion のページ名・Slack のメッセージ）で、2 パス目はそこに書かれた `{placeholder}` を指示に変える。未知キーと閉じない `{` はそのまま出す。`scan` は識別子の形の `{name}` だけを拾う（JSON 形の波括弧は内容） |
| `prompt` | **`compose_prompt(&TaskDispatchParams)`**（#759 で herdr / orca の同一コピーから昇格）: extra context を前置きに、本文（無ければタイトル）を末尾に置いた agent 向けプロンプト。文字列の extra context は JSON リテラルでなく生テキスト（#158） |
| `submit` | **`SubmitClient`**: `task/submit` を送り persist-before-ack の結果を待つ。ack 3 値（`accepted`/`duplicate`/`rejected`）は**最終**（再送しない）。JSON-RPC error（`NOT_ACCEPTING`/`SUBMIT_OVERLOADED`/`INTERNAL_ERROR`）・writer 喪失・ack timeout（30s）は指数バックオフ（1s→…→30s、最大 5 回）で再送 — submit は冪等なので再送は常に安全（ack 喪失後の再送は `duplicate` で吸収）。5 回で `GaveUp`（ソースシステムが durable origin なので恒久喪失なし）。clone 共有の pending map を `serve()` が解決 |
| `lookup` | **`LookupClient`**: `task/lookup` を送り「この会話は既知か / どのリポジトリか」を得る（0.2.4、#242）。**失敗はエラー条件ではない** — `submit` と違い最終的に通す必要がなく、タイムアウトやエラーは単に「答えが無い」なので、**リトライもバックオフもしない**（1 回・タイムアウト・`Lookup::Unknown`）。再試行しても呼び出し側が同じフォールバックを待たされるだけ。`Lookup::{Known{repo}, New, Unknown{reason}}` の 3 値で、`skips_resolution()` が true になるのは `Known` のみ — **未応答を「既知」と読むと会話がリポジトリ無しでディスパッチされる**ため、`Unknown` は必ず false。orchestrator はエンジンループで応答するので `git fetch` 等で数秒待たされうる（タイムアウト前提の設計） |
| `assignee` | **`AssigneeFilter`** / **`check(...)`**（#572）: `trigger.assignee` の条件（`@me` / `@none` / `@any` / login / 配列の OR）を解釈し、タスクの assignee 一覧と突き合わせる。**省略時の既定は `["@me", "@none"]`** で、これは #572 以前のプラグイン全体のゲート（F-08）と同一 —— つまりこれは旧ゲートの**置き換え**であって前段ではない。二重ゲートにすると `assignee = "teammate"` のような「書けるのに効かない」設定が作れてしまうため、経路を 1 本にしてある。特殊語に `@` を付けるのは衝突回避で、`me` / `none` / `any` はどれも実在しうるログイン名である。`check` は `initialize` 用で、**評価不能な条件を起動時に落とす**（`@me` なのに identity 設定が無い / people プロパティが未マップ）。**ただし `@any` は people プロパティを要求しない**（#582）—— `matches` が assignee 一覧を読む前に `true` を返すので、未マップでも評価できる。以前は本当にプロパティを要る条件と一緒に弾いていたため、**「assignee で絞り込まない」と明示する手段が無く**、キーを省略するのが唯一の静かな道になっていた（そしてその省略が #582 の穴そのものである）。判定は `reads_assignees()`ほか、`status` を伴わない `assignee` 単独トリガーに「1 タスク 1 回になる」warning を返す（**lane identity を刻むソースにだけ**。notion はどのトリガーでも刻まないので #573、`status` を足しても直らず、効かない対処を案内しないよう `status_mints_lane_identity = false` を渡す）。**共有しているのはキー名と値の語彙だけ**で、何と突き合わせるか（github は Issue 組み込みの assignee と `github_login`、notion は `property_map.assignee` と `notion_user_id`）は各プラグインが持つ。**`AssigneeFilter::parse_exclude`**（[ADR-0091](/decisions/adr-0091-trigger-exclude.md)）は `trigger.exclude.assignee` を同じ語彙で読むが既定を持たず、書かれていなければ `None`（誰も除外しない）。`check` はこちらにも同じ評価可能性の検査（people プロパティ・`@me` の identity）をかける —— 評価できない除外条件は「何も除外しない」まま黙るため |
| `trigger` | **`unknown_trigger_keys(workflows, valid)`**（#574）: そのソースが読まない `[[workflows]].trigger` キーを 1 件 1 メッセージで返す。呼び出し側は `initialize` でこれを `CONFIG_INVALID` へ倒す。**必要なのは、トリガーの解釈が `.get("…")` だから** —— 誰も読まないキーは黙って捨てられ、条件が 1 つ減る。つまりタイポは trigger を**狭めず広げる**（`assinee` と書くと「条件なし」になり、除外したかったタスクにこそ発火する）。`valid` は呼び出し側がリテラルで渡す（パーサの隣に `TRIGGER_KEYS` として置く規約） —— 導出しないので、キーを足してここを忘れると新しいキーのテストが落ちる。エラー文は有効キー一覧を含み、改名からの移行案内も兼ねる。`trigger = {}` はキーが無いのでこの検査は常に通る —— **「空 trigger に意味があるか」はこの関数の管轄ではなく、ソースが決める**（[ADR-0080](/decisions/adr-0080-slack-mention-trigger-marker.md)）。github / notion では従来どおり catch-all（#396）、slack は起動条件ゼロとして自前で弾く。**`unknown_exclude_keys(workflows, valid)`**（[ADR-0091](/decisions/adr-0091-trigger-exclude.md)）: 同じ検査を `trigger.exclude` の中身にかける。`valid` はそのソースが否定できるキーで、trigger の語彙と一致するとは限らない（notion の `filter` は否定できないので含めない）。`exclude` は含めないので入れ子も未知キーとして落ちる。検査はキーの綴りだけで、`exclude = {}` などの意味は問わない。**`one_or_many(value)`**: 文字列か文字列配列の trigger 値を OR の候補列として読む（`label` / `exclude.status` / `exclude.label`） |
| `watch` | **`WatchTrigger`** / **`resolve(...)`** / **`BackfillLimits`**（#616、[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md)）: チャンネル監視トリガ `trigger = { channel = "<id>", channel_name = "<名前>", repo = "<repo>", from = [...] }` の解釈。**id が正・`channel_name` は照合専用**（起動時に実名と突き合わせて改名を警告する契約。`name_mismatch()` が全ソース共通の文言を返す）。**起動ゲートは操作者本人 + `from` の完全一致 id** で、`allows()` だけが唯一の判定経路（`from` は非公開フィールド — 操作者を締め出せる形を作らない。空の author / operator でも開かない）。`resolve` は `initialize` 用で、repo の実在（`InitializeParams.repositories` と照合）・同一チャンネルの二重 watch・`reaction` との同居・`channel` 抜きの watch キー・operator identity 未設定を全部まとめて `CONFIG_INVALID` に倒す（**watch を試みた workflow があれば identity 検査も同じパスで出す** — 表を直した次の起動で初めて出る、を避けるため）。`BackfillLimits` は[起動時バックフィル](/glossary/startup-backfill.md)の窓（既定 100 件 / 24h、どちらも 0 は拒否、時間の乗算は saturating）と `cutoff()` を持つ。**バックフィルのループ自体はここに無い**: 履歴を読み・そのソースの判定表に通し・enrich し・**そのソース自身の submit 経路**（結果投稿先の座標を記録する）へ流す、のどれもソース固有で、共通ループが素の `Submitter` へ流すと backfill 由来のタスクだけ `result/publish` が失敗する。一般化できるのは窓とその規則なので、そこだけを提供する |
| `poll` | **`poll_loop`**: `InitializeParams.workflows`（0.6.0 / #554 で `triggers` から改称。`WorkflowInfo` は `workflow` 名も運ぶ）× 各ソースが自分の `[<name>].poll_interval_secs` から読む周期の fetch→submit タイマー（github/notion がプラグイン内部でこの周期を使う唯一の取り込み経路。旧 `tasks/fetch` RPC は 0.2.0 で削除済み）。tick は非重複、間隔は ±10% jitter（SplitMix64、rand 依存なし）。fetch 失敗はその tick のみスキップ。dedup は Orchestrator 側 `duplicate` ack に委譲し seen-set を持たない |

# 利用パターン

- **イベント駆動ソース（slack 型）**: `runtime::stdio()` → パイプラインに `SubmitClient` の clone を渡してイベント→`submit_task(task, workflow)`（**どの `[[workflows]]` に属するかはプラグインが決めて名前で渡す** — 0.6.0 / #554）、`serve(handler, &stdio)` で host リクエストに応答。 会話継続ソースは submit の前に `LookupClient` で既知判定し、既知なら新規会話向けの解決（LLM 呼び出し・リポジトリ選択 UI）を省く。`serve()` は全 response 行を `submit` / `lookup` 両クライアントへ渡し、各自が発行していない id を無視する（id 接頭辞 `submit-` / `lookup-` で分離）。
- **agent_ide**: `stdio()` → `serve(AgentIdeServer::new(handler, stdio.writer.clone()), &stdio)`。handler は IDE 固有の処理だけを持ち、`state_subscribe` では状態変化を流す `mpsc` の受信側を返す。
- **ポーリングソース（github/notion 型）**: `initialize` で受けた `workflows` と、自分の config の `poll_interval_secs`（0.6.0 / #554 で `[<name>]` のキーになり、`InitializeParams` からは消えた）を `poll_loop(workflows, interval, submit, fetch_fn)` に渡して spawn。`fetch_fn` は `WorkflowInfo` を受け取り、その `trigger` の解釈もワークフローの選択もプラグイン側で行う（core に予約語彙は無い、[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）。

# 公式プラグインを載せて合わなかった点（#759）

載せ替えで SDK 側を直したもの、および仕様として受け入れた差。**直すのはプラグインではなく SDK の側**という方針で進めた。

- **params の検査と未初期化の順序**: 手書き server は session を見てから params を読んでいたので、`initialize` 前に params の壊れた request は `INVALID_REQUEST` だった。SDK は params を先に読むので `INVALID_PARAMS` になり、orca の統合テストが落ちて発覚した。**`initialized()` を trait に足して SDK 側で埋めた**（params が解析できないときだけ参照する）。
- **`state/subscribe` の ACK を `Reply` で返せない**: `serve` は `handle_line` が戻ってから応答を書くので、先に転送タスクを起こすと通知が ACK を追い越しうる。`AgentIdeServer` は ACK を writer へ自分で書く（上の `agent_ide` 行）。
- **`session/list` は params を読まない**: 手書き server は params を無視していた。`SessionListParams` は空の struct で `null` を受けないため、型で読むと params 省略の request が壊れる。
- **受け入れた差（エラーコードは不変、文言だけ）**: `INVALID_PARAMS` の文言に SDK の接尾辞 `→ fix the request shape` が付く。discord の未知メソッドの文言が他と揃う。discord の `method` 欠落は `METHOD_NOT_FOUND` に揃う。`config/validate` は params を型で読むので、`config` の無い params（ホストは送らない）は `INVALID_PARAMS` になる。
- **SDK の `serve` は shutdown の ACK を書き切る前にプロセスが終わりうる**。ホストは ACK を読まずにプロセスの終了だけを待つので実害は無く、task_source 側では元からこの形だった。

# 依存

- `plugin-protocol` / `serde` / `serde_json` / `tokio`（io-std）/ `tracing`

# 関連

- [ADR-0008 task/submit による push 型タスク取り込み](/decisions/adr-0008-task-submit-push-ingestion.md)
- [plugin-protocol クレート](/components/plugin-protocol.md)
- [プラグイン開発ガイド](/development/plugin-dev-guide.md)
