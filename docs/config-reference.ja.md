> 🌐 [English](config-reference.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/development/config-reference.md sha256:8f9df837ab33eb937dfa0aba4b6950cb8d0d62da4cc1ffe86b7735f79dc5273b -->

# 設定リファレンス

`config.toml` とプラグインごとの `plugins/{name}.toml` の全キーを、型・既定値・意味とともに示す。

## ファイルの場所

- 共通設定: `$XDG_CONFIG_HOME/totsuka/config.toml`（既定は `~/.config/totsuka/config.toml`）
- プラグイン個別設定: `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml`。totsuka は中身を解釈せずに保持し、シークレットを解決してからプラグインへ渡す
- `--config <path>` で `config.toml` の場所を上書きできる

`totsuka init` が雛形を書き出す。`totsuka config validate` で検証し、`totsuka config show [--redacted]` で表示する。

## シークレット参照

**平文のシークレットを設定に書かない。** 文字列値は次のいずれかにできる。

| 形式 | 解決元 |
|---|---|
| `keychain:<service>/<account>` | macOS Keychain |
| `op://<vault>/<item>/<field>` | 1Password |
| `cmd:<command>` | コマンドの標準出力 |
| `${ENV_VAR}` を含む文字列 | 環境変数 |

`~` と `${ENV}` はパスでも展開される。

**`op://`** は 1Password CLI を呼び出す形式で、事前に `op signin` 済みであることを前提とする。どちらの設定ファイルの**任意の文字列値**でも使え、CLI がクロスプラットフォームなので **macOS 以外で動く唯一のバックエンド**でもある。CLI 未導入・item 不在・未サインインは、それぞれ具体的で行動につながるエラーになる。`totsuka doctor` が 1Password を検査するのは、設定に `op://` 参照が実際にあるときだけである。

**`cmd:`** はコマンドを `/bin/sh -c` で実行し、その標準出力を秘密値として使う（末尾の改行は除去される）。`token = "cmd:gh auth token"` のように、**別のツールが管理・ローテートしている credential** 向けである — 毎回その時点の値を取るので、コピーが黙って古くなることがない。非ゼロ終了や空出力は起動時エラーで、stderr の先頭行を引用する（標準出力はどこにも引用しない）。コマンドが走るのは `totsuka run` がシークレットを解決するときだけで、パースや `config show` では実行されない。

**コマンド文字列に秘密を直書きしないこと。** 参照文字列は設定の一部としてエラーメッセージに引用されうる。「設定に平文の秘密を書かない」規則はコマンド文字列にも及ぶ — 秘密はコマンドに**取得させる**のがこの形式の目的である。

## トップレベルのキー

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `version` | int | 1 | 設定スキーマの版。不一致は起動時の検証で失敗する |
| `max_concurrency` | int? | 4 | 同時に走らせるタスク数の全体上限 |
| `[[repositories]]` | 配列 | — | 作業対象のリポジトリ |
| `[plugins.{name}]` | テーブル | — | どのプラグインが存在するかと、その共通設定 |
| `[[workflows]]` | 配列 | — | ワークフロー定義 |
| `[llm]` | テーブル | なし | AI ゲートウェイの設定。無いと、LLM が要るリポジトリ選択は `pending` へ縮退する |
| `[worktree]` | テーブル | — | worktree の配置と掃除 |
| `[log]` | テーブル | — | ログ |
| `[hooks]` | テーブル | — | エージェント CLI のフックイベント受信 |
| `default_tool` | string? | `"claude"` | ワークフローもリポジトリも指定しないときの既定 AI ツール |
| `[tools.{name}]` | テーブル | — | AI ツールのレジストリ。組み込みを上書き・拡張する |
| `[prompts]` | テーブル | — | AI ツールへ差し込むプロンプト文の上書き |

## スキーマのバージョニング

現行のスキーマは **v1** で、一度も上がっていない。

`version` が一致しない `config.toml` は起動時の検証で拒否され、**totsuka が設定を書き換えることはない**。`config validate` / `run` / `doctor` は同じ検証を共有するので 3 つとも同じ不一致に気づくが、扱いは異なる。`config validate` と `run` はエラーで停止し、`doctor` は `config` チェックの失敗として報告したうえで他のチェックを続行する。

案内は、どちらが遅れているかで逆になる。

- `version` が totsuka の想定より新しい → **totsuka が古い。** そのスキーマを理解する版へ更新する
- `version` が古い → **設定が古い。** `config.toml` を現行の形へ直し、`version` を書き換える

**`totsuka config migrate` は存在しない。**

## `[[repositories]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ブランチ名やログで使う安定した ID |
| `path` | string | 必須 | ローカルクローンのパス（`~` と `${ENV}` を展開） |
| `summary` | string? | なし | LLM がリポジトリを選ぶときに使う説明 |
| `tool` | string? | `default_tool` | このリポジトリへ渡されるタスクの既定 AI ツール。ワークフローの `tool` が優先される |
| `max_concurrency` | int? | 無制限 | リポジトリ単位の同時実行上限 |
| `worktree_location` | string? | `[worktree].location` | このリポジトリの worktree 配置テンプレートを上書きする |

## `[plugins.{name}]`

`{name}` は、ワークフローが `source` / `agent` で参照するインスタンス名。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `enabled` | bool | false | 有効かどうか。`totsuka plugin enable/disable` でも切り替わる |
| `kind` | enum | 必須 | `task_source` / `agent_ide` / `notifier` |
| `max_concurrency` | int? | 無制限 | agent プラグイン単位の同時実行上限 |
| `timeout_secs` | int? | 120 | プラグイン呼び出し 1 本のタイムアウト |
| `log_level` | string? | なし | プラグインのログレベル |
| `poll_interval_secs` | int? | 60 | task_source のみ。push 型ソースは totsuka からポーリングされず、この値はプラグインへ転送されてプラグイン内部の取得周期になる |

## `[[workflows]]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `name` | string | 必須 | ワークフロー名 |
| `source` | string | 必須 | task_source のインスタンス名 |
| `trigger` | テーブル | `{}`（全マッチ） | 一致条件。下記参照 |
| `profile` | enum? | なし | `answer` / `triage` / `design` / `implement` のいずれか。`mode` / `output` / `verification` をまとめて決める |
| `mode` | enum | `profile` が無ければ必須 | `plan` / `implement` |
| `agent` | string | 必須 | agent_ide のインスタンス名 |
| `output` | enum | `profile` が無ければ必須 | `source` / `none` |
| `on_success` | `{ set_status = "..." }`? | なし | 成功時にソース側のステータスを更新する |
| `on_failure` | `{ set_status = "..." }`? | なし | 失敗時にソース側のステータスを更新する。再試行可能な失敗では書き戻さない |
| `verification` | enum | `llm` | 完了申告の検収方式。`llm`（セッション内で検収）/ `human`（`totsuka task verify` を待つ）/ `none`。`profile` とは併記できない |
| `timeout_secs` | int? | 1800 | 最後のシグナルから無応答が続いてエスカレートするまでの秒数。**`0` はこのワークフローを掃引の対象外にする** |
| `rubric` | string? | なし | `llm` 検収で使う判定基準 |
| `[workflows.prompts]` | テーブル | — | このワークフロー専用のプロンプト上書き。最も強い層 |
| `tool` | string? | なし | AI ツールのピン。ワークフロー > リポジトリ > `default_tool` |
| `initial_prompt` | string? | なし | このワークフローのエージェントへ前置きする追加指示。下記参照 |

ワークフローは定義順に照合され、最初に一致したものが使われる。同一ソース内でトリガーが重なると警告が出る。**catch-all（`trigger = {}`）より後に定義した同一ソースのワークフローは到達不能**で、こちらも警告になる。

`timeout_secs = 0` は、人間が pane を見ている運用向けである。真にハングしたエージェントも検知されなくなるので、無人のワークフローには設定しないこと。

`verification = "llm"` が Claude 以外のツールへ解決されうる構成では、`tool = "claude"` のピンを勧める警告が出る。セッション内検収には Claude のフックが要るためである。

### trigger の予約キー

次のキーは totsuka が正規化済みのタスクに対して再判定する。それ以外は不透明な値としてプラグインへ素通しされ、プラグインが解釈する。

| キー | 照合先 |
|---|---|
| `status` / `project_status` | タスクのステータス |
| `label`（文字列）/ `labels`（配列） | タスクのラベル。配列は全部必要 |
| `reaction` | `reaction:<絵文字名>` のラベル |

### `reaction` — 絵文字でワークフローを選ぶ

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }     # :hammer: を自分で付けたら実装タスク
profile = "implement"
agent = "herdr"

