---
type: Decision
title: ADR-0033 ワークフローの原型は [[workflows]].profile の 4 値で束ねる
description: "2 値の WorkflowMode では「worktree は read-only だが外部へは書く」を表現できないため、answer / triage / design / implement の 4 原型を Rust 固定で定義し、mode・output・verification をまとめて解決する決定。profile と mode / verification の併用は CONFIG_INVALID、output だけは上書き可。resolved アクセサで解決を Workflow::from_config に一元化し、state.db と plugin-protocol は変更しない。"
resource: https://github.com/tomoya-k31/totsuka/issues/394
tags: [decision, config, workflow, profile, permissions, adr]
generated: { by: claude-code/opus-5, at: 2026-08-09T12:00:00+09:00 }
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
| profile ごとの `permissions.deny` セット | [#395](https://github.com/tomoya-k31/totsuka/issues/395) | 未着手 |
| 成果物の書き手の分割と URL 実在検収 | [#398](https://github.com/tomoya-k31/totsuka/issues/398) | 未着手 |
| profile が要求する外部ツールの認証検査 | [#399](https://github.com/tomoya-k31/totsuka/issues/399) | 未着手 |

つまり**この時点の profile は「mode / output / verification の別名」でしかない**。権限としての実効性は #395 が入って初めて生まれる。実機検収も未了なので `verified` は付けていない。

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

`upsert_workflow` は `profile` / `mode` / `output` を「値が無ければキーごと消す」書き方にした。明示記法から profile へ書き換えた既存エントリに `mode` が残ると、ウィザード自身が `ProfileConflict` を書き込むことになる。

# Consequences

## 良くなること

- 権限に関わる組み合わせを人間が合わせなくてよくなる。#395 が入れば「answer なのに worktree を編集できた」が構造的に起きなくなる
- 「worktree は read-only だが外部へは書く」が初めて表現可能になり、#393 の WF 3 / 4 / 6 が書けるようになる
- 新しい原型が要るときに足す場所が 1 つ（`Profile` の enum と解決テーブル）に決まる

## 引き受けたコスト

- **設定の書き方が 2 通りになった。** profile 記法と明示記法が併存する。統一しなかったのは「Human sign-off required」のように 4 原型で表せない組み合わせが実在するためで、明示記法は非推奨ではない
- **ロールバックが非対称。** profile を使った config は旧バイナリでは `deny_unknown_fields` によりパースエラーになる。新 → 旧に戻すときは config も戻す必要がある（リリースノートに明記する）
- **この時点では profile に実効性が無い。** 上の Status 表のとおり、権限としての意味は #395 待ちである。`profile = "design"` と書いても worktree の編集は止まらない。それを止めるのが #395 の仕事で、本 ADR はその受け皿を用意しただけである

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
