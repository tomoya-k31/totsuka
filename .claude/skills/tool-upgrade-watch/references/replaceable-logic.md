# 置き換え候補の台帳

AI ツール（Claude Code / Codex / opencode）の機能不足を totsuka 側で肩代わりしている箇所の一覧。
各項目は「ツールに何が入れば消せるか」と「消えたら totsuka の何が縮むか」の組。

## 項目の読み方

- **id**: 起票時の重複判定キー（issue 本文の印 `<!-- tool-upgrade-watch: item=<id> tool=<tool> -->`）。**変えない**。
- **種別**:
  - `feature` — ツール側の新機能で置き換えられる。changelog / docs の監視対象。
  - `policy` — 機能はもうあるが、ADR で**意図して使っていない**。新機能が出ても自動では起票せず、
    「前提が変わったか」をレポートに 1 行書くだけにする（判断は人間がする）。
- **アンカー**: `git grep -n -F '<文字列>' -- <path>` で今も当たるはずの文字列。当たらなければ
  コード側が既に変わっている（＝台帳が古い）。行番号は書かない — すぐずれるので。
- **置き換え条件**: 「これが docs で確認できたら置き換え可能」と判定する基準。changelog の 1 行では足りず、
  docs で入力・出力・スコープまで確かめられることが条件。
- **検索語**: changelog / docs を grep する正規表現の種（大文字小文字は無視して使う）。

`core` = `crates/orchestrator-core/src`。

## 基準版（totsuka が実機で確かめた版）

changelog はこの版から最新までを読む。これより古い版で入った機能でも、台帳の前提が古いだけなので候補になる
（その場合「基準版の時点で既にあった」と issue に書く）。置き換えを実装したら、ここを上げる。

| ツール | 基準版 | 出所 |
|---|---|---|
| Claude Code | 2.1.277 | `ai-docs/references/orca-cli-control.md`（orca 1.4.205 と併せた実測、2026-09-19） |
| Codex | 0.145.0 | `core/tool/mod.rs` の Phase 2 spike、`ai-docs/operations/codex-tool-setup.md` |
| opencode | v2.0.18 | ADR-0105 / ADR-0107 |

---

## Codex

### codex-global-hooks

- 種別: feature / ツール: Codex / ADR-0014
- アンカー: `fn sync_registration` — `core/hooks/codex.rs`
- 現状: Claude の `--settings` に当たる起動単位の指定が無いので、ユーザー全体の `$CODEX_HOME/hooks.json` に
  totsuka のエントリを差し込む。個人セッションでも hook が発火するため、全スクリプトが `TOTSUKA_JOB_ID`
  未設定なら即 exit する。
- 置き換え条件: 起動単位で hooks を渡す手段が docs にある — hooks ファイルを指す CLI フラグ、
  `-c` による inline `[hooks]` の上書き、または起動時に読ませる設定層（profile / 環境変数で場所を変えられる
  config 層）。**ユーザーの `~/.codex/` を書き換えずに済むこと**が要件。repo 内 `.codex/hooks.json` は
  worktree ごとにファイルを置く必要があり、ユーザーの repo を汚すので単独では条件を満たさない。
- 検索語: `--hooks`, `-c .*hooks`, `inline \[hooks\]`, `hooks\.json`, `CODEX_HOME`, `config layer`, `profile`, `session.*hooks`, `dangerously-bypass-hook-trust`
- 読む docs: `docs-hooks.md` に加え `docs-config-basic.md` / `docs-config-advanced.md`（`-c` と設定層の優先順位はここにしか無い）
- 置き換え後: `codex.rs` のマージ・in-place 更新・`is_managed` の大半を削除し、`launch_spec` にパスを渡す。
  スクリプトの `TOTSUKA_JOB_ID` ゲートは残してよい（無害）。

### codex-trust-index

- 種別: feature / ツール: Codex
- アンカー: `untrusted_events` — `core/hooks/codex.rs`
- 現状: trust のキーが `path:event:idx:0` なので配列の index を動かさないよう in-place で置き換える。
  doctor が `config.toml` の `[hooks.state]` を読み、黙ってスキップされた hook を検出する。初回は TUI で
  "Trust all" を人間が押す必要がある。