[[workflows]]
name = "slack-reply"                  # メンション: catch-all。必ず最後
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

- **リアクションのワークフローは catch-all より前に定義する。** 後ろに置くと到達不能で、絵文字を付けても何も起きない（警告は出る）
- 絵文字名は Slack が報告する形（コロン無し）の**文字列**。`":eyes:"` と書いてもコロンは剥がされる。👀 は `eyes`、👁 は `eye` で別物である
- **`reaction = 123` のような文字列以外の値は起動時エラー。** 読めない予約キーは照合時にスキップされる仕様なので、放置すると逆方向に 2 つ壊れる — そのワークフローが全タスクに一致し（catch-all より前にあるのでメンションを吸う）、一方でプラグインは絵文字を 1 つも登録しない。どちらも単体ではエラーを出さない
- **同じ絵文字を 2 つのワークフローに書くと設定エラー。** first-match で片方が黙って勝つのを許さない
- `plugins/slack.toml` の旧 `trigger_reactions` との併用もエラーになる。旧記法だけの構成は非推奨警告つきで従来どおり動く
- 起動するのは自分が付けたリアクションだけで、これを緩める設定は無い

**混在バージョンの注意:** 新しいプラグインを古いコアと組み合わせると、コアに `reaction` 予約キーが無いためリアクションのワークフローが全タスクを吸う。コアを先に、プラグインを後に上げること。戻すときはリアクションのワークフローを設定から外す。

### `initial_prompt`

```toml
[[workflows]]
name = "github-design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"
agent = "herdr"
on_success = { set_status = "設計レビュー待ち" }
initial_prompt = "/grill-me スキルを使用して、詳細設計を行ってください"
```

| 性質 | 挙動 |
|---|---|
| **可視** | pane に見える形で入る。タスクの進め方を丸ごと変えうる指示なので、後から追えるようにしてある |
| **先頭** | タスク本文の前に置かれる |
| **新規会話のみ** | 会話を再開するディスパッチでは入らない。開始宣言のような指示が 3 ターン目に再入力されるとスキルが再起動して文脈を壊すためである。再開できないツールは毎回が新規会話なので毎回入る |
| **リテラル** | プレースホルダ展開を通さないので `{` をそのまま書ける |
| **未設定なら現状どおり** | 空文字列や空白のみは未設定と同じ扱いで、設定していないワークフローの挙動はバイト単位で変わらない |

