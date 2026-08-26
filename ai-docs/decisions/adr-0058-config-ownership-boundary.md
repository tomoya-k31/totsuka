---
type: Decision
title: ADR-0058 設定の所有はファイル位置ではなく宣言で切り、config.toml へ一本化する
description: "プラグイン固有の設定項目の定義と検証をプラグインへ委譲するための設計。plugins/{name}.toml を廃止して config.toml のトップレベル [<name>] へ移し、[[workflows]] の追加キーはフラットに書いて source と agent の両方へ送り「ちょうど 1 つが引き取る」を規則にする。ワークフロー選択はプラグインが task/submit で名指しし、core の予約 trigger 語彙（reaction / project_status / label）を撤廃する。repo→トラッカーの紐付けは [[projects]] と [[repositories]].project へ移し ADR-0056 を置き換える。manifest への静的スキーマ宣言・名前空間つきの記法・互換のための二重読みは不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/554
tags: [decision, config, protocol, plugin, workflow, projects, adr]
generated: { by: claude-code/opus-5, at: 2026-08-25T21:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[#554](https://github.com/tomoya-k31/totsuka/issues/554) の実装とともに確定した。protocol 0.6.0（破壊的）。

置き換える決定:

- [ADR-0056](/decisions/adr-0056-multi-tracker-routing.md) §4（repo→トラッカーの順方向マッピングはプラグイン設定を正本にする）を**置き換える**。同 ADR の §1〜§3（複数対象は単一プラグイン内のリストで持つ／旧トップキーの削除／要素の絞り込み）は生きている
- [ADR-0057](/decisions/adr-0057-per-workflow-publish-and-cleanup.md) の配送方式の**運び方**を変える。`publish` という設定と「triage の報告は承認を飛ばしてよい」という判断はそのまま、読む主体が core からプラグインへ移る
- [ADR-0025](/decisions/adr-0025-reaction-task-trigger.md) の不変条件（本人が付けたリアクションでだけ起動する）は**不変**。変わるのは、その判定に core が二度目の照合を挟まなくなること

# Context

プラグインが有効になったときに追加で設定できるようになる項目は、その定義と検証ロジックをプラグイン自身が持つべきである。実際には一部が core に寄っていた。

**原則は既にあった。** F-59 は「プラグイン固有設定の検証は必須の `config/validate` RPC でプラグインへ委譲する」と決めており、実装も済んでいる（`config validate` が有効なプラグインを起動して聞く、`--offline` で skip = F-63）。

詰まっていたのは **F-64 が「プラグイン固有設定」を*置き場所*で定義していた**ことである:

> Plugin-specific configuration is separated into `$XDG_CONFIG_HOME/totsuka/plugins/{name}.toml`

委譲がファイル境界で止まる。`plugins/{name}.toml` は完全にプラグインのもの（core は無解釈で保持し、シークレット解決だけして `initialize` へ渡す）だが、**core の構造体の中に置きたいプラグイン固有項目には委譲の経路が無い**。`[[workflows]]` は core の概念なので、そこにプラグインがプロパティを生やす口が存在しなかった（`WorkflowConfig` は `deny_unknown_fields`）。

結果、core がプラグイン固有の語彙とロジックを持っていた:

| core にあったもの | 実体 |
|---|---|
| 予約 trigger キー `project_status` | GitHub Projects の概念（[ADR-0062](/decisions/adr-0062-status-vocabulary.md) が `status` として core 所有に戻した） |
| 予約 trigger キー `reaction` | Slack の概念 |
| `reaction` の型検証・重複検出・trigger の重なり警告 | Slack 固有の妥当性判断 |
| `[[workflows]].publish` | 実装しているのは slack だけ |
| `[plugins.{name}].poll_interval_secs` | core は使わず転送するだけ |

## ワークフロー選択が二重に行われていた

この歪みのほとんどは 1 つの原因に帰着する。**「このタスクはどの workflow か」の判定が 2 回行われていた。**

1. **プラグイン側**: `initialize.triggers`（workflow 定義順）を受け取り first-match を再現する
2. **core 側**: `run/ingest.rs` の `match_workflow` → `Trigger::matches` で**同じ判定をやり直す**

core が予約キーを知らなければならないのは、この 2 つ目のためだけだった。派生していた歪み:

- **`reaction:<emoji>` という合成ラベル。** プラグインが既に知っている判定結果を core に再導出させるためだけに `Task.labels` へ詰めていた。詰め忘れると**どの workflow にもマッチせず黙って捨てられる**
- **「1 絵文字 = 1 workflow」の検証が 2 実装**（core と slack）
- **trigger table が core → plugin の側方通信路に転用されていた。** core が profile から導出した `instructions_kind` / `task_id_prefix` を trigger へ注入して渡していた

### そしてこの「防御的再判定」は防御になっていなかった

`Trigger::matches` が照合していた `task.status` / `task.labels` は、**プラグインが submit した `Task` の中身**である。core が上書きするのは `task.source` だけだった。つまり**検査する側とされる側が同じプラグイン**で、間違ったプラグイン・悪意あるプラグインからは一切守れない。

守れていたのは #396 が塞いだ**config のミス**（読めない `reaction` 値 → trigger が見た目より弱くなり catch-all を追い越す）だけで、それは選択を一元化すれば**構造的に消える**（core が trigger を読まなくなるので「弱くなる」余地が無い）。

# Decision

## 1. 設定は config.toml へ一本化し、`plugins/{name}.toml` を廃止する

プラグイン固有設定はトップレベルの `[<name>]` テーブルへ移す。

```toml
[plugins.slack]          # ロスター（core 所有）
enabled = true
kind = "task_source"

[slack]                  # 設定（プラグイン所有・core は無解釈）
app_token = "op://Dev/Totsuka - local/app_token"
target_user_id = "U08T7QXPTTK"

  [[slack.channel_groups]]
  prefix = "dev-dotfiles-"
  repos = ["dotfiles"]
```

**動機はファイル所有の理屈ではなく運用上の認知負荷である**（「このキーはどっちに書くのか」を消す）。所有境界を宣言で切るという §2 の主張とは独立で、両立するというだけの関係にある。

**テーブルが 2 つ残るのは構造的な要請**である。`[plugins.*]` のロスターは、下の未知テーブル検査が拠り所にするので畳めない。

### 未知のトップレベルテーブルはロスターと照合する

`RootConfig` の `deny_unknown_fields` を外す代わりに、**`[plugins.*]` に居る名前のテーブルだけ**を許す。

```text
error: unknown top-level table `slak` → no plugin named `slak` is declared in [plugins.*]
error: unknown top-level table `worktre` → …
```

タイポ検出は弱まるどころか**強くなる** — core キーのタイポもプラグイン名のタイポも落ちる。以前は前者しか落ちなかった。

### プラグイン名は core の予約トップレベル名を使えない

`version` / `max_concurrency` / `repositories` / `projects` / `plugins` / `default_tool` / `tools` / `workflows` / `llm` / `worktree` / `log` / `hooks` / `prompts` は plugin 名として拒否する。プラグイン名はバイナリ名と同一で改名できない（[ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md) が `name ≠ bin name` を拒否済み）ため、衝突したプラグインは**使えない**。

判定は手書きのリストではなく **1 キーのプローブを serde に食わせて**行う。リストは構造体のフィールド集合の 2 つ目のコピーで、両者を同期させる仕掛けが無い — トップレベルキーを増やしてリストを忘れると、「プラグインが空設定で黙って起動する」という元の危険が戻る。

**将来の core 増設は縛らない。** つまり core がトップレベルキーを 1 つ増やすと、その名のサードパーティプラグインを**後から使えなくする**。名前空間を共有した代償で、意図した上での選択である。

### 移行はしない

`version` は据え置き、`config migrate` も作らず、旧 `plugins/*.toml` の残存検出もしない。リポジトリは未公開で利用者は 1 人、変更と同日に消せば済むため。残っていても読まれないだけで、パースエラーにもならない。

### `poll_interval_secs` も `[<name>]` へ

`[plugins.{name}].poll_interval_secs` は背景の表のとおり core が使わず転送するだけだったので、各ソースの `[<name>]` のキーにし、`InitializeParams.poll_interval_secs` を削除した。値は `initialize.config` の中で届き、`0` を busy-spin に倒さないガードは元からプラグイン側にあってそのまま残る。ロスターの `[plugins.{name}]` に残るのは core がその値で何かを決めるキーだけ（`enabled` / `kind` / `max_concurrency` / `timeout_secs` / `log_level` / `restart`）である。

## 2. core 構造体の中のプラグイン固有キーは「フラット + 引き取り規則」

`[[workflows]]` にはプロパティを**そのまま**足す。名前空間もサブテーブルも挟まない。

```toml
[[workflows]]
name = "slack-books"
source = "slack"
agent = "herdr"
profile = "triage"
publish = "direct"      # プラグインが定義するプロパティ。見た目は core キーと同格
```

`WorkflowConfig` から `deny_unknown_fields` を外し、余ったキーは **`source` と `agent` の両方へ送って「ちょうど 1 つが引き取る」**を規則にする:

| 引き取り手 | 判定 |
|---|---|
| 0 | エラー（タイポ。`profil = "triage"` はここで落ちる） |
| 1 | そのプラグインのもの |
| 2 | エラー（曖昧。どちらの意味か定義しない） |

これで `deny_unknown_fields` を外してもタイポ検出は失われない。プロトコル上は `InitializeResult.claimed_options` が答えで、`ConfigValidateParams` も同じリストを運ぶ。

**答えなかったプラグインが居る workflow は判定しない。** 起動失敗と「何も引き取らない」は wire 上で区別できず、前者を後者と読むと正しい config に対して「未知のキー」が並ぶ。

**検査は `run` と `config validate` の両方に置く。** `run` は後者を呼ばないので、片方だけだと「`config validate` を実行しない運用者には何も検出されない」。

## 3. ワークフロー選択をプラグインへ一元化し、core の予約 trigger 語彙を撤廃する

`task/submit` に `workflow` を載せ、**プラグインが名指しする**。core が確認するのは core だけが知っていること —— その名前の workflow が実在するか、その `source` が submit してきたプラグインかの 2 点だけ。

削除したもの: `Trigger::matches` と予約キー表、`match_workflow`、trigger の重なり警告・catch-all 到達不能検出・`reaction` の型検査。

**Slack ではこの危険が移動ではなく消滅する。** メンションとリアクションはプラグイン内で別のイベント経路なので、リアクションの workflow をメンションの後ろに書いても隠されない（#396 の危険は core が 1 本のリストを first-match していたことに由来していた）。本当に曖昧なもの —— 同じ絵文字を 2 つの workflow が主張する / メンションを 2 つが主張する —— は意味論のある場所、つまりプラグインの `initialize` で拒否する（後者は今回新設）。文字列でない `reaction` 値も同じ場所で拒否する —— 「reaction 無し」と読むと黙ってメンションの行き先になるためで、core の型検査は削除済みなので、この拒否が唯一の門である。

`reaction:<emoji>` ラベルは残すが、義務ではなくなった。core が再導出しなくなったので、元から読めていたとおりの「どう起票されたかの記録」になる。

**trigger への注入も撤廃した。** core が profile から導出する `instructions_kind` / `task_id_prefix` は `WorkflowInfo` の専用フィールドで運び、`trigger` は運用者が書いたテーブルの**素通し**になった。#398 が trigger に焼き込んだのは「プロトコル変更なしで運べる面が trigger しか無かった」ためで、options が wire に載った 0.6.0 でその理由は消えている（不採用案の「`trigger` をそのまま拡張フィールドとして使い続ける」参照）。

**失うもの**: trigger の重なり警告が**どこにも無くなった**。`--offline` の話ではない —— 検査そのものを移していないので、オンラインでも出ない。github / notion で 2 つの workflow が同じステータス列を書いても、報告なしで先勝ちになる。

> **改訂（[ADR-0062](/decisions/adr-0062-status-vocabulary.md)）。** 「core の予約 trigger 語彙を撤廃する」は 1 語だけ戻った。`status`（当時の `project_status`）は **core 所有のキーとして `trigger` テーブルに住む**と定め直してある。用途は #565 の閉路検査の列グラフを組む 1 つだけで、文字列を比較するだけの lexical な読み方に限られる —— タスクの照合には使わないので、この節が撤廃した「core による二度目の照合」は撤廃されたままである。受理するかは各ソースが決めてよい（slack は未知キーとして拒否する）。あわせて同 ADR が `project_status` / `set_status` の綴りを `status` に統一し、`status_map` を廃止した。

移さなかったのは、slack で消滅したのと同じ理由が github / notion には**当たらない**からではなく、当たるからである: これらの trigger はステータス・ラベルのフィルタで、重なりの意味は「両方に一致したら定義順で先勝ち」という**定義済みの挙動**でしかない。core が警告していたのは catch-all を追い越す `reaction` の危険（#396）と同じ枠で見ていたためで、その危険は slack 固有だった。とはいえ「意図しない重なり」を利用者が知りたいことはありうるので、必要になったら各プラグインの `config/validate` へ足す。

## 4. repo→トラッカーの紐付けは `[[projects]]` + `[[repositories]].project`

```toml
[[projects]]
name = "tomo-prj"
source = "github"
owner = "tomoya-k31"        # ← github が読む
owner_type = "user"
project_number = 6

[[projects]]
name = "design-db"
source = "notion"
database_id = "…"           # ← notion が読む

[[repositories]]
name = "totsuka"
path = "~/Workspace/github/tomoya-k31/totsuka"
project = "tomo-prj"
```

要素は `name` と `source` を core が読み、残りは無解釈でプラグインへ渡る。`[[repositories]].project` は**任意** —— トラッカーの無いリポジトリは正常な状態である。

**`source` を書かせるのは推測できないからではない。** キーからは推測できる（`project_number` を理解するのは github だけ）。書かせるのは、**参照連鎖 `[[repositories]].project` → `[[projects]].name` → `[plugins.<source>]` をプラグインを起動せずに辿れる**ようにするためで、壊れた参照は `config validate --offline` でも、ファイルを読む人間にも見える。

### ADR-0056 §4 の却下理由はこの形には当たらない

| ADR-0056 の却下理由 | この案での成否 |
|---|---|
| 「同じ情報を core にも書くと**二重管理**」 | **当たらない。** 逆引きリスト `repos = [...]` が消え、対応は `[[repositories]].project` の 1 箇所だけになる。複製ではなく**移動** |
| 「core がプラグイン固有の概念を知ることになる」 | **当たらない。** core が知るのは*参照*だけ。`owner` / `project_number` / `database_id` は要素の中に無解釈で残る |

得られたもの:

- **`ClaimConflict` が構造的に起こりえなくなり、機構ごと削除した。** 1 リポジトリが 1 project を、1 project が 1 source を指すので、2 プラグインが同じリポジトリを主張する状態が**書けない**。ADR-0056 は「起きたら報告する」を選んだが、この案は起きない形である
- **逆引きリストが増えない。** github / notion / jira と増えても `repos = [...]` が 3 本に分かれない
- **`[[projects]].repos` の二重の役割が解けた。** 取り込みフィルタ**兼** repo→ボードのマッピングで、github プラグインの config が自ら「2 つの役割は分離できない」と書いていた。core が正本を持てば、フィルタはそこから導出される
- **`[[projects]].repos` と `[[repositories]].name` を一致させる運用上の前提が消えた**

**workflow の options と違い引き取り規則は要らない。** project 要素は `source` でちょうど 1 つのプラグインを名指すので所有が曖昧にならず、プラグイン自身の `deny_unknown_fields` がタイポを弾く。

`ClaimedRepo`（repo → 宛先の散文）は**そのまま残す**。散文はエージェントのプロンプト向けで core が組み立てられないため。core が `RepoInfo.project` を渡し、プラグインは自分が引き取った要素に紐づくリポジトリについて散文を返す。

## 5. `publish` をプラグイン所有へ移す

`[[workflows]].publish` を core から外し、Slack プラグインが引き取る最初の実例にする。`ResultPublishParams.delivery` と `PublishDelivery`（0.5.2 で追加）は削除する。

0.5.2 でこれらを wire に置いたのは、**プラグイン所有のキーがプラグインへ届く経路が無く** core が読んで翻訳するしかなかったためである。0.6.0 がその経路であり、2 世代でこの wire フィールドが消えるのはその帰結にすぎない。

**読めない値の扱いが良くなった。** 以前は `result/publish` の呼び出しごとに届いたので「draft に倒して黙る」しか答えが無かった。`initialize` で読むようになったため**拒否できる** —— `publish = "diretc"` と書いて承認ゲートを外したつもりの運用者が、起動時に気づく。

ポリシー（どの workflow が承認を飛ばしてよいか）は移動していない。運用者の選択のまま、`config.toml` に書くまま。変わったのは誰が読むかだけである。

# Alternatives considered

| 案 | なぜ不採用か |
|---|---|
| **`plugin.toml` へスキーマを静的宣言する** | 全 5 プラグインが既に `deny_unknown_fields` を持つ。宣言は serde 構造体と**一致を検査する仕組みの無い** 2 つ目の真実になる。設計初版はこれを推していたが、根拠が誤りだった —— 「雛形生成に要る」と書いたが、`totsuka setup` の雛形は CLI 内のハードコード済みレシピ製で、宣言に依存していない |
| **`[workflows.options.<plugin>]` の名前空間** | 引き取り規則が同名キー衝突を*検出*するので、記法で予防する必要が無い。設計初版の記法。撤回した |
| **新メソッド `config/schema` を足す** | `config/validate`（F-59）が既にあり、同じ目的の経路が 2 本になる |
| **`trigger` をそのまま拡張フィールドとして使い続ける** | 既に側方通信路として過負荷（`instructions_kind` / `task_id_prefix` の注入）。フィルタ条件と設定が同じ袋に入り、どちらの理由でキーがあるのか読めない |
| **`[plugins.<name>.options]` / `[plugin.<name>]` などのネスト** | 入口を 1 つにするのが目的なのに、ネストで読みにくさを自分で足す。`[plugin.slack]` は `[plugins.slack]` と 1 文字違いで並ぶので特に危険 |
| **予約名だけ守り、他の未知トップレベルテーブルは素通し** | core キーのタイポもプラグイン名のタイポも黙って通る。今より弱い |
| **互換を残して両方読む / deprecation 期間を置く** | 「どっちに書くのか」を消すのが目的なのに、両方読めるとそれが残る。両方に同じキーがあるときの優先順位を新しく発明することにもなる |
| **`[[repositories]]` に `tracker = { source, project_number }` を持つ** | ADR-0056 が却下したとおり core がトラッカーの形を知ることになる。`project = "<name>"` の参照ならその問題は起きない |

# Consequences

- **protocol 0.6.0（破壊的）。** `initialize.triggers` → `workflows` の改名、`WorkflowInfo.options` / `.instructions_kind` / `.task_id_prefix` / `ProjectInfo` / `RepoInfo.project` / `InitializeResult.claimed_options` / `TaskSubmitParams.workflow` の追加、`ResultPublishParams.delivery` と `InitializeParams.poll_interval_secs` の削除。同梱 manifest は `>=0.6.0, <0.7` へ。**`triggers` を読む 0.5 系プラグインは「トリガーゼロ」と読んで黙って何も監視しなくなる**ので、F-54 のゲートで起動拒否に倒す
- **trigger の重なり警告は完全に無くなった**（`--offline` に限らない）。上の決定 3 を参照
- **`config validate --offline` は弱くなる。** `[[workflows]]` のプラグイン定義キーの検証が出なくなる。`run` は起動時に必ず検査するので、素通りするのは「`config validate --offline` だけを実行して `run` しない」場合に限られる
- **プラグインが workflow 名を詐称できる。** read-only な source が `profile = "implement"` の workflow を名乗れる。ただし**以前も同じ**（`status` / `labels` をプラグインが作っていた）ので後退ではない。core は `source` の一致だけは必ず検証する
- **`totsuka setup` の爆風範囲が広がる。** 書き込み単位がファイルからテーブルへ変わり、編集をしくじると `config.toml` 全体を損なう。以前はプラグイン 1 ファイルで済んだ
- **旧 `plugins/*.toml` は黙って無視される。** `version` を上げず検出もしないと決めたため。プラグインは空設定で起動し、症状は原因から遠い場所に出る。利用者が 1 人で同日に消すため許容した
- **`tracker` という語は core から消した**（同一概念に 2 語を並存させない）。`doctor` のチェック id は `trackers` → `projects`、`[prompts].tracker_destination` は `project_destination` へ改名（上書きしている config はキー名を追随する）。ADR-0056 のタイトルなど過去の記録はそのまま
- F-64 は撤廃、F-01 / F-56 / F-59 / F-81 は記述を更新した