- 置き換え条件: trust が配列の位置ではなく安定した id / 名前で引かれ、**かつ** totsuka のエントリだけを非対話で
  trust できる（ユーザー自身の hook の trust を巻き込まない）。満たすのが一部だけなら「部分的」:
  - 未 trust の hook が警告になるだけ → 検出（doctor）は縮むが、index 保存も初回の対話承認も残る
  - 起動単位で trust を飛ばすフラグ（`--dangerously-bypass-hook-trust` 等）→ 同じ pane のユーザーの未承認 hook まで
    動くので、採るかは判断が要る（隣のユーザー hook の trust を守るのが index 保存の目的、`codex.rs` の in-place 更新）
- 検索語: `trust`, `trusted_hash`, `hooks\.state`, `untrusted`, `dangerously-bypass-hook-trust`
- 置き換え後: 確認できた機能が解消する分だけを削る（警告なら doctor の検出、安定キーなら index 保存の規則、
  非対話 trust なら runbook の TUI 手順）。

### codex-no-notification

- 種別: feature / ツール: Codex
- アンカー: `PermissionRequest` — `core/hooks/on-notification.sh`
- 現状: Codex に Notification が無いので PermissionRequest を `on-notification.sh` に通し、
  `permission_prompt: <tool>` を合成する。stdout に何か出すと承認判断として扱われるので空に保つ。
- 置き換え条件: Codex hooks に Notification 相当（承認待ち / idle / 入力待ち）のイベント、
  または判断を返さない観測専用の PermissionRequest が docs にある。
- 検索語: `Notification`, `PermissionRequest`, `idle`, `observe`
- 置き換え後: `on-notification.sh` の Codex 分岐を消し、素の Notification を登録する。

### codex-sessionend-clamp

- 種別: feature / ツール: Codex
- アンカー: `SessionEnd hook timeouts at 3s` — `core/hooks/on-session-end.sh`
- 現状: Codex が SessionEnd の timeout を 3 秒に制限するので、単発の高速 POST に留めている。
- 置き換え条件: SessionEnd の timeout 上限が docs で引き上げ / 撤廃されている。
- 検索語: `SessionEnd`, `timeout`
- 置き換え後: timeout を他イベントと揃え、制約コメントを削る。

### codex-no-question-tool

- 種別: feature / ツール: Codex / ADR-0050（openai/codex #11536, #30150）
- アンカー: `interactive_question` — `core/tool/mod.rs`
- 現状: `request_user_input` が Plan Mode 限定なので `interactive_question=None`。番号付きリストで質問させ
  `NEEDS_INPUT` で止める。
- 置き換え条件: `request_user_input`（または同等の質問ツール）が Default mode で使え、かつ開いたことを
  hook（PreToolUse 等）で観測できる。
- 検索語: `request_user_input`, `request_user_input_async`, `Default mode`, `ask.*question`, `#11536`, `#30150`
- 注意: 非同期版（`request_user_input_async`、0.153.0）はターンを止めないので、今の `Stop{NEEDS_INPUT}` による停止をそのまま使える。モデルカタログ次第で有効になる点は「部分的」の理由として書く。
- 置き換え後: `interactive_question=Some("request_user_input")`、`MANAGED_HOOKS` に PreToolUse の中継を足し、
  番号付きリストのフォールバックを削る。

### codex-no-plan-mode

- 種別: policy / ツール: Codex / ADR-0014, ADR-0035
- アンカー: `--sandbox` — `core/tool/mod.rs`
- 現状: plan mode を `--sandbox read-only --ask-for-approval never` で近似。sandbox の方が強い境界なので
  意図して維持している。
- 置き換え条件: なし（plan 専用フラグが出ても sandbox は残す）。新フラグが出たらレポートに 1 行書くだけ。
- 検索語: `plan mode`, `--mode`, `collaboration mode`

---

## opencode

### oc-global-plugin

- 種別: feature / ツール: opencode
- アンカー: `fn sync_assets` — `core/hooks/opencode.rs`
- 現状: JS プラグインをユーザー全体の `~/.config/opencode/plugins/` に置き、`TOTSUKA_HOOK_ENDPOINT` /
  `TOTSUKA_JOB_ID` が無ければ何もしない。