**人間に問いかけさせる指示を書くと、無人 pane ではハングする。** ツールの応答待ちでは何も発火しないので、`timeout_secs` でエスカレートするまで止まる。totsuka は但し書きを自動で足さない — 書いた内容と矛盾する指示が混ざりうるためである。

### `profile` — 4 つの原型

噛み合う `mode` / `output` / `verification` の組み合わせに名前を付けたもの。

| profile | mode | output | verification | 想定用途 |
|---|---|---|---|---|
| `answer` | `plan` | `source` | `llm` | 質問に答え、ソースへ返信する |
| `triage` | `plan` | `source` | `llm` | GitHub や Notion へ起票する |
| `design` | `plan` | `none` | `llm` | 詳細設計を issue コメントやページへ書く |
| `implement` | `implement` | `none` | `llm` | 実装してプルリクエストを出す |

```toml
[[workflows]]
name = "gh-design"
source = "github"
trigger = { project_status = "設計待ち" }
profile = "design"
agent = "herdr"
on_success = { set_status = "設計済み" }
```

| 組み合わせ | 結果 |
|---|---|
| `profile` + `mode` / `verification` | **エラー。** profile が決める値なので、書くと「生きて見える死んだ設定」が残る |
| `profile` + `output` | **可。** `output` が勝つ。権限ではなく配線先の選択であり、Slack 起点の implement がプルリクエストの URL をスレッドへ返すのに要る |
| `profile` 無しで `mode` / `output` が欠けている | **エラー。** profile を書くか、両方を明示する |
| `profile` + `rubric` / `[workflows.prompts]` / `tool` / `timeout_secs` / `on_success` / `on_failure` | 可 |

profile は必須ではない。4 原型で表せない組み合わせ（たとえば `verification = "human"` — 4 原型はいずれも `llm` に解決する）は明示記法で書く。

**戻すときの注意:** `profile` を書いた設定は、古いバイナリではパースエラーになる。totsuka を戻すときは設定も戻すこと。

profile はこの 3 キー以外にも次を決める。

| 決まる挙動 | 対象 profile |
|---|---|
| Claude の設定へ `permissions.deny` を注入する | answer / triage / design |
| `Bash` をツールごと deny する（コマンドを 1 つも実行できない） | answer |
| Claude の `--permission-mode plan` を**渡さない** | answer / triage / design |
| worktree がブランチ上にあったら成功扱いにせず失敗させる | answer / triage / design |
| Claude の設定へ `permissions.defaultMode = "auto"` を注入する | 全 profile |
| ソースプラグインへ、どの種類の指示を載せるかを伝える | triage / design / implement |
| 検収基準を「成果物の URL が実在すること」に差し替える | triage |
| 完了申告の指示を、後述の確認プロトコル版に差し替える | design / implement |
| 検収基準を「人間が明示的に承認したか」に差し替える | design / implement |
| ソースプラグインへ、会話とは別 ID でタスクを立てるよう伝える | implement / triage |
| 必要な外部ツールが無ければディスパッチ前に待機させる | implement |

### design と implement の完了は人間が承認する

`design` と `implement` は人間が pane を見ている前提の profile で、**完了の最終判断は人間が行う**。

1. エージェントは作業を終えたと思っても完了を**申告せず**、内容を要約して確認を求め、「入力待ち」で停止する
2. totsuka はタスクを入力待ちとして park する（掃引の対象外・並列枠の解放・通知の送信）
3. 人間が pane 上で明示的に承認すると、エージェントが完了を申告してタスクが終わる

検収基準もこれに合わせて変わり、会話を見られるジャッジが「申告より前に人間が承認しているか」を判定する。**確認を飛ばして完了を申告したエージェントは、マーカー欠落を止めるのと同じ層でブロックされる。** 確認のための停止は完了申告ではないので、ブロックされない。

長い自走中の誤エスカレートを避けたいなら `timeout_secs = 0` と併用する。

既知の制限として、入力待ちの最中の 2 回目の問い合わせ（修正指示 → 再確認）では通知が飛ばない。人間が会話の当事者なので実害は小さい。

### 検収基準の優先順位

強い順に次の 5 層。

1. `[[workflows]].prompts.verification_rubric`
2. `[[workflows]].rubric`
3. `[prompts].verification_rubric`（グローバル）
4. profile の既定
5. 汎用の既定

**3 が 4 より強い**ため、グローバルの `verification_rubric` を設定済みだと `triage` のワークフローでも URL 検収にならない。症状は「投稿していない設計を『書いた』と申告したタスクが通る」である。profile を使うなら、グローバルの rubric を外すか `[[workflows]].rubric` で明示すること。**同じ梯子が完了申告の指示にも効く** — グローバルの `marker_self_report` を設定済みだと、`design` / `implement` でも確認プロトコル版にならない。

### 外部ツールが無いときの待機

`implement` のタスクはプルリクエストを作るので `gh` が要る。無ければ**ディスパッチされず待機**し、通知が一度出る。環境を整えれば数分以内に自分で流れ出すので、操作は要らない。

通知は流れて消えるので、`totsuka status` にも理由が出る。

```text
not starting yet:
  task 12 (2026-08-11T09:00:00Z): gh unavailable in the orchestrator's environment → …
```

