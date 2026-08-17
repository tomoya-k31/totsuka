---
type: Decision
title: ADR-0023 AI ツールへ差し込むプロンプトは設定可能にし、実行を決める面は設定不可のまま残す
description: claude/codex/opencode へ注入するプロンプト文をコードから外出しし config.toml から上書き可能にする一方、スクリプト・argv・permission ブロック・ステータスマーカーは設定不可のまま残す決定。上書きはインライン文字列のみで、ファイルパス指定と TOTSUKA_PROMPTS_* env は不採用。2026-08-17 に amend し、上書き面（[prompts] と [[workflows]].prompts の 15 キー）は撤回して組み込み専用へ戻した。残る上書きは [[workflows]].rubric 1 キーのみ。
resource: https://github.com/tomoya-k31/totsuka/issues/311
tags: [decision, prompt, config, security, marker, adr]
generated: { by: human:tomoya-k31, at: 2026-08-17T07:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

Accepted（2026-07-30、エピック [#311](https://github.com/tomoya-k31/totsuka/issues/311)）、**一部 amended（2026-08-17、[#465](https://github.com/tomoya-k31/totsuka/issues/465)）**。

**決定 1 の後半「設定で上書き可能にする」を撤回した。** core の `[prompts]` / `[[workflows]].prompts` は削除され、プロンプト文は組み込み専用に戻った。残る上書き面は `[[workflows]].rubric` 1 キーのみである。**決定 1 の前半（`defaults.toml` への外出し）と、決定 2〜5 は生きている** — 撤回したのは設定面だけで、「何を伝えるか / 何が動くか」の一線も、インライン文字列のみという方針も、マーカーを失う出力を検査するという方針も変わらない。詳細と根拠は下の Amendment を参照。

以下の Decision / Consequences は **2026-07-30 時点の決定として読むこと**。設定面に言及している箇所は Amendment が上書きする。

[ADR-0020](/decisions/adr-0020-status-marker-stays.md)（マーカー存置）を **supersede しない** — 本 ADR はその決定を前提に、マーカーを「教える散文」だけを設定可能にする。
[ADR-0004](/decisions/adr-0004-hook-completion-signal.md)（llm 検収はセッション内 prompt 型 Stop フック）、
[ADR-0014](/decisions/adr-0014-tool-abstraction.md)（`[tools]` レジストリ = ツール知識は core の設定に置く先例）、
[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)（`TOTSUKA_*` は明示ホワイトリスト）と関連する。

# Context

claude / codex / opencode に差し込むプロンプト文が Rust の文字列リテラルとしてソースに散在しており、**文言を調整するたびにコード変更とリビルドが必要**だった。対象は 6 箇所:

| 場所 | 内容 |
|---|---|
| `run/hooks.rs` `MARKER_SELF_REPORT_INSTRUCTION` | 全ディスパッチに注入される完了自己申告指示 |
| `hooks/mod.rs` `DEFAULT_RUBRIC` / `BACKGROUND_EXEMPTION` / `marker_convention()` | `verification = "llm"` の prompt 型 Stop フック本文 |
| `hooks/totsuka-plan.md` | opencode plan モードのエージェント markdown |
| `agent-ide-orca` `default_plan_prefix()` | plan モードのプロンプト前置き |
| `task-source-slack` `pipeline.rs` | 返信案指示 + body テンプレート |
| `task-source-slack` `llm.rs` | リポジトリ分類 LLM の system / user / retry プロンプト |

これらは「モデルに何をどう伝えるか」という**運用上のチューニング対象**であり、動作を変えるコードとは性質が違う。運用者が試行錯誤する対象がリリースサイクルに縛られていた。

一方で、同じファイル群には**動作そのものを決める面**が同居している。opencode の plan エージェント markdown は YAML frontmatter に `permission: {edit: deny, bash: deny, task: deny}` を持ち、この deny マップが plan モードの読み取り専用保証そのものである。フックスクリプト（`.sh`）と opencode の JS プラグインは実行されるコードである。

# Decision

## 1. プロンプト文はデータとして外出しし、設定で上書き可能にする

組み込みデフォルトを埋め込み `defaults.toml`（`include_str!` + `LazyLock`）へ移し、`config.toml` の `[prompts]`（グローバル）と `[[workflows]].prompts`（ワークフロー単位）で上書きできるようにする。プラグイン側は自分の `plugins/{name}.toml` を使う（プラグインは core の設定を見られないため）。

## 2. 守る一線: プロンプトは「何を伝えるか」だけを変え、「何が動くか」は変えない

**プロンプトキーはスクリプト・argv・permission ブロックを追加も改変もできない。** 具体的には:

| 面 | 設定可否 | 理由 |
|---|---|---|
| プロンプトの散文 | **可** | 本 ADR の目的 |
| ステータスマーカー（`<<STATUS:COMPLETED>>` 等） | 不可 | `on-stop.sh`（bash）と `totsuka-opencode.js` がリテラルをパースする。ADR-0020 が 3 ツール共通の唯一の完了信号と定めている |
| フックスクリプト 6 本（`.sh`） | 不可 | 実行されるコード |
| opencode JS プラグイン | 不可 | 実行されるコード |
| plan エージェントの YAML frontmatter（`permission` を含む） | 不可 | 散文に見えるキーから `bash: allow` を注入できてしまうと**権限昇格**になる |
| `[tools]` の argv（`command` / `mode_args` / `plan_args`） | 可（ADR-0014 の範囲） | 本 ADR とは別軸の既存決定 |

## 3. 上書きはインライン文字列のみ

`{ file = "~/.config/totsuka/prompts/marker.md" }` のようなパス指定形式は採らない。

## 4. マーカー規約を失う上書きは検証エラーにする

`config validate` / `run` / `doctor` が、**組み立て後**の出力を検査する。組み立て後を見るのは、葉が `{marker_*}` を失った場合と組み立てが節を落とした場合の両方を捕まえるためである。

- `marker_self_report` にマーカーへの言及が 1 つも無ければ **エラー**（起動を止める）
- `verification = "llm"` のワークフローで、組み立て後の `verification_prompt` にマーカーへの言及が無ければ **警告**
- プレースホルダのタイポ（`{marker_completd}` 等）は **エラー**

ただし**タイポが常に捕まるわけではない**（#328）。`{marker-needs-input}` のように名前が識別子でないものは「中身」として扱われ、レンダリング時にそのまま出力される — プロンプトがモデルに `{"ok": true}` のような JSON 出力形を示すのを許すための扱いである。この形のタイポは*その*経路では検出されないので、マーカーについては**3 つすべてが組み立て後の出力に現れること**を直接検査する（1 つでも欠けたら Error）。「どれか 1 つでもあれば良い」では、2 つ残った状態のタイポを見逃す。

## 5. `TOTSUKA_PROMPTS_*` env override は追加しない

# 検討した選択肢

## 上書き値の形式

| 案 | 判断 |
|---|---|
| **インライン文字列のみ** | **採用。** 既存の `rubric` / `pr_body_template` と同じ形で、`config show` / redaction / validate がそのまま効く |
| `{ file = "..." }` のパス指定 | **不採用。** repo 相対パスを持ち込めてしまい、決定 2 の一線を破る。リポジトリに置かれたファイルがプロンプトになるということは、リポジトリへの書き込み権限がプロンプト注入権限になるということである。加えて読み込み失敗・相対パス解決・doctor 検査が増える |

## デフォルトの置き場

| 案 | 判断 |
|---|---|
| **埋め込み `defaults.toml` 1 枚（crate ごと）** | **採用。** ユーザー config と同じ形なので差分が読みやすく、キー一覧がファイル内で完結する |
| `.md` 個別ファイルに分割して `include_str!` | **不採用。** キー一覧が結局 Rust 側の配列に残り、「コードで管理しない」が半端になる |
| `$XDG_DATA_HOME` へ書き出して読む | **不採用。** doctor の改竄検知・自己修復は「ディスク上の内容が期待値と違えば drift」という設計で、ユーザーが編集する前提のファイルとは意味論が衝突する |

## マーカー規約を失う上書きの扱い

| 案 | 判断 |
|---|---|
| **`marker_self_report` はエラー、`verification_prompt` は警告** | **採用。** 完了自己申告の指示なのに完了マーカーに一切触れない、という上書きに正当なユースケースが無い。検収文の再構成には正当なユースケースがある |
| 両方エラー | 不採用。rubric だけにしたいユースケースを潰す |
| 両方警告 | 不採用。警告は起動を止めないので、タイポで完了検知が壊れてもエスカレーション待ちまで気づけない |

## `TOTSUKA_PROMPTS_*` env override

**不採用。** 複数行の散文を env に通すのは footgun で、ADR-0009 の選定基準は「CI が `config.toml` を書き換えずに差し替えたいスカラー」である。プロンプトはこれに該当しない。

## `on-stop.sh` の block reason（マーカー再送指示）

**設定化しない（スコープ外）。** 理由は 4 つ。

1. 中身の実体はマーカー構文そのもので、設定化しても変わるのは周りの文言だけ
2. **セーフティネットは素のままにする。** 前倒し注入はこの block をほぼ発火させないための仕組みであり、主経路と fallback の両方を上書き可能にすると設定ミス 1 つでセッションからマーカーの言及が完全に消える
3. 手書き JSON なので、ユーザー文字列中の `"` や改行で不正 JSON になる。安全にやるには `jq -n --arg` が要るが、`on-stop.sh` には jq 不在時の fail-open 分岐があり、その経路で reason が丸ごと消える
4. `env_overrides::RESERVED` が 1 つ増える（ADR-0009 は意図的に狭く保っている）

代わりに、`on-stop.sh` が 3 つの `MARKER_*` 定数を含むことを assert するドリフト検知テストを置く。

# Consequences

## 良くなること

- プロンプトの文言調整がリビルドなしになり、ワークフロー単位で試せる
- 実機で得た知見（前倒し提示・配送契約・バックグラウンドタスク中の非マーカー）が、**上書きするユーザーが読む場所**である `defaults.toml` のキー直上に置かれる
- `[tools]`（ADR-0014）に続き、ツール固有の知識が core の設定として一元化される

## 受け入れるコスト・リスク

- **上書きミスで完了検知が壊れうる。** 緩和は決定 4 の検証エラーと、決定「`on-stop.sh` は固定」による第 2 のチャンス
- **アセットの意味論が変わる。** `orchestrator-<workflow>.json` と `agents/totsuka-plan.md` は config 由来のレンダリング結果になる。ドリフト検知は嘘にならない（`verify_assets` が毎回 config から再レンダリングした期待値を作って `verify_one` に渡す）が、[フックのセキュリティ](/security/hook-security.md) §3 の「静的埋め込み」の主張は**プロンプト文には当てはまらなくなる**
- **稼働中セッションには届かない。** `[prompts]` を編集すると次の `run` / `doctor` が settings ファイルを書き換えるが、既に起動しているエージェントには反映されない。プロンプト変更は**次のディスパッチから有効**
- 信頼境界は変わらない。`config.toml` はユーザー自身の XDG config 配下にあり、ペインに直接打ち込むのと同じ信頼領域である。決定 2 の一線を守る限り、攻撃面は増えない

# 実装

エピック [#311](https://github.com/tomoya-k31/totsuka/issues/311) の子 issue として段階的に実装する。全段が完了している（本 ADR は #315 と同時に書かれ、以降の PR が状態列を更新してきた）。

| issue | 内容 | 状態 |
|---|---|---|
| [#312](https://github.com/tomoya-k31/totsuka/issues/312) | `template` モジュール抽出（シングルパスレンダラの共通化） | 完了 |
| [#313](https://github.com/tomoya-k31/totsuka/issues/313) | `prompts` レジストリ + 埋め込み `defaults.toml`（挙動保存） | 完了 |
| [#314](https://github.com/tomoya-k31/totsuka/issues/314) | `[prompts]` / `[[workflows]].prompts` の設定面 | 完了 → **#465 で撤回** |
| [#317](https://github.com/tomoya-k31/totsuka/issues/317) | agent-ide-orca | 完了 |
| [#315](https://github.com/tomoya-k31/totsuka/issues/315) | 検証（決定 4）+ doctor の上書き数表示 + 本 ADR | 本 ADR と同時 |
| [#316](https://github.com/tomoya-k31/totsuka/issues/316) | opencode plan エージェント（frontmatter は固定） | 完了 |
| [#318](https://github.com/tomoya-k31/totsuka/issues/318) | task-source-slack | 完了 |

決定 2 の表のうち plan エージェントの frontmatter の行、および決定 4 の `opencode_plan_agent` に対する frontmatter 検査は #316 で実装済みである。#318（task-source-slack）はプラグイン側の `plugins/slack.toml` を使うため core の `[prompts]` とは独立しており、これでエピックの全段が入った。

# Amendment（2026-08-17、#465）: 設定面の撤回

## 何を撤回したか

| 面 | 2026-07-30 | 2026-08-17 以降 |
|---|---|---|
| `[prompts]`（グローバル 8 キー） | 上書き可 | **削除** |
| `[[workflows]].prompts`（7 キー） | 上書き可 | **削除** |
| `[[workflows]].rubric` | 上位 2 面に負けるレガシー | **唯一の上書き面**。profile 既定に勝ち、組み込みに勝つ |
| `defaults.toml` への外出し（決定 1 前半） | 採用 | 変更なし |
| 決定 2（何が動くかは変えない一線） | 採用 | 変更なし。むしろ面が減って守りやすくなった |
| 決定 3（インライン文字列のみ） | 採用 | 変更なし（対象が `rubric` 1 キーになった） |
| 決定 4（マーカー規約の検査） | 設定値に対する検査 | **検査対象が変わった**。組み込み `defaults.toml` に対する単体テストになり、設定に対しては `rubric` のプレースホルダ検査だけが残る |
| 決定 5（`TOTSUKA_PROMPTS_*` は作らない） | 採用 | 変更なし |

優先順位の梯子は 5 段から 3 段になった: `[[workflows]].rubric` > profile 既定（[#398](https://github.com/tomoya-k31/totsuka/issues/398) の成果物 URL 検収 / [#440](https://github.com/tomoya-k31/totsuka/issues/440) の人間承認検収） > 組み込み既定。

## なぜ

### 1. 設計が 2 週間で追い越した

`defaults.toml` の 11 キーのうち config に露出していたのは 8 キーで、**露出していない 3 キーはすべて `[prompts]` より後に入り、profile が自動選択するもの**だった。

| 日付 | 出来事 |
|---|---|
| 2026-07-30 | `[prompts]` 15 ノブ（本 ADR） |
| 2026-08-07 | `verification_nonclaim_exemption` 追加（[#390](https://github.com/tomoya-k31/totsuka/issues/390)） |
| 2026-08-09 | `verification_rubric_artifact_url` 追加 — **設定不可**（#398） |
| 2026-08-13 | `marker_self_report_confirm` / `verification_rubric_human_approval` 追加 — **どちらも設定不可**（#440） |

「新しい葉は profile が選び、設定はしない」が出荷から 2 週間で既定路線になった。残っていた 15 ノブは、その路線が確立する前の地層である。

### 2. ノブが実際にバグを生んでいた

梯子でグローバル `[prompts]` が profile 既定より**強かった**ため、キーを 1 つ書くだけで後から入った検収が黙って無効化された。設定リファレンスにも [ADR-0033](/decisions/adr-0033-workflow-profile.md) にも [ADR-0043](/decisions/adr-0043-human-approved-completion.md) にも「documented gap」として書いてあったが、書いてあること自体が問題だった:

- `[prompts].verification_rubric` を設定済み → `triage` workflow が **URL 検収にならない**
- `[prompts].marker_self_report` を設定済み → `design` / `implement` workflow が **確認プロトコルにならない**

**どちらも症状は「検収が緩くなる」方向**、つまり気づきにくい方向へ倒れる。梯子を逆順にする案（profile が勝つ）は「全 workflow に対して既に選ばれた文言を後から入った profile が黙って覆す」という対称の悪さがあり、これは**梯子の順序の問題ではなく、グローバルな面が存在することの問題**だった。残した `rubric` はワークフロー単位なので、運用者が見ていないワークフローに届かない。

### 3. 使用実績（根拠としては弱い）

実運用 config に `[prompts]` / `[workflows.prompts]` は 1 つも無く、唯一のプロンプト調整は `[[workflows]].rubric` — 本 ADR より前からあるキーだった。**この根拠は弱い**（期間 2.5 週間・運用者 1 人）ので、1 と 2 の補強材料として扱う。実際、残すキーを選ぶ判断にはこれが効いた。

## 正面から受け入れ直すコスト

**`defaults.toml` は `include_str!` でバイナリに埋め込まれるので、編集にはリビルドが要る。** `[prompts]` を消すと、**リビルドなしでプロンプトを変える経路が無くなる**。

これは本 ADR の Context がまさに問題視していたこと（「文言を調整するたびにコード変更とリビルドが必要」）であり、本 amendment は**その問題を意図的に受け入れ直す**決定である。前提が変わったからで、当時「運用上のチューニング対象」と見ていたプロンプト文は、2 週間後には profile と検収機構に結びついた**動作の一部**だと判明した。動作の一部を再ビルドなしに差し替えられることは利点ではない。

`rubric` だけを残したのは、そこだけが今も本当にチューニング対象だからである。ただし `rubric` も `config.toml` の編集を要するので、「リビルド不要」の利益は完全には失われていない。

## 撤回しなかったもの

- **決定 2 の一線。** プロンプトはスクリプト・argv・permission ブロックを変えられない。面が減ったぶん守りやすくなっただけで、緩めていない
- **`ALLOWED_PLACEHOLDERS` とマーカー検査。** 組み込みプロンプト同士の組み立て（`verification_prompt` が 4 つの葉を埋める）はノブを消しても続く。**消したのは「設定値の検証」であって「組み立ての検証」ではない** — 後者はむしろ強くなり、`defaults.toml` 自身が `ALLOWED_PLACEHOLDERS` に照らして検査されるようになった（従来は運用者が書いた値だけが検査され、全ビルドに載るアセットのタイポは無検査だった）
- **プラグイン側の `[prompts]`。** `plugins/slack.toml`（11 キー、[#318](https://github.com/tomoya-k31/totsuka/issues/318)）と `plugins/{github,notion}.toml`（#398）は対象外である。あちらは LLM 向けのプロンプトのみで、悪い上書きの被害が返信案の質低下に留まり完了検知を壊せない。危険度が違うので同じ判断を機械的には適用しない

## 組み込みプロンプトは英語で書く

上書き面が消えると、`defaults.toml` の文言は**運用者が触る設定値ではなくコードの一部**になる。
コードのコメント・識別子・`defaults.toml` の説明文はすべて英語なので、値だけ日本語で残ると
位置づけと言語が食い違う。**全 11 キーを英語へ揃えた**（`hooks::opencode` の
`PLAN_AGENT_FRONTMATTER` の `description:` も同様）。

削除対象でないキーまで英語化したのは、**組み立て後が混在物になるため**である。
`verification_prompt` は外枠で、葉として `verification_rubric` 系（残存）を埋める。外枠だけ
英語化するとジャッジへ渡るプロンプトが「英語の枠 ＋ 日本語の条件」になる。言語の境界を
上書き面の境界に合わせる理由は無いので、ファイル単位で揃えた。

**文法上の制約は翻訳後も維持している。** `verification_*` は「命令」ではなく「**真であるべき
条件**」として読まれる（決定 4 と #389）。英語は命令法へ流れやすいので、`Include the URL` 型の
書き方をしていないことを単体テストで固定した — `please allow` / `allow the stop` /
`do not block` が組み立て後に現れないことを直接検査する。

訳す過程で既存の不具合を 1 件見つけて直した。`verification_rubric_artifact_url` は
[#398](https://github.com/tomoya-k31/totsuka/issues/398) 以来 2 つの箇条書きを TOML の行継続で
繋いでおり、改行が消えて 1 行に潰れ、途中に `- ` が残っていた。**意味のほうは潰れていたおかげで
正しかった** — 枝は OR で並ぶので、2 つの箇条書きに分かれていたら「URL があれば内容が無関係でも
通る」に弱まっていた。1 つの箇条書きへ書き直し、各枝が 1 行 1 箇条書きであることを葉に対して
検査するテストを足した。

**エージェントの応答言語はプロンプトで指示していない。** タスク本文が日本語ならモデルは自然に
日本語で応答するはずで、ノブを減らす決定で新しい指示節を増やすのは筋が悪い。ただしこれは
**推測であって実測ではない** — 応答が英語化していないことの確認は実機検収に残る。

### 運用者の目に見える変更が 1 つある

`marker_self_report_confirm` が NEEDS_INPUT の reason に指定する文字列が
`"完了確認待ち"` → `"awaiting completion confirmation"` に変わる。これは
`run::hooks` が `notify_all` へ `WaitingInput` のペイロードとして渡すので、
**pane の確認待ちを知らせる Slack 通知の本文がその分だけ英語になる**。ワイヤ上でパースは
していないので機械的な追随先は無いが、運用者が読む文字列なのでここに記録する。

## 移行

`[prompts]` / `[[workflows]].prompts` は**パースはされ、検証で落ちる**。素の `unknown field` で落とすと、運用者が意図して書いた文言に対して「そのキーは無い」としか言わないことになるため、キーごとに何になったかを名指しするエラーを別途用意した。

```text
[prompts] sets `verification_rubric`, which was removed in favour of built-in
prompt text → write the criteria as `rubric` on the workflow itself — the one
prompt key that survived
```

移行期間は置かない。既存ファイルの母数が 0 だからである（実運用 config・dotfiles・リポジトリ内 fixture のいずれにも `[prompts]` は無い）。