- 置き換え条件: 起動単位でプラグイン / 設定ディレクトリを渡す手段（`--plugin <path>`、プロセス単位で効く
  `OPENCODE_CONFIG` / `OPENCODE_CONFIG_DIR` 等）が docs にある。
- 検索語: `--plugin`, `OPENCODE_CONFIG`, `OPENCODE_CONFIG_DIR`, `plugin.*path`
- 置き換え後: ユーザーの config dir への書き込みをやめ、`launch_spec` で渡す。`references_opencode` と
  asset 同期が縮む。

### oc-standalone

- 種別: feature / ツール: opencode / ADR-0105 D2
- アンカー: `--standalone` — `core/tool/mod.rs`
- 現状: `--standalone` を常に付ける。無いと共有バックグラウンドサーバが pane の env を見られず、完了が届かない。
  代償は pane ごとに opencode サーバが 1 つ立つこと。
- 置き換え条件: TUI クライアントからサーバ側プラグインへ、セッション単位の env / メタデータを渡す手段が docs にある。
- 検索語: `--standalone`, `session.*metadata`, `plugin.*env`, `client.*env`
- 置き換え後: `--standalone` の強制を外し、プラグインが `process.env` ではなくセッションメタデータを読む。

### oc-plan-agent

- 種別: feature / ツール: opencode / ADR-0014 Phase 3, ADR-0023
- アンカー: `totsuka-plan` — `core/hooks/opencode.rs`
- 現状: edit / bash / task を deny するカスタム agent `agents/totsuka-plan.md` を `--agent totsuka-plan` で起動。
  部分的な deny は subagent への委譲で漏れる。
- 置き換え条件: 構造的な読み取り専用フラグ、または subagent に継承される permission deny が docs にある。
- 検索語: `plan`, `read-only`, `permission.*inherit`, `subagent.*permission`, `--permission`
- 置き換え後: agent asset と frontmatter のピンを削り、フラグを使う。

### oc-sessionstart-synth

- 種別: feature / ツール: opencode / ADR-0105 D1
- アンカー: `session.execution.started` — `core/hooks/totsuka-opencode.js`
- 現状: v2 にはルートセッションの作成イベントが無いので、セッションごとに最初の `session.execution.started`
  を SessionStart の代わりにしている。
- 置き換え条件: v2 系の docs（`docs-v2-plugins.html`。repo の `2.0` branch の `.mdx` は v1 の API のまま更新されていないので使わない）に `session.created` 等の作成イベントがあり、
  **ルートセッションでも発火する**と読める。イベント名が載っているだけで発火範囲が書かれていなければ
  「部分的」とし、実機確認（live-e2e-orca 等）を issue の作業に含める。
- 検索語: `session\.created`, `SessionStart`, `session lifecycle`
- 置き換え後: `started` の Set を作成イベントに置き換える。

### oc-marker-reask

- 種別: feature / ツール: opencode / ADR-0107 D2
- アンカー: `reasking` — `core/hooks/totsuka-opencode.js`
- 現状: マーカーが無いとき `ctx.session.prompt` で 1 回だけ再質問する（`reasking` Set で追跡）。
  再質問はユーザー入力として画面に見える。
- 置き換え条件: ターン終了を拒否して理由付きで続行させるプラグイン hook（Claude の Stop `decision: block` 相当）が docs にある。
- 検索語: `block`, `continue`, `turn.*end`, `stop`, `decision`
- 置き換え後: 再質問と Set を block の返り値に置き換え、文言同期テストを削る。

### oc-subagent-filter

- 種別: feature / ツール: opencode / ADR-0107 D5
- アンカー: `session.get` — `core/hooks/totsuka-opencode.js`
- 現状: `context` hook の入力に `parentID` が無いので、セッションごとに `ctx.session.get` を 1 回呼んで
  subagent かを判定する。