`--json` ではタスクの `wait_reason` に入る。**表示は totsuka が記録した内容で、`status` はツールを再検査しない** — status はあなたのシェルで走るので、そこで `gh` が見えても totsuka から見えているとは限らないためである。ディスパッチできれば表示は自動で消えるが、**`totsuka run` が止まっている間に環境を直しても消えない**（次に `run` が回ったときに消える）。

**この検査は間違うことがある。** 判定は totsuka のプロセスで走り、エージェントはシェル環境が効いた pane で走るので、**pane からしか `gh` が見えない構成では「無い」と判定される**。そのため `doctor` は失敗ではなく警告として報告し、ディスパッチも失敗させず待機にとどめる。心当たりがあればこの警告は無視してよい。

**検査しない範囲:** `triage` と `design` も外部へ書くが、**どこへ**書くかはソース依存で、totsuka はプラグインのインスタンス名から判別できない。推測して誤ると動いたはずのタスクを止めるので、検査しない。`doctor` はその旨をスキップ行で明示する。検査するのは「設定されているか」だけで、`gh auth status` は実行しない。期限切れのトークンはここを通り、従来どおり pane で失敗する。

### リアクションで実装タスクを起こす

実行中タスクの権限を広げるのではなく、リアクションで別のタスクを起こす。

```toml
[[workflows]]
name = "slack-implement"
source = "slack"
trigger = { reaction = "hammer" }
profile = "implement"
output = "source"                 # プルリクエストの URL をスレッドへ返すため
agent = "herdr"

[[workflows]]
name = "slack-reply"              # catch-all。必ず最後
source = "slack"
trigger = {}
profile = "answer"
agent = "herdr"
```

- タスク ID はスレッドの回答タスクと衝突しない別 ID になる
- **エージェントが見る文脈は、どこにリアクションしたかで決まる。** スレッドの先頭（またはスレッド外の単独メッセージ）なら会話全体、スレッド内の返信 1 つならそのメッセージのみ
- リポジトリは会話から引き継ぐ。回答タスクが解決済みならそれを使い、LLM 呼び出しもピッカーも走らない
- 報告は承認ゲートを通る。実装報告こそ誤送信の影響が大きいためである

**制限。** スレッドの取得は 200 件でクランプされるので、それを超えるスレッドは古い方から欠ける。また回答タスクが実行中のうちにリアクションを付けると 2 つのタスクが並走する。別 worktree なので壊れないが、方針が決まる前に実装が始まることになる。

### ソースプラグインの指示文

`plugins/github.toml` と `plugins/notion.toml` は `[prompts]` テーブルを受け付ける。profile がタスクの種類を伝えたときに、そのプラグインが載せる書き込み先の指示である。

| キー | 使われるとき | プレースホルダ |
|---|---|---|
| `triage_instructions` | `profile = "triage"` | github: `{issue_number}` `{repo}` / notion: `{page_url}` `{title}` |
| `design_instructions` | `profile = "design"` | 同上 |
| `implement_instructions` | `profile = "implement"` | 同上 |

いずれも省略可。**profile を使わない構成ではこのキー群は一切使われず**、タスクの指示は従来どおり空になる。

Slack ソースは同じ種類の情報を読んで、自前の 3 キーから選ぶ。**選択は種類であってタスク ID の接頭辞ではない** — `triage` と `implement` はどちらも接頭辞を持つので、接頭辞で分岐すると triage のタスクに実装指示が渡る。種類が不明なときは推測せず返信指示へ縮退する。

**`profile = "design"` を Slack ソースに書くと何も起きない。** Slack プラグインは design の指示文を持たず、しかも `design` は何も出力しないので、指示を受けたエージェントが動いて結果がどこにも出ない。設定の検証は通ってしまうので、プラグインはディスパッチ時に警告ログを出す。Slack 起点で起票させたいなら `triage` を使う。

展開はシングルパスである — issue のタイトルやページ名は他人が書ける内容なので、そこに書かれた `{placeholder}` は文字列として挿入されるだけで指示にはならない。

## `mode = "plan"` は git を構造的には止めない

plan モードは「worktree は作るが push もプルリクエスト作成もしない」モードとして定義され、実装も permission mode やサンドボックスがそれを担保する前提で書かれてきた。**実機ではその前提が破れた** — plan モードのタスクがブランチを切り、コミットし、push し、プルリクエストまで作成した。対象リポジトリ自身の規約が「終わったら push して PR を作れ」と指示していたためである。Claude の `--permission-mode plan` に至っては、plan のままファイルが書かれた実測がある。**書き込みを止める機構として数えないこと。**

**profile を付けない素の `mode = "plan"` は今も検出だけ**である。worktree にブランチが現れると `run` が警告を出す。既存の構成がアップグレードで黙って厳しくならないよう、意図的に警告のままにしてある。

**profile を書いたワークフローは失敗する。** ただし **read-only な profile の read-only 性は保証ではない** — OS レベルで封じるサンドボックスは実現可能と実測済みだが、実装しないと決めた。`cat >` でのファイル書き込みや、`&&` やパイプを挟んだ git / gh は deny を素通りする。read-only profile のタスクの worktree がブランチ上にあると、成果物を公開せず失敗し、worktree とコミットは調査用に保持される。**これは防止ではない** — ブランチがある時点で push は済んでいるかもしれず、取り返せない。失敗させることで「黙って成功」を避けているだけである。復帰するには worktree を detach してから `totsuka task retry`（そのままの retry は同じ検査で再び落ちる）か、`totsuka task cancel` を使う。

