---
type: Decision
title: ADR-0033 ワークフローの原型は [[workflows]].profile の 4 値で束ねる
description: "2 値の WorkflowMode では「worktree は read-only だが外部へは書く」を表現できないため、answer / triage / design / implement の 4 原型を Rust 固定で定義し、mode・output・verification をまとめて解決する決定。profile と mode / verification の併用は CONFIG_INVALID、output だけは上書き可。resolved アクセサで解決を Workflow::from_config に一元化し、state.db と plugin-protocol は変更しない。"
resource: https://github.com/tomoya-k31/totsuka/issues/394
tags: [decision, config, workflow, profile, permissions, adr]
generated: { by: claude-code/opus-5, at: 2026-08-09T22:15:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-393
    resource: https://github.com/tomoya-k31/totsuka/issues/393
    title: "#393 workflow profile 導入と成果物責務の再配置（エピック。D1〜D9 の設計記録）"
  - id: issue-394
    resource: https://github.com/tomoya-k31/totsuka/issues/394
    title: "#394 [[workflows]].profile を新設し answer/triage/design/implement の 4 原型を導入する"
  - id: claude-permissions
    resource: https://code.claude.com/docs/en/permissions
    title: "Claude Code — Permissions（deny のスコープ横断マージ、CLAUDE.md は enforcement ではない旨の明言）"
---

# Status

stable。[#394](https://github.com/tomoya-k31/totsuka/issues/394) の実装とともに確定した。
エピック [#393](https://github.com/tomoya-k31/totsuka/issues/393) の D5 にあたり、その土台として最初に入る。

本 ADR が決めるのは**スキーマと解決ロジックだけ**である。profile が将来束ねる残りの要素は別 issue に分かれている:

| 要素 | issue | 状態 |
|---|---|---|
| profile ごとの `permissions.deny` セット | [#395](https://github.com/tomoya-k31/totsuka/issues/395) | **済**（D4 節を参照） |
| 成果物の書き手の分割と URL 実在検収 | [#398](https://github.com/tomoya-k31/totsuka/issues/398) | **済**（D2/D3 節を参照） |
| profile が要求する外部ツールの認証検査 | [#399](https://github.com/tomoya-k31/totsuka/issues/399) | **済**（D9 節を参照。ただし検査範囲は当初設計より狭い） |

#395 が入るまでの profile は「mode / output / verification の別名」でしかなかった。いまは claude タスクに限り**一定の権限効果がある**（下の D4 節）。ただし**それは read-only の保証ではない** — 実機検収は [#410](https://github.com/tomoya-k31/totsuka/issues/410) で実施され、**否定的な結果になった**（D4 節の「実機で否定された部分」）。この ADR に `verified` は付けない。

# Context

## 2 値の mode で表現できない形がある

`[[workflows]].mode` は [F-82](/product/orchestrator-spec.md) 由来の 2 値（`plan` / `implement`）で、意味は「worktree に書くか否か」だった。[#393](https://github.com/tomoya-k31/totsuka/issues/393) が見据える 8 本のワークフローを並べると、この軸だけでは足りない形が 2 つ出る:

| やりたいこと | worktree | 外部への書き込み | 既存の mode で書けるか |
|---|---|---|---|
| Slack の質問に答える | 読むだけ | プラグインが承認後に投稿 | `plan` で書ける |
| Slack の依頼を GitHub / Notion に起票する | 読むだけ | **エージェントが `gh` / Notion MCP で書く** | **書けない** |
| status 変化を受けて詳細設計を issue コメントへ | 読むだけ | **エージェントが書く** | **書けない** |
| 実装して PR を出す | 書く | エージェントが書く | `implement` で書ける |

中央 2 行は「worktree は read-only、しかし外部へは書く」であり、`plan` を選ぶと外部書き込みまで止めたくなるし、`implement` を選ぶと worktree が開いてしまう。`mode` に 3 値目・4 値目を足す案もあるが、それでは deny セット・検収ルーブリック・必要な外部ツールを束ねる受け皿が無く、組み合わせを人間が手で合わせ続けることになる。

## 組み合わせを手で合わせる構造が事故の発生源だった

これは仮説ではない。`mode` / `output` / `verification` / `tool` は互いに独立した必須キーとして並んでおり、噛み合わない組み合わせが実際に出荷されている:

- `verification = "llm"` を非 claude 系ツールに設定すると、prompt 型 Stop フックが無いので**検収されないまま publish されていた**（[#301](https://github.com/tomoya-k31/totsuka/issues/301)）。`ToolCapabilities::prompt_verification` は制約を宣言していたが誰も読んでいなかった
- `mode = "plan"` は git を構造的に止めておらず、対象リポジトリの `CLAUDE.md` に誘導されて plan タスクがブランチ・push・PR まで到達した（[#378](https://github.com/tomoya-k31/totsuka/issues/378)、検出のみ入ったのが [#385](https://github.com/tomoya-k31/totsuka/issues/385)）

どちらも「個々のキーは正しく、束ね方が間違っていた」形をしている。束ね方を設定に委ねる限り同じ形は再発する。

## 束ねる場所は Rust であってキーではない

profile が最終的に決めるものには `permissions.deny` セットが含まれる（#395）。deny は権限境界なので、**設定文字列から到達できる場所に置かない**。[ADR-0023](/decisions/adr-0023-configurable-prompt-surface.md) が opencode の deny マップについて出した結論と同じで、理由も同じ（文字列経由の権限昇格を防ぐ）。したがって profile の解決テーブルは Rust 固定にし、設定側には「どの原型か」を選ぶ enum 1 個だけを見せる。

# Decision

## 1. `[[workflows]].profile` を新設し、4 原型を Rust で定義する

| profile | mode | output | verification | 想定 WF（#393） |
|---|---|---|---|---|
| `answer` | `plan` | `source` | `llm` | Slack の質問への回答（WF 1, 2） |
| `triage` | `plan` | `source` | `llm` | Slack から GitHub / Notion への起票（WF 3） |
| `design` | `plan` | `none` | `llm` | 詳細設計を issue / ページへ（WF 4, 6） |
| `implement` | `implement` | `none` | `llm` | 実装と PR（WF 5, 7） |

`answer` と `triage` はこの表の上では同一である。分けているのは、**#395 と #398 が両者に別の deny セットと別の検収ルーブリックを与える**からで、それが入るまでは意図的に区別だけが先に存在する。

`WorkflowMode` / `OutputPolicy` / `VerificationMode` の enum 自体は変更しない。profile はそれらの**組み合わせに名前を付けたもの**であって、新しい実行モードではない。

## 2. `mode` / `output` / `verification` を `Option` にし、解決を 1 箇所へ集める

`WorkflowConfig` の 3 キーは `Option` になり、読み出しは `resolved_mode()` / `resolved_output()` / `resolved_verification()` を通す。生のフィールドを読んでよいのは**検証だけ**で、そこだけが「省略された」と「明示された」を区別する必要がある。

解決が起きる唯一の点は `Workflow::from_config` である。ドメイン `Workflow` は解決済みの具体値だけを持つので、`run` も `hooks` も profile の存在を知らない。この形にした理由は、profile が増えたときに触る場所を 1 つに保つためと、**解決を忘れた読み出し箇所が生まれないようにする**ためである。

例外は `hooks::render_settings` で、ここは設定の `WorkflowConfig` を直接受け取る。ここが生の `verification` を読むと profile 付きワークフローで `None` を見て prompt 型フックを出さず、**「実行は成功するが一度も検収されない」形で静かに壊れる**。resolved アクセサ経由に変え、4 profile すべてでフックが出ることをテストで固定した。

## 3. 併用の可否 — `output` だけが上書き可

| 構成 | 結果 |
|---|---|
| `profile` + `mode` | `CONFIG_INVALID`（`ProfileConflict`） |
| `profile` + `verification` | `CONFIG_INVALID`（`ProfileConflict`） |
| `profile` + `output` | **許可**。`output` は profile の値に勝つ |
| `profile` 無し + `mode` または `output` 欠落 | `CONFIG_INVALID`（`WorkflowMissingKey`） |
| `profile` + `rubric` / `prompts` / `tool` / `timeout_secs` / `on_success` / `on_failure` | 許可（プロンプトは data = ADR-0023、tool は直交軸 = [ADR-0014](/decisions/adr-0014-tool-abstraction.md)） |
| 未知の profile 名 | serde の enum 拒否でパースエラー |

`mode` / `verification` の併用を「どちらかを勝たせて受理」にしなかったのは、負けた側が**生きて見える死んだ設定**として残るからである。`output = "pull_request"` を「受理して `source` として扱う」ではなく削除した判断（[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md)）と同じ理屈で、黙って無視された設定は誰も気づかない。

`output` を例外にしたのは、それが**権限ではなく配線先の選択**だからである。Slack 起点の implement（#393 D6 / [#397](https://github.com/tomoya-k31/totsuka/issues/397)）は PR URL をスレッドへ返す必要があり、`profile = "implement"` + `output = "source"` でしか書けない。`output` をどちらに倒してもエージェントにできることは変わらないので、上書きを許しても安全性は下がらない。

## 4. 永続化とプロトコルは変更しない

- `tasks.mode` 列は `"plan"` / `"implement"` のまま。profile → 実行モードの写像は既存の `mode_str()` が行う。**state.db マイグレーションは不要**（[ADR-0017](/decisions/adr-0017-state-db-compatibility-policy.md) の適用外）
- `TaskDispatchParams.mode`（`ExecutionMode`）不変。**plugin-protocol のバージョンも据え置き**で、旧プラグインと完全互換
- profile は `record.workflow` から config を逆引きすれば常に復元できるので、DB には刻まない

`tasks.mode` が変わらないことには実務上の意味がある。worktree のクリーンアップは `record.mode == "plan"` で `cleanup_plan` / `cleanup_implement` を選び分けており、`answer` / `triage` / `design` はすべて `plan` に写る。ここがずれると answer タスクの worktree が implement 側の保持期間で残り続け、**ディスクが埋まるまで誰も気づかない**。写像をテストで固定してある。

## 5. `setup` のレシピは profile 記法へ寄せる

生成される `config.toml` が新記法の実例になる。ただし「Human sign-off required」レシピだけは明示記法のまま残した — `verification = "human"` を要求し、4 原型はいずれも `llm` に解決するので profile では書けない。これは劣化ではなく、**明示記法が残る理由そのもの**である。

**「Slack — reply as yourself」は記法だけでなく挙動も変わる。** このレシピは `mode = "implement"` を書いていたが、`profile = "answer"` は `plan` に解決する。表記の言い換えではなく意図した変更で、根拠は 2 つ:

- このレシピは #393 の WF 1 そのものであり、**メンションに答えることは実装ではない**
- Slack の質問が実装を要することが分かった場合の経路は、同じタスクの権限が広がることではなく、本人のリアクションで別の `impl:` タスクを起こすこと（#393 D6 / [#397](https://github.com/tomoya-k31/totsuka/issues/397)）

既存 config への影響は無い（`setup` は既存 `config.toml` を上書きしない）。影響を受けるのは、これから同レシピを選ぶ利用者が書き込み可能な worktree を得なくなる点である。

`upsert_workflow` は `profile` / `mode` / `output` を「値が無ければキーごと消す」書き方にした。明示記法から profile へ書き換えた既存エントリに `mode` が残ると、ウィザード自身が `ProfileConflict` を書き込むことになる。

## 6. D4 — plan 系 profile は Rust 固定の `permissions.deny` を持つ（#395）

`hooks::render_settings` が、`answer` / `triage` / `design` の workflow に対して `--settings` JSON へ `permissions.deny` を書く。リストは `hooks::permissions` の Rust 定数で、**設定キーからは合成できない**（理由は上の「束ねる場所は Rust であってキーではない」と同じ）。

効く根拠は Claude Code の permission モデルそのものにある:

- **deny はスコープ横断でマージされ、どこかで deny されたツールは他のどのスコープの allow でも許可できない。** よって `--settings` の deny は対象リポジトリの `.claude/settings.json` の allow に必ず勝つ
- 公式ドキュメントは「Permission rules are enforced by Claude Code, not by the model. Instructions in your prompt or CLAUDE.md … don't change what Claude Code allows.」と明言している。**ただしこれは「deny したツール／コマンド文字列は CLAUDE.md では覆せない」という意味であって、「CLAUDE.md の指示どおりの結果にならない」という意味ではない。** 本 ADR の初版はこれを [#378](https://github.com/tomoya-k31/totsuka/issues/378) への答えだと書いたが、[#410](https://github.com/tomoya-k31/totsuka/issues/410) の実機検証で否定された（下の「実機で否定された部分」を参照）
- deny は全 permission mode で有効なので、`--permission-mode plan` と併用できる

| profile | 拒否するもの |
|---|---|
| `answer` | ファイル編集 + **`Bash` そのもの**（[ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) で `Bash(...)` パターン列挙から置き換えた。あわせて claude の `--permission-mode plan` も渡さなくなった） |
| `triage` / `design` | ファイル編集 + git 書き込み + PR + `gh repo delete` / `rename` + `gh api`。**`gh issue …` は開けたまま** — そこに成果物を書くのがこの profile の仕事だから |
| `implement` | 何も拒否しない（`permissions` キー自体を書かない） |

**下の表と節は初版（#395 時点）の記述である。** `answer` については [ADR-0035](/decisions/adr-0035-answer-profile-shell-removal.md) が置き換えた。`triage` / `design` については今も現状のままで、[#409](https://github.com/tomoya-k31/totsuka/issues/409) 待ちである。

**`gh api` は read-only profile すべてで塞ぐ。** REST も GraphQL も叩けるので、開けたまま `gh repo delete` や `gh pr create` を denyしても意味がない — `gh api -X DELETE repos/{owner}/{repo}` と `gh api -X POST .../pulls` で同じ場所に届く。**実際より強く読めるリストは、短いリストより悪い。** 代償は本物で、パターンは `GET` と `POST` を区別できないので読み取り用の API 呼び出しも一緒に塞がる（`gh issue view` / `gh pr view` / `gh search` で足りる範囲に収まる想定）。**GraphQL が要る workflow が出てきたら** — Projects v2 のフィールドや draft issue には `gh` サブコマンドが無い — それはこのルールを意識的に見直す合図であって、黙って穴を開けたままにする理由ではない。

### 保証の強さは層で違う

| 層 | 機構 | 実際に止まるもの |
|---|---|---|
| 1 | `--permission-mode plan` | **何も止めない**。`permissionMode` が `plan` のまま `cat >` が成功した実測がある（#410）。残るのは `ExitPlanMode` の承認ゲートだけで、無人 pane では非決定的に振る舞う |
| 2 | 裸のツール名（`Edit` / `Write` / `NotebookEdit`） | **その名前のツール**。ファイル書き込みではない（下記） |
| 3 | `Bash(...)` パターン | その文字列で**始まる**コマンドだけ |
| 4 | ブランチ検出警告（#385） | 何も止めない。事後に警告するだけ |

### 実機で否定された部分（#410）

**この節の初版は「層 2 はファイル編集の実質保証」「`CLAUDE.md` が push / PR を指示していても効かない」と書いていた。[#410](https://github.com/tomoya-k31/totsuka/issues/410) の実機検証がどちらも否定した。**

ルールは正しく生成され、正しく適用され、実際に発火した（`Write` は消え、`git switch -c` は拒否された）。そのうえで `answer` profile のタスクがブランチを切り、commit し、push し、PR を作った。穴は 2 つ:

1. **編集ツールを消しても worktree は read-only にならない。`Bash` が残っているため。** 消えた `Write` が触るはずだったファイルに `cat > file` / `python3 - <<EOF` / `tee` で書ける。`DENY_FILE_EDITS` に対応する `Bash` 規則は存在せず、**列挙で塞ぎ切れる集合でもない**
2. **前方一致は `&&` とパイプに負ける。** `git add -A && git commit` は `git add` で始まるので `Bash(git commit *)` に掛からない。`git push … | tail -5` と `gh pr create --fill | tail -5` はルールがある状態で通った

ハード保証には sandbox が要る。**それまでは、これらの規則が profile を read-only にすると書かないこと** — #410 が否定するまで security ポリシーと本 ADR にその一文があった。守れない約束は、約束が無いより悪い。

書式の罠を 2 つ踏まないようにしている: **`Write(path)` / `NotebookEdit(path)` のパス付きルールは受理されて参照されない**（パス限定が効くのは `Edit(path)` / `Read(path)` だけ）ので裸名のみを使う。`Bash(git *)` と `Bash(git*)` は別物（後者は `gitk` にもマッチ）なので、ワイルドカード前のスペースをテストで固定した。

### 明示記法には deny を付けない

`mode = "plan"` と書いただけの workflow は deny を得ない。`mode` は元々何も強制しておらず（#378 がその証拠）、そこから権限境界を推測すると**既存の構成がアップグレードで黙って厳しくなる** — 意図してブランチを切っていた plan タスクが、設定を一切変えていないのに落ちるようになる。強制が欲しければ profile 記法へ移行する、という線にした。

### 走行中セッションへの反映

Claude Code は settings ファイルの変更を実行中セッションに取り込む。つまり totsuka を更新して `install` が走ると、**走行中の plan タスクにも新しい deny が即時適用される**。変化は常に「制限が強まる」方向なので安全側だが、走行中タスクが突然 `Edit` を拒否されうる点はリリースノートに書く。

### 積み残し: Notion MCP の write 系

MCP ツールの deny（`mcp__<server>__<tool>`）は書けるが、**サーバ名がユーザ環境依存**なので Rust 固定にできない。`answer` profile から Notion への書き込みを止める手段は現状ここには無く、instructions 層（#398）に委ねている。

## 7. D2/D3 — 成果物はエージェントが直接書き、URL の実在で検収する（#398）

`design` / `implement` は `output = "none"` なので、コアは `result/publish` を呼ばない。成果物（issue コメント / Notion ページ / PR）は**エージェントが `gh` / Notion MCP で自分で書く**。[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md) が push / PR について出した結論を、issue コメントと Notion ページへ広げたもの。

status の書き戻しだけは**コアに残す**（`task/update_status`）。更新忘れと `in_progress_statuses` との不整合（再タスク化ループ）は、完了を知っている側が機械的にやるのが確実だから。

### 書き込み先はどうやってエージェントに伝わるか

指示は 2 層に分かれる（[ADR-0024](/decisions/adr-0024-agent-instruction-layers.md)）:

- **層 1（ソースプラグインの `Task.instructions`）** — どこに何を書くか。`plugins/task-source-{github,notion}/src/defaults.toml` に置き、`[prompts]` で上書き可能
- **層 2（コアの不可視プロンプト）** — 完了マーカーなど、既存のものだけ。URL 必須は層 1 が言う。**同じことを 2 箇所で言わない** — ズレたときどちらが正か決められなくなる

プラグインは profile を知らない。コアが `TriggerInfo.trigger` に `instructions_kind`（`triage` / `design` / `implement`）を焼き込み、プラグインはそのキーで自分の指示文を選ぶ。トリガはもともと plugin-defined な `Value` なので**プロトコル変更もバージョン bump も不要**で、旧プラグインは未知キーを無視して従来どおり動く。

**代償は縮退が無言なこと。** 新コア + 旧プラグインでは指示が付かず、書き込み先を知らないエージェントが dispatch される。capability 宣言が無いので probe できず、doctor にも検査を置けない — **コアとソースプラグインは同時にリリースする**。

### URL 実在検収

書き込みは Stop フックより前に済むので、**検収は事前ゲートではない**。「URL 検収の失敗 = 公開の取り消し」ではなく「タスクを完了扱いにしない」だけである。誤った書き込みの受け皿は、status を動かした人間の事後レビュー。

これは GitHub Agentic Workflows の "stage and vet all writes" との**意図的な相違**で、成立するのは書き込み先が PR / issue コメント — **それ自体がレビュー面である場所**だからである。

検収は rubric の差し替えで行う。`verification_rubric_artifact_url` が `triage` / `design` / `implement` の rubric leaf の既定になり、「最終メッセージに成果物 URL が実際に含まれているか」「その URL の内容が申告と整合するか」を条件として見る。`answer` は対象外 — 返信はプラグインの承認ゲートを通るので URL が無く、要求すると正常な回答が全部落ちる。

**穴が 1 つあった。** この既定は global `[prompts].verification_rubric` より**弱い**ので、それを設定済みの構成は URL 検収にならなかった。梯子をこの順にしたのは「全 workflow に対して既に選ばれた文言を、後から入った profile が黙って覆す」方が悪いからだが、どちらの順でも黙って壊れることに変わりはなく、**[#465](https://github.com/tomoya-k31/totsuka/issues/465) がグローバルな面そのものを削除して穴を塞いだ**（[ADR-0023 の Amendment](/decisions/adr-0023-configurable-prompt-surface.md)）。現在この既定に勝てるのは同じワークフローの `rubric` だけである。

### 削除は 0.3

`result_publish` の実体は残し、**呼ばれたときだけ**非推奨警告を出す（`initialize` 時ではない — その経路を通らない構成に、対処しようのない警告を出しても雑音になる）。実体と Notion の `blocks.rs` の削除は 0.3。

> **後日談**: ここで「削除は 0.3」と書いたが、**実際に消えたのは 0.5 系**である。0.3.0 も 0.4.0 も breaking bump だったのに、そのとき誰もこの行を参照しなかった。[ADR-0030](/decisions/adr-0030-herdr-pane-layout.md) が `design_preview` でまったく同じ失敗を記録しており、**同じ書き方が同じ結果を生んだ**ことになる。「次の breaking bump で消す」は、bump のたびに未処理の削除予定を洗い出す仕掛けが無い限り実行されない。
>
> また `blocks.rs` の削除という書き方は広すぎた。消えたのは**書き込み方向だけ**（`markdown_to_blocks` とその補助）で、`blocks_to_markdown` / `rich_text_plain` はページ本文をタスク本体に載せる経路が使い続けている。
>
> 削除に伴い、`answer` / `triage` profile を github / notion で使うには `output = "none"` の明示が要るようになった（両 profile は `output` を `source` に解決するが、両プラグインは capability を宣言しなくなったため）。

## 8. D6/D7 — 実装への移行はリアクションで別タスクを起こす（#397）

Slack の「質問 → 方針決定 → 実装」（#393 の WF 8）を、**実行中タスクの権限昇格ではなく**、本人のリアクションを起点とする別タスクとして実現する。`answer` タスクは deny を一度も緩めずに完結し、書き込み権限の付与は常に本人の明示的な操作を経由する。

- **ID**: `impl:{channel}:{反応したメッセージの ts}`。スレッドには既に `answer` タスク（`{channel}:{thread_ts}`、[ADR-0015](/decisions/adr-0015-conversation-task-identity.md)）があるので、接頭辞が `UNIQUE(source, source_task_id)` の衝突を避ける。接頭辞は profile から導出し、コアが `TriggerInfo.trigger` の `task_id_prefix` に焼き込む（プラグインは profile 概念を知らない、#398 の `instructions_kind` と同じ手法）
- **スコープ**: 親（スレッド root、またはスレッド外の単独メッセージ）へのリアクション → **スレッド全会話**が文脈。子（返信の 1 つ）へのリアクション → **そのメッセージのみ**。特定のメッセージに反応する行為自体が「ここだけ」という指定なので、スレッドを引き込むと指されていない会話を渡すことになる
- **リポジトリ継承**: `task/lookup` を**接頭辞なしの会話 ID** で引く。接頭辞付きの自分の ID で引くと必ず「新規」が返り、answer タスクが解決済みのリポジトリを捨てて LLM 呼び出しかピッカーをやり直すことになる
- **返信**: PR の URL をスレッドへ返して初めて完結するので `profile = "implement"` + `output = "source"`（D3 の output 上書き例外がここで要る）。**承認ゲートは不変** — 誤った実装報告こそ無レビューで出てはいけない

### dedup キーは「結果のタスク ID」

#397 は `{channel}:{ts}:{emoji}` を指定していたが、実装では**そのリアクションが生むタスク ID**をキーにした。

絵文字をキーに入れると、狙っていた問題（`:eyes:` で回答済みのメッセージへの `:hammer:` が再配送として潰される。しかも LRU は再起動でしか消えないので恒久的に）は解ける。しかし**同じタスクを生む 2 つのリアクションまで別扱いになり**、[#319](https://github.com/tomoya-k31/totsuka/issues/319) が確立した「1 メッセージがメンションとリアクションの両方で届いても 1 タスク」という不変条件が壊れる（実装中に既存テストが落ちて判明した）。

タスク ID をキーにすると両方成立する — 別タスクは別、同じタスクは 1 つ。絵文字が効くのは「別の接頭辞を持つ workflow を選ぶとき」、つまりまさにタスクが別になるときだけである。

### 積み残し

- `conversations.replies` は 200 件でクランプしている。親リアクションは全会話を文脈にしたいが、それを超えるとページングの実装が要る（未実装、config-reference に明記）
- answer タスクが Running のうちに `:hammer:` が付くと並走する。別タスク・別 worktree なので衝突はしないが「方針決定前に実装が始まる」。初版は許容 — ブロックすると `task/lookup` の往復が load-bearing になり、FR-31 の禁止事項に触れる

## 9. D9 — 必要な外部ツールの不在は dispatch 前に検知して待機させる（#399）

#398 でエージェントが自分で成果物を書くようになったので、タスクとは無関係な理由（必要なツールが未認証）で失敗しうるようになった。壊れ方が悪い — エージェントが起動し、`gh` が GitHub に届かないと分かり、誰かが見るまで pane で座礁する。

`dispatch_ready` で弾く（`dispatch_one` ではなく — スロットも worktree も取らないため）。

### fail ではなく skip / warn

**タスクは `Queued` のまま**で、状態遷移は増やさない（D9 の当初案は `Pending` だったが、`Pending` は F-14「リポジトリ選択待ち」の意味で state machine に組み込み済みで、意味が濁る）。検査が通れば次の cycle で自然に流れ出す。

検査は orchestrator のプロセスで走り、エージェントはユーザのシェルプロファイルが効いた pane で走る。**pane からしか見えない `gh` は「無い」と読まれる** — 文書化済みの偽陰性である。だから:

- dispatch では **skip**。偽陰性は仕事の消失ではなく遅延で済む
- doctor では **warn**（当初設計の `fail` から変更）。`fail` は exit code を動かすので、全部動いているマシンで「壊れている」と報告することになる。**間違いうる検査に「止まれ」と言わせない**

これは机上の懸念ではない。severity を `fail` にした版で `setup` の E2E が落ちた — scratch な `XDG_CONFIG_HOME` には `gh/hosts.yml` が無いので、`gh` を設定済みの環境でも fail する。

### 検査しない範囲の方が大きい

**`implement` の `gh` だけ**を見る。PR を開くのは source によらず `gh` なので、ここで判断できる。

`triage` / `design` も外部へ書くが、**どこへ**書くかは source 依存（GitHub なら `gh issue comment`、Notion なら MCP）で、Orchestrator は区別できない — `[[workflows]].source` はユーザが決めるインスタンス名（`github` のこともあれば `gh-work` のこともある）で、そこから推測すると**動いたはずのタスクをブロックする**。**推測するゲートはゲートが無いより悪い。** doctor はその旨を `skip` 行で明示する（黙って通すと「検査済み」と読まれるため）。

Notion MCP はもう 1 つ別の理由でも届かない: エージェント側の設定であり、しかも場所は workflow が解決するツールに依存し、`notion` プラグイン自身のトークンとは別系統である。

### ネットワーク probe は dispatch パスに持ち込まない

判定はローカルのみ（バイナリの存在 + `gh` の認証ファイルの存在）で、`gh auth status` は実行しない。dispatch は毎 cycle 走るので、そこにネットワークのレイテンシと flakiness を足すことになる。代償は**「見た目は正しいが期限切れ」の資格情報が通る**ことで、これは受け入れる — この検査が捕まえるのは「そもそも設定していない」という一番多いケースで、期限切れは従来どおり pane で見える形で失敗する。

### 通知は既存 variant の再利用

`NotifierEvent::Pending` を使う。新 variant は現行プロトコルでビルドされた notifier プラグインで deserialize に失敗し、配送は fire-and-forget（F-93）なので**通知が黙って止まる**のが症状になる。専用 variant は protocol 0.3 の `#[serde(other)]` フォールバックとセットで入れる。

# Consequences

## 良くなること

- 権限に関わる組み合わせを人間が合わせなくてよくなる。profile を選べば mode / output / verification と deny セットが一括で決まる（**deny が read-only を保証するという当初の記述は #410 で撤回した** — D4 節「実機で否定された部分」）
- 「worktree は read-only だが外部へは書く」が初めて表現可能になり、#393 の WF 3 / 4 / 6 が書けるようになる
- 新しい原型が要るときに足す場所が 1 つ（`Profile` の enum と解決テーブル）に決まる

## 引き受けたコスト

- **設定の書き方が 2 通りになった。** profile 記法と明示記法が併存する。統一しなかったのは「Human sign-off required」のように 4 原型で表せない組み合わせが実在するためで、明示記法は非推奨ではない
- **ロールバックが非対称。** profile を使った config は旧バイナリでは `deny_unknown_fields` によりパースエラーになる。新 → 旧に戻すときは config も戻す必要がある（リリースノートに明記する）
- **実効性は claude タスクに限られる。** deny は `--settings` 経由なので、codex（`--sandbox read-only` で別途 OS レベルに制限）と opencode（agent ファイルの deny マップ）はこの経路を読まない。3 つの機構が同じ意図を別々に実装している状態で、[ADR-0014](/decisions/adr-0014-tool-abstraction.md) の縮退表どおりではあるが、集約されてはいない
- **deny は read-only の保証ではない — 層 2 も含めて。** 上の D4 節「実機で否定された部分」のとおり、[#410](https://github.com/tomoya-k31/totsuka/issues/410) の実機検証で `answer` タスクがブランチ・commit・push・PR まで到達した。層 3 が弱いことは初版から書いていたが、**層 2 を「ファイル編集の実質保証」と書いたのが誤り**だった — 編集ツールを消しても `Bash` が残っていれば同じファイルに書ける
- **「deny に書いてあるから安全」と読まれるのが一番危ない、と初版に書きながら、その一番危ない文章を自分で書いていた。** 保証を書く前に実機で測る、が守れていなかった

## 非破壊であること

既存の config は無変更で通る。`mode` / `output` が必須から `Option` になったのは緩和方向で、`profile` は新規キーなので「両方欠落」も「併用」も既存 config には存在しえない。

# 不採用案

| 案 | 不採用理由 |
|---|---|
| `mode` を 4 値化して profile 概念を持たない | deny セット・検収ルーブリック・必要な外部ツールを束ねる受け皿が無く、組み合わせミスの余地が残る。`mode` の意味（worktree に書くか）も曖昧になる |
| deny セットを設定キーにする | 権限境界を文字列経由で到達可能にすることになる。ADR-0023 で opencode の deny マップについて出した結論と同じ |
| `profile` + `mode` を「profile が勝つ」で受理 | 負けた `mode` が生きて見える死んだ設定として残る。`output = "pull_request"` を削除した判断と同じ |
| `profile` + `mode` を「mode が勝つ」で受理 | 同上に加え、権限を決める側が後出しの上書きで変わるのは #395 以降に危険 |
| `output` も併用不可にする | Slack 起点の implement が PR URL をスレッドへ返せなくなる（#397）。`output` は権限に触れないので、禁じても安全性は上がらない |
| profile を `tasks` テーブルに永続化する | `record.workflow` から逆引きできるので情報が増えない。state.db マイグレーションのコストだけが残る |
| profile ごとに `WorkflowMode` を増やす | protocol の `ExecutionMode` まで波及し、旧プラグインとの互換が壊れる。写像で足りる |
| 解決を各読み出し箇所で行う（アクセサを作らない） | `render_settings` のような「設定を直接受け取る」箇所が解決を忘れると、検収が黙って消える。実際にその形の罠が 1 箇所あった |