- 置き換え条件: 全イベントと `context` hook 入力に `parentID`（または `isSubagent`）が載る。
- 検索語: `parentID`, `isSubagent`, `context`
- 置き換え後: `children` / `isChild` マップと `session.get` 呼び出しを削る。

---

## Codex と opencode の共通

### no-prompt-hook

- 種別: feature / ツール: Codex, opencode / ADR-0004, ADR-0014 D5, ADR-0020, ADR-0107
- アンカー: `fn verification_for` — `core/run/hooks.rs`、`prompt_verification` — `core/tool/mod.rs`
- 現状: LLM 評価型の Stop hook が無いので `prompt_verification=false`。`verification="llm"` は完了時に
  `human` へ格下げされ、validate が警告する。
- 置き換え条件: Codex なら `hooks.json` に prompt 型（LLM 評価）の hook、opencode なら組み込みの
  LLM 評価 hook が docs にある。opencode は `ctx.generate.text` で totsuka 自前実装もできる（ADR-0107 で見送り）
  ので、それはレポートで触れるだけにする。**ツールごとに別判定**し、起票もツールごとに分ける。
- 検索語: `type.*prompt`, `prompt hook`, `agent hook`, `LLM`, `generate\.text`
- 置き換え後: 該当ツールの capability を反転し、rubric hook を登録する。そのツールの格下げと警告を削る。

### no-heartbeat

- 種別: feature / ツール: Codex, opencode
- アンカー: `.turn_id` — `core/hooks/on-stop.sh`、`heartbeat` — `core/tool/mod.rs`
- 現状: Stop 入力に `background_tasks` が無いので `heartbeat=false`。Codex の `turn_id` を `prompt_id` に詰め替える。
  runbook は長い `timeout_secs` を勧めている。
- 置き換え条件: Stop / ターン終了の入力に、作業継続中を示すフィールド（`background_tasks` 相当）がある。
- 検索語: `background_tasks`, `background`, `turn_id`, `Stop.*input`
- 置き換え後: capability を反転。`on-stop.sh` の `.turn_id` フォールバックは Codex が名前を揃えたら削る。

---

## Claude Code

### claude-notif-no-id

- 種別: feature / ツール: Claude Code
- アンカー: `n-$(iso_now)` — `core/hooks/on-notification.sh`
- 現状: Notification 入力に id が無いので `n-<時刻>-<pid>` を合成する（以前は 2 件目以降が重複として捨てられた）。
- 置き換え条件: hooks docs の Notification 入力スキーマに通知単位の id（`notification_id` 等）がある。
- 検索語: `notification_id`, `Notification.*input`, `"id"`
- 置き換え後: ネイティブ id を使い、合成フォールバックを削る。

### approval-resolved

- 種別: feature / ツール: Claude Code（Codex にも登録している）/ ADR-0086
- アンカー: `awaiting approval` — `core/hooks/on-post-tool-use.sh`、`awaiting_approval` — `core/run/hooks.rs`
- 現状: 承認が済んだことを知る手段が無いので、PostToolUse を heartbeat として登録し `awaiting_approval` を外す。
  フラグはメモリだけなので再起動で消える。
- 置き換え条件: 承認 / 拒否が決まったときに発火するイベント（承認結果の hook、通知の解消イベント等）が docs にある。
  拒否側だけのイベント（auto mode の拒否のみ等）は「部分的」。
- 検索語: `PermissionResolved`, `PermissionDecision`, `PermissionDenied`, `approved`, `dismiss`
- 置き換え後: **イベントを確認できたツールの登録だけ**を外し、そのイベントでフラグを外す（Claude なら
  `render_settings` の PostToolUse）。Codex は `MANAGED_HOOKS` の PostToolUse で同じ解除をしているので、
  Codex に同等のイベントが無い間は `on-post-tool-use.sh` と Codex の登録を残す（消すと承認後の作業時間が
  沈黙監視から外れたままになる）。

### question-pending

- 種別: feature / ツール: Claude Code / ADR-0038 D6, ADR-0050
- アンカー: `QuestionPending` — `core/hooks/on-ask-user-question.sh`
- 現状: 質問ダイアログ中は Stop が来ないので、PreToolUse(`AskUserQuestion`) から `QuestionPending` を送って
  タスクを止める。`tool_use_id` が無ければ入力の cksum を id にする。人間が答えても何も発火しないので、
  次の Stop まで `waiting_input` のまま。