副作用の無いモードとして plan を選ぶなら、**対象リポジトリの規約に push や PR 作成を指示する記述が無いか**を確認すること。

## `[tools.{name}]`

pane 内で起動する AI ツール CLI の定義。組み込みとして `claude` / `codex` / `opencode` が常に存在し、同名のエントリで上書きできる。`claude-fast` のように同じ種別の別プロファイルも定義できる。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `kind` | enum | 必須 | アダプタ種別: `claude` / `codex` / `opencode`。コマンドラインの組み立て方と完了検知の方式を決める |
| `command` | string? | kind 名 | 空白区切りのコマンドライン。先頭がプログラムで残りが基本引数（例 `"claude --model haiku"`） |
| `mode_args` | string[]? | kind ごと | implement モードで追加する引数。codex: `["--sandbox", "workspace-write", "--ask-for-approval", "never"]`、opencode: `["--auto"]`、claude: なし |
| `plan_args` | string[]? | kind ごと | plan モードで追加する引数。claude: `["--permission-mode", "plan"]`、codex: `["--sandbox", "read-only", "--ask-for-approval", "never"]`、opencode: `["--agent", "totsuka-plan", "--auto"]` |

`kind = "codex"` はツール側での一回きりの信頼設定が要る。`kind = "opencode"` は信頼設定こそ不要だが、縮退する箇所が多い。

アダプタは、再開の仕方とフック設定の受け取り方が異なる。claude は設定ファイルを受け取りフラグで再開し、codex はフックをグローバルに登録してサブコマンドで再開し、opencode もグローバル配置でフラグで再開する。opencode は不可視の注入ができないため、タスクの指示とマーカー規約は pane から見える形で渡る。

### 承認プロンプトで止まらないこと

**pane には答える人が居ない**ので、3 ツールとも人間に確認を求めない設定で起動する。

| ツール | 設定 | どこで |
|---|---|---|
| claude | `permissions.defaultMode = "auto"` | 設定ファイル（profile のあるワークフローのみ） |
| codex | `--ask-for-approval never` | plan / implement 両方の既定引数 |
| opencode | `--auto` | plan / implement 両方の既定引数 |

**これはエージェントにできることを広げる設定ではない。** 境界はそれぞれ別の機構が持っており、この設定はそれを緩めない。claude の deny はどの permission mode でも適用され、codex の `--sandbox` は承認ポリシーとは別のフラグであり、opencode の `--auto` は「**明示的に拒否されたものを除いて**自動承認する」なので plan エージェントの deny はそのまま残る。

変わるのは、**境界が拒否しないもの**について人間に聞くかどうかだけである。

放っておくとどうなるかは実測してある。何も設定していない claude は手動モードで起動し、allowlist に無いコマンドの手前で確認を出したまま動かなくなる。codex はモデルが必要と判断したときに聞き、opencode もいくつかの分類について聞く。

**`mode_args` / `plan_args` を明示すると既定を丸ごと置き換える**ので、これらのフラグも消える。無人で回すなら自分で書き足すこと。

ディスパッチ時のツール解決は、ワークフローのピン > リポジトリの既定 > `default_tool` > 組み込みの `claude` の順である。

## `[prompts]`

AI ツールへ差し込むプロンプト文の上書き。組み込みの既定はバイナリに埋め込まれており、このテーブルは**キー単位の上書き**である（未指定のキーは組み込みのまま）。値はインライン文字列のみで、ファイルを指す形式は無い。

| キー | 既定 | 内容 | プレースホルダ |
|---|---|---|---|
| `marker_self_report` | 組み込み（profile で分岐） | 全ディスパッチに注入される完了自己申告の指示。`design` / `implement` の既定は確認プロトコル版。このキーを上書きすると profile の分岐より優先される | `{marker_completed}` `{marker_needs_input}` `{marker_failed}` |
| `branch_convention` | 組み込み | ブランチ作成の指示。worktree は detached で渡されるので、エージェントがリポジトリの規約を読んで自分で切る。**plan モードでは注入しない**。既にブランチ上のタスクにも注入しない | なし |
| `verification_rubric` | 組み込み（profile で分岐） | 完了申告を許可してよい条件。**命令文ではなく条件節として書く** — 下記参照 | — |
| `verification_background_exemption` | 組み込み | バックグラウンド実行中の中間停止を許可する条件節 | — |
| `verification_nonclaim_exemption` | 組み込み | 「入力待ち」「失敗」を報告した停止を許可する条件節 | `{marker_needs_input}` `{marker_failed}` |
| `verification_marker_convention` | 組み込み | ブロックするときに理由へ何を書かせるか。理由はエージェントへ差し戻されるので、ここでマーカー規約を教える | `{marker_completed}` `{marker_needs_input}` `{marker_failed}` |
| `verification_prompt` | 下記 | 各条件節の組み立て方 | `{rubric}` `{background_exemption}` `{nonclaim_exemption}` `{marker_convention}` |
| `opencode_plan_agent` | 組み込み | opencode の plan エージェントファイルの**散文本体**。**グローバル専用** — ディスク上の 1 枚を全セッションが共有するので、ワークフロー配下に書くとパースエラーになる | — |

> **`verification_*` は「命令」ではなく「条件」である。** Claude Code はフック本文を固定のシステムプロンプト配下でモデルに渡し、判定を受け取る。否定の判定が停止をブロックし、その理由がエージェントへ差し戻される。**モデルはブロックを制御していないので、「ブロックせず許可してください」と書いても効かない。** その形を一度出荷したことがあり、実機ではジャッジが当該文言を逐語引用しながら 8 回連続で拒否した。ここに書くテキストは、**許可してよい全ケースで真になる条件**として書くこと。