- 置き換え条件: 質問待ちを示す Notification matcher（または入力待ちイベント）が id 付きであり、加えて
  「回答された」ことを示すイベントがある。前者だけなら「部分的」。
- 検索語: `AskUserQuestion`, `question`, `agent_needs_input`, `elicitation_dialog`, `answered`, `tool_use_id`
- 置き換え後: PreToolUse の中継をネイティブイベントに替え、cksum を削る。回答イベントで `Running` へ戻す。

### claude-prompt-hook-isolated

- 種別: feature / ツール: Claude Code / ADR-0020 §1
- アンカー: `"type": "prompt"` — `core/hooks/mod.rs`
- 現状: prompt 型の rubric hook は毎回の Stop で走り、heartbeat や NEEDS_INPUT の Stop を自分の文面で除外する。
  その判定結果は `on-stop.sh` から読めないので、マーカーが完了シグナルとして残る。
- 置き換え条件: hook の判定結果を後続 hook / HTTP hook が受け取れる、または hook に発火条件
  （`if` 等で `background_tasks` が空のときだけ）を書ける。
- 検索語: `"if"`, `condition`, `hook.*result`, `chain`, `type.*agent`
- 置き換え後: rubric が単純になり、エンジンが判定を読める経路ができる（`marker-protocol` の前提）。

### claude-plan-mode-gate

- 種別: feature / ツール: Claude Code / ADR-0035, ADR-0036
- アンカー: `plan_mode_only_adds_the_gate` — `core/tool/mod.rs`
- 現状: 読み取り専用 profile では `--permission-mode plan` を付けない。plan mode は `cat >` の書き込みを止めず、
  `ExitPlanMode` が無人 pane を承認待ちで止める（14 分の停止を実測）。
- 置き換え条件: plan mode が Bash のファイル書き込みを塞ぎ、かつ人間の承認なしで終了できる（無人運用向けの
  オプション）ことが docs にある。
- 検索語: `permission-mode`, `plan mode`, `ExitPlanMode`, `headless`, `non-interactive`
- 置き換え後: `plan_mode_only_adds_the_gate` と settings path の条件を削り、常に plan mode を渡す。

### claude-bash-deny-bypass

- 種別: policy / ツール: Claude Code / ADR-0035, ADR-0036, ADR-0040 D5, ADR-0045
- アンカー: `fn deny_rules` — `core/hooks/permissions.rs`、`enforce_read_only` — `core/run/sweep.rs`
- 現状: `Bash(git ...)` の deny パターンは `&&` / パイプ / heredoc ですり抜けるので、事後検出（publish 検査と
  60 秒 sweep）で失敗させる。Claude 自身の sandbox は存在するが、運用判断で使っていない。
- 置き換え条件: 複合コマンドの各要素を個別に照合する Bash permission が docs に入ったら、
  前提が変わったとレポートに 1 行書く（sandbox 採用は方針判断なので起票しない）。
- 検索語: `compound`, `&&`, `subcommand`, `sandbox`, `denyWrite`

### claude-write-path-rule

- 種別: feature / ツール: Claude Code
- アンカー: `Write(` — `core/hooks/permissions.rs`
- 現状: `Write(path)` / `NotebookEdit(path)` のパス限定ルールは受理されて無視される（v2.1.210 から警告）ので、
  ツール名だけで丸ごと deny している。
- 置き換え条件: パス限定の `Write(...)` ルールが実際に効くと docs / changelog にある。docs が `Edit(path)` で
  Write も覆うと書いている場合は、`Edit(path)` で置き換えられるかを判定する（`Write(path)` そのものは来ないかもしれない）。
- 検索語: `Write\(`, `path-scoped`, `NotebookEdit`, `permission rule`
- 置き換え後: 読み取り専用 profile の契約（worktree とリポジトリへの書き込み禁止、`permissions.rs` の
  `deny_rules`）は**弱めない**。パス限定が効いて得られるのは、その禁止を保ったまま worktree 外（一時ファイル等）
  への書き込みだけを許す、という緩和の余地だけ。禁止範囲が狭まる置き換えは候補にしない。