`verification_*` の 5 キーが使われるのは `verification = "llm"` のワークフローだけである。必要なフックを持つのは Claude だけで、他のツールでは `human` へ縮退する。

`opencode_plan_agent` は**散文本体のみ**である。frontmatter は Rust 側で固定されていて設定できない — その deny マップが plan の意図を運ぶ唯一の機構であり、散文に見えるキーから allow を注入できると権限昇格になるためである。値が `---` の行をどこかに含むと検証エラーになる。frontmatter は慣例上ファイル先頭でしか解釈されないが、ここは権限境界なのでその推論に依存しない設計にしてある（本文の水平線は `***` で書ける）。

**マーカー自体は設定できない。** フックスクリプトがリテラルとしてパースしており、3 ツール共通の唯一の完了信号だからである。ここで編集できるのは規約を**教える散文**であって、規約そのものではない。

### 優先順位

強い順に 4 層。

1. `[[workflows]].prompts.<key>` — このワークフロー専用（`opencode_plan_agent` を除く。グローバル専用のため）
2. `[[workflows]].rubric` — レガシー。rubric にのみ効く
3. `[prompts].<key>` — グローバル
4. 組み込みの既定

**2 が 3 より強いのは意図的である。** どちらもワークフロー単位の設定なので、逆順にすると、グローバルの `verification_rubric` を新たに足した瞬間に既存のワークフロー個別の `rubric` が黙って上書きされる。

### 展開規則

- プレースホルダの置換は**シングルパス**。置換された値の中の `{token}` は再展開されない
- 組み立ては 2 段階で、各段がシングルパスなので、rubric 内に書いたリテラルの `{marker_convention}` は挿入されるだけで展開されない
- プレースホルダ名は識別子に限られるので、それ以外の波括弧は中身として素通しされ、`{"ok": true}` のような JSON をプロンプトに書ける。**その裏返しとして、識別子でない綴りのタイポはプレースホルダ検査では捕まらない。** マーカーについては、3 つすべてが組み立て後の出力に現れることを別途検査している
- 波括弧の中にさらに `{` があると、その範囲全体が 1 つの未知の名前として素通しされ、中の本物のプレースホルダが落ちる。この形は警告として報告される
- `[worktree]` のテンプレートは置換方式が異なり、**波括弧の中身がすべて検査される**ので、`{repo-name}` のようなタイポはエラーのままである
- 未知のプレースホルダはそのまま出力される
- **プロンプトの変更は次のディスパッチから有効になる。** 既に起動しているエージェントには届かない

### 例

```toml
[prompts]
verification_rubric = "変更が意図どおり動くことを、実際にテストを走らせて確認してください。"

[[workflows]]
name = "slack-reply"
source = "slack"
mode = "implement"
agent = "herdr"
output = "source"
verification = "llm"

  [workflows.prompts]
  verification_rubric = "返信案が質問に直接答えているか、根拠が示されているかを検証してください。"
```

## `[llm]`

OpenAI 互換の `/chat/completions` を前提とする。ヒントを持たないタスクのリポジトリ選択に使うほか、task_source プラグインへ分類用の既定として供給される（プラグイン自身の LLM 設定が常に優先される）。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `base_url` | string | 必須 | ベース URL（例 `https://openrouter.ai/api/v1`） |
| `model` | string | 必須 | モデル名 |
| `max_tokens` | int? | 256 | 分類呼び出しの最大トークン |
| `timeout_secs` | int? | 30 | リクエストのタイムアウト |
| `api_key_ref` | string? | なし | API キーのシークレット参照 |

## `[worktree]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `location` | string? | `<state dir>/worktrees/{repo_name}/{worktree_name}` | 配置テンプレート。`{repo}` `{repo_name}` `{worktree_name}` `{task_id}` `{source}` `${ENV}` `~` を展開する。**`{branch}` は廃止された** — ブランチは worktree ができた後にエージェントが決めるので、ディレクトリ名には使えない。残っていると起動しない |
| `cleanup` | policy? | `manual` | implement モードの掃除ポリシー |
| `plan_cleanup` | policy? | `immediate` | plan モードの掃除ポリシー |

**既定値の解決。** `location` を省略したときの `<state dir>` は `$XDG_STATE_HOME/totsuka` で、未設定なら `$HOME/.local/state/totsuka` にフォールバックする。既定値は解決済みのパスとして組み立てられるので `${ENV}` 展開を経由しない。逆に `location` を**明示した場合、未設定の `${ENV}` は空文字ではなくエラー**になり、worktree の作成はディスパッチ時なので、起動時ではなく毎タスクの失敗として現れる。`doctor` の `worktree-location` チェックが事前に検出する。

ポリシーの値は `"immediate"` / `"manual"` / `{ retention_days = 5 }` / `"keep_7d"` / `"keep_28d"`。`keep_*` は 7 日・28 日の糖衣で、他の日数は明示形式で書く。未コミットの変更がある worktree は決して削除されない。

```toml
[worktree]
cleanup      = "keep_7d"              # implement: 7 日保持してから削除
plan_cleanup = "immediate"            # plan: 即削除（既定）
# cleanup    = { retention_days = 3 } # 任意の日数は明示形式で
```