### claude-resume-cwd

- 種別: feature / ツール: Claude Code / ADR-0015
- アンカー: `SESSION_UNRESUMABLE` — `core/run/dispatch.rs`
- 現状: セッションが cwd 単位で保存されるので、会話はタスクと worktree に縛られる。`--resume` 失敗を
  `SESSION_UNRESUMABLE` に写像し、1 回だけ新規でやり直す。
- 置き換え条件: 別 cwd からの id 指定 resume、または `--resume` 失敗に専用の終了コードがある。
- 検索語: `--resume`, `resume.*directory`, `exit code`
- 読む docs: `docs-sessions.md`
- 置き換え後: 任意。エラーの写像が単純になる。

### http-hook-transport

- 種別: feature / ツール: Claude Code（他 2 つも同様に見る）
- アンカー: `spool_line` — `core/hooks/hook-common.sh`
- 現状: command hook には配送保証が無いので、curl + spool + 起動時 replay を自前で持つ。
- 置き換え条件: ネイティブの HTTP hook（`type: "http"` 等）が**Unix ドメインソケットへ送れ**、ヘッダ（Bearer）を
  付けられ、失敗時に再送する。TCP の URL しか受けないなら、受信側を TCP に変える設計判断が要るので「部分的」とし、
  issue にはその判断が必要だと書く。
- 検索語: `type.*http`, `unix`, `socket`, `retry`, `headers`
- 置き換え後: curl を呼ぶスクリプトが縮む。spool は受信側が落ちている間の保険なので残る可能性が高い。

---

## 3 ツール共通（単独では置き換えられない）

### marker-protocol

- 種別: feature / ツール: Claude Code, Codex, opencode / ADR-0020, ADR-0004
- アンカー: `fn strip_status_markers` — `core/run/hooks.rs`
- 現状: 応答末尾の `<<STATUS:...>>` を grep し、publish 前に剥がす。無ければ 1 回 block し、UNKNOWN の連続数で
  エスカレーションする。
- 置き換え条件: ターン終了のイベント入力に、表示テキストとは別の構造化された完了ステータス（status / reason）が
  載る。**ADR-0020 により 3 ツールすべてで揃うまでマーカーは消せない。** 1 ツールだけ揃った場合は起票せず、
  レポートに「n/3 揃った」と書く。3 つ揃ったときだけ起票する。
- 検索語: `structured output`, `output schema`, `json-schema`, `completion status`, `result`
- 置き換え後: マーカー解析・strip・再質問 / block・UNKNOWN 連続数とその上限・`marker_self_report*` の大半を削る（大きな変更）。

### tui-prompt-delivery

- 種別: feature / ツール: Claude Code（herdr / orca 経由で全ツール）
- アンカー: `submit_if_left_unsent` — `plugins/agent-ide-herdr/src/agent.rs`、`wait_for_agent` — `plugins/agent-ide-orca/src/agent.rs`
- 現状: タスク本文を TUI に打ち込む。herdr は止まったプロンプトに Enter を押し直し、orca は `agentIdentity` を
  待ってから送る。複数行のプロンプトは argv に載せられない。
- 置き換え条件: 対話モードのまま初回プロンプトをファイル / stdin から受け取る起動フラグが docs にある。
  herdr / orca 側の制約も絡むので、満たしても「部分的」とし、issue に herdr / orca 側の確認を含める。
- 検索語: `--prompt-file`, `initial prompt`, `stdin`, `--append-system-prompt-file`
- 置き換え後: プロンプトを `ToolLaunchSpec` に畳み込み、`submit_prompt` / `confirm_submission` /
  `submit_if_left_unsent` と orca の `wait_for_agent` を削る。

---

## 台帳の保守

- 新しいワークアラウンドを足したら、ここに項目を足す（id は kebab-case で一意に）。
- 置き換えが済んだ項目は**削除する**（issue と PR が履歴になる）。
- アンカーが当たらなくなったら、コードを読んで新しいアンカーに直すか、項目ごと消す。