**pane は worktree に連動する。** worktree を削除すると判定したとき、先にそのタスクの pane が閉じられる。保持された worktree（retention 未経過・`manual`・未コミット変更あり）の pane は残る。**既定の `cleanup = "manual"` では worktree も pane も自動では消えず、タスクごとに pane が増えていく。** コミット済み未 push の作業を pane で確認したい運用でなければ、`keep_7d` を勧める。

## `[log]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `level` | string? | info | `error` / `warn` / `info` / `debug` / `trace`。`--debug` で debug へ引き上がる |
| `log_prompts` | bool | true | プロンプトとペイロードを記録する。実際に出力されるのは debug 以上のときだけ |
| `max_files` | int? | 7 | 日次ログの保持世代数 |

## `[hooks]`

エージェント CLI のフックイベント受信の設定。全キー省略可。

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `auth_token_ref` | string? | なし | フックの POST を認証する Bearer トークンのシークレット参照（例 `keychain:totsuka/hook-token`）。**運用上は必須** — 未設定だとソケットのパーミッションだけが防御になる |
| `socket_path` | string? | 組み込み既定 | 受信ソケットのパス |
| `spool_dir` | string? | 組み込み既定 | POST に失敗したイベントを退避するディレクトリ |
| `block_retry_limit` | int? | 3 | 停止のブロック差し戻しの連続上限。超えるとエスカレートする |

フック対応のエージェントを使うワークフローがある構成で `auth_token_ref` が未設定だと、`config validate` と `run` がワークフローごとに警告を出し、`doctor` は**失敗**する。フック対応エージェントを使わない構成では `doctor` は警告のみ。参照を設定したのに解決できない場合は、構成によらず失敗する。

## `plugins/slack.toml`

Slack ソースはイベント駆動で、受け取ったイベントをその場で push するため `poll_interval_secs` は使わない。

```toml
[plugins.slack]
enabled = true
kind = "task_source"
```

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `app_token` | string | 必須 | ソケット接続用の App-Level Token（`xapp-`）。シークレット参照を推奨 |
| `user_token` | string | 必須 | 本人名義の読み書きに使う User OAuth Token（`xoxp-`）。シークレット参照を推奨 |
| `bot_token` | string? | なし | Bot Token（`xoxb-`）。設定すると、返信案やピッカーの到着時に bot が本人へ DM を送る。**未設定なら機能が off になるだけ**（起動時に警告 1 回） |
| `target_user_id` | string | 必須 | 自分の Slack ユーザー ID。このユーザー宛のメンションがタスクになり、トークン自身の identity とも照合される |
| `trigger_reactions` | string[] | `[]` | **非推奨**。ワークフローの `trigger.reaction` を使う。**自分が付けると**タスクを起こす絵文字名。コロンは剥がされる。`reactions:read` スコープが要る |
| `thread_context_limit` | int | 6 | タスク本文に含めるスレッド直近メッセージ数 |
| `reply_style` | string? | なし | タスク本文へ注入する返信トーンの指示 |
| `[prompts]` | テーブル | — | このプラグインが送るプロンプト文の上書き |
| `source_name` | string | `slack` | 各タスクに刻印するソース名 |
| `[[repos]]` | 配列 | なし | リポジトリ候補。`name`（`config.toml` のものと一致必須）と、任意の `summary` / `path`。**省略すると `config.toml` のリポジトリがそのまま候補になる**ので、通常は書かなくてよい |
| `[[channel_groups]]` | 配列 | なし | チャンネル名の接頭辞で候補を絞る規則。定義順に first-match。`prefix` と `repos` を持つ |
| `[llm]` | テーブル | なし | 分類用の LLM。`base_url` / `model` / `api_key` / `confidence_threshold`（既定 0.6、下回るとピッカーへ）。**省略すると `config.toml` の `[llm]` が既定になる**（キーが解決できる場合のみ）。候補が 2 件以上でどちらにも無ければ起動に失敗する |
| `api_url` | string | `https://slack.com/api` | Web API のベース URL（テスト用） |
| `max_retries` | int | 3 | 再試行可能な API 失敗の最大再試行回数 |

### Slack ソースの `[prompts]`

このプラグインが送るプロンプト文のキー単位の上書き。キー名がそのまま設定キーである。

| キー | 用途 | プレースホルダ |
|---|---|---|
| `reply_instructions` | 返信案の作成指示。`answer` の既定であり、種類が不明なときのフォールバックでもある | — |
| `implement_instructions` | `implement` の既定。実装してプルリクエストを作り、その URL を報告に含めさせる | — |
| `triage_instructions` | `triage` の既定。issue を起票し、その URL を報告に含めさせる | — |
| `reply_style_suffix` | `reply_style` が設定されているときだけ返信指示に追記される | `{style}` |
| `body_template` | pane に表示されるタスク本文 | `{sender}` `{channel}` `{text}` |
| `body_thread_header` | スレッド文脈セクションの見出し | `{count}` |
| `body_thread_line` | スレッド文脈 1 行ぶん | `{line}` |
| `body_thread_unavailable` | 文脈の取得に失敗したときにセクションごと差し替わる文 | — |
| `classifier_system` | リポジトリ分類の system プロンプト | `{repo_names}` |
| `classifier_user` | 対応する user メッセージ | `{mention_text}` `{thread_context}` `{catalog}` |
| `classifier_correction` | 応答が JSON として壊れていたときの再試行ターン | — |

注意点。

- **`{text}` は引用済みで渡る。** 書き換えは展開の前に行われるので、先頭の `> ` を落としたテンプレートでも継続行は壊れず、`> ` を残しても二重引用にならない
- **`{text}` `{thread_context}` `{catalog}` の中身は Slack の投稿者が決められる。** 展開はシングルパスなので、本文に `{catalog}` と書かれたメンションはその文字列として挿入されるだけで、候補リストの差し込みにはならない
- 未知のプレースホルダはそのまま出力され、起動時に警告としてログに出る。**エラーにしないのは意図的である** — 症状は下書きに見える `{token}` であって目に見える。コアの `[prompts]` がエラーにするのは、あちらで消えるのが完了マーカーの規約で、症状がタイムアウトによるエスカレーションしか無いからである
- ここは **LLM 向けのプロンプトのみ**である。悪い上書きは分類の劣化や返信案の質低下に留まり、コアの `[prompts]` と違って完了検知は壊せない

## `plugins/herdr.toml`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `socket_path` | string? | なし | ソケットの明示パス。解決順の最上位 |
| `session` | string? | なし | 名前付きセッション。`socket_path` 未設定時に使う |
| `[layout]` | テーブル | 下記 | ディスパッチした pane の配置 |
| `[kind_map]` | テーブル | `{}` | 実行ファイル名を herdr 側の語彙へ写像する |
| `[identity]` | テーブル | `{ enabled = true }` | ディスパッチがリポジトリとタスクを herdr へ報告するか |
| `request_timeout_secs` | int | 30 | ソケット呼び出し 1 本あたりのタイムアウト |

ソケットの解決順は `socket_path` > `session` > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > 既定パス。

**herdr は 0.7.5 以降が必要。** それより古い herdr に対しては初期化を拒否し、`config validate` と `doctor` がバージョンを名指しで報告する。

### `[identity]`

```toml
[identity]
enabled = true   # 既定
```

ディスパッチは workspace と root pane の**両方**へメタデータを報告する。サイドバーはパネルごとに名前の解決先が違うので、片方だけでは片方のパネルしか直らない。

| token | 値 |
|---|---|
| `totsuka_task` | タスク ID を**そのまま**。比較に使う機械識別子なので整形も切り詰めもせず、上限に収まらない ID は**送らない**（切れた識別子は無い識別子より悪い） |
| `repo` | リポジトリ名（古いオーケストレータからは届かないので省かれる） |
| `task` | タスクのタイトル。**表示用**なので空白を畳んで切り詰める |
| `mode` | `plan` / `implement` |

**totsuka はサイドバーの行構成を書き換えない。** herdr の設定は herdr と運用者のものである。**スニペットを入れていない環境では、報告してもラベル以外は何も変わらない。**

**両方の報告が成功したときだけ**、workspace のラベルを `{repo}: {タイトル}` に rename する。機械可読な所有マーカーは workspace の作成時点で書かれるので、rename に失敗しても残る。リポジトリ名が無ければ rename しない。

**報告の失敗はディスパッチを落とさない**（警告のみ）。identity は装飾であり、herdr が一瞬詰まっただけで走れるタスクを失うほうが高くつく。

`enabled = false` で報告が止まる。

### `[kind_map]`

herdr は実行ファイルを自身の固定語彙から選ぶので、プラグインは**ファイル名**をその語彙へ翻訳する。`claude` / `codex` / `opencode` はそのまま通るため、通常このテーブルは要らない。必要になるのは、herdr が知らない名前のラッパースクリプトを使うときだけである。

```toml
[kind_map]
my-claude = "claude"
```

- キーは**ファイル名**と比較する（パスではない）。`/opt/bin/my-claude` は `my-claude` で引く
- 値は検証しない。未知の値は herdr 自身が拒否する。語彙をこちらに複製すると、herdr が増やしたときに黙って食い違う
- `[tools]` レジストリには置かない。あちらは agent に依存しない共有設定なので、herdr 固有の語彙を持ち込むと herdr を使わない構成にも漏れる

### `[layout]`

| キー | 型 | 既定 | 意味 |
|---|---|---|---|
| `shell` | bool | `true` | 併設シェル pane を出すか。`false` ならエージェントが全画面になり、他の 2 キーは無視される |
| `direction` | `"down"` / `"right"` | `"down"` | 分割方向。他の値は起動時エラー（`up` / `left` は herdr に存在しない） |
| `ratio` | float | `0.8` | **エージェント側**の取り分。**範囲検査はせず**そのまま渡す |

- 既定は「エージェントを上 80%、シェルを下 20%」
- **併設シェルにはフックの環境変数（Bearer トークンを含む）が渡らない。** 人間が直接叩くシェルにトークンを常駐させないためである
- **レイアウトの失敗はディスパッチを落とさない。** 警告を出して続行し、シェルなし（または herdr の既定配置）に落ちる。`ratio` が不正で herdr が拒否した場合も同じ経路になる

## 例

設計から実装へのハンドオフ。

```toml
[[workflows]]
name = "design"
source = "github"
trigger = { project_status = "設計待ち" }
mode = "plan"
agent = "herdr"
output = "source"
on_success = { set_status = "設計レビュー待ち" }

[[workflows]]
name = "implement"
source = "github"
trigger = { project_status = "実装待ち" }
mode = "implement"
agent = "herdr"
output = "source"
on_success = { set_status = "レビュー待ち" }
```

---

このページは内部ドキュメント `ai-docs/development/config-reference.md` から生成されている。設計上の判断や実測の経緯はそちらにある。
