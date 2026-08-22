---
type: Decision
title: ADR-0054 組み込みプロンプトは英語で書き、成果物の言語は文脈に従わせる
description: "プラグインの defaults.toml が持つエージェント向け指示文を英語へ統一し、そこに焼き込まれていた「日本語で」という言語指定を「スレッド / 元 issue と同じ言語で書け」という規則へ置き換える決定。locale を読む案と output_language キー新設案は却下。人間が読む UI 文言とタスク本文のラベルは対象外。"
tags: [decision, prompts, i18n, plugins, adr]
generated: { by: claude-code/opus-5, at: 2026-08-22T00:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0023
    resource: /decisions/adr-0023-prompt-externalization.md
    title: "ADR-0023 プロンプトを defaults.toml へ外出しする"
  - id: adr-0024
    resource: /decisions/adr-0024-agent-instruction-layers.md
    title: "ADR-0024 エージェントへの指示は task_source プラグインが所有する"
  - id: config-reference
    resource: /development/config-reference.md
    title: "config.toml リファレンス"
  - id: slack-component
    resource: /components/task-source-slack.md
    title: "task-source-slack"
---

# Status

stable。

# Context

`defaults.toml` に外出しされた組み込みプロンプト（ADR-0023）のうち、
`crates/orchestrator-core/src/prompts/defaults.toml`（385 行）は最初から英語だったが、
4 つのプラグイン（slack / github / notion / orca）の指示文だけが日本語だった。
public 化を控えて、この非対称を解消したい。

素直に英訳すると 1 箇所だけ矛盾する。Slack の `reply_instructions` が
**「返信案を日本語で作成してください」と言語そのものを名指ししていた**ためで、
これを `in Japanese` と訳すと「英語化したのに日本語固定が残る」という最悪の形になる。

`defaults.toml` の中身は 1 種類ではない。実際には 3 つが混在している:

| 種別 | 例 | 誰のための文字列か |
|---|---|---|
| ① エージェントへの指示 | `*_instructions`、orca の `plan_prefix`、slack の `classifier_*` | 配管。LLM しか読まない |
| ② 成果物の言語指定 | `reply_instructions` 内の「日本語で」 | 利用者のワークスペースの性質 |
| ③ タスク本文のラベル | slack の `body_template`（`- 送信者:` 等）、`body_thread_*` | エージェント + ペインを見る人間 |

矛盾しているのは②だけで、①と③は独立に決められる。

# Decision Drivers

- **公開リポジトリの配管は英語であるべき** — ①はコードと同じ層の文字列である
- **利用者の言語を totsuka が決めてはいけない** — 現状の「日本語で」は、エージェント側の
  設定（`~/.claude/CLAUDE.md` 等）と元メッセージの言語の**両方を上書き**している
- **設定源を増やさない** — 静かに壊れる新しい経路を作らない
- **人間が読む文字列は利用者の言語のままでよい** — 配管と UI は別の判断

# Options Considered

| 案 | 内容 | 判断 |
|---|---|---|
| A | ①のみ英訳し、②は `in Japanese` と訳す | 却下。英語化の目的を自ら潰す |
| B | ①を英訳し、②を文脈追従の規則に置き換える | **採用** |
| C | B に加えて `output_language` 設定キーを全 task source に新設 | 見送り（下記） |
| D | `LANG` / `LC_ALL` から導出する | 却下（下記） |

## なぜ D（locale を読む）が成立しないか

1. macOS の Terminal は `LANG` を設定しないことが多く、設定していても `en_US.UTF-8` のまま
   日本語で使っている利用者が大半である
2. エージェントは herdr のペインで動くので、totsuka 自身のプロセス環境とは別物
3. POSIX locale は「表示の言語」であって「文章を書く言語」ではない
4. 外れても**静かに外れる** — 英語の返信が本人名義で Slack に投稿されるまで気づけない

## なぜ C（`output_language` キー）を今は入れないか

文脈追従で足りるかどうかは実機で測れる。測る前にキーを足すと、
「導出のまま固まった設定」が 1 つ増える。上書きの口は既に 3 つある:

| 口 | 範囲 |
|---|---|
| `plugins/*.toml` の `[prompts]` | 全キーを個別に上書き |
| `plugins/slack.toml` の `reply_style` | Slack のみ・1 行 |
| `[[workflows]] initial_prompt` | 全 source・新規会話のみ |

# Decision

## 1. ①は英語に統一する

対象は 4 ファイル: slack の `reply_instructions` / `implement_instructions` /
`triage_instructions` / `reply_style_suffix`、github と notion の
`triage_instructions` / `design_instructions` / `implement_instructions`、
orca の `plan_prefix`。

## 2. ②は言語を名指しせず、文脈に従わせる

指示文には具体的な言語名を書かない。代わりに規則を書く:

```text
Write the reply in the same language as the thread.
```

github / notion は `the same language as the source issue` / `the source page`。
これで足りる理由は、**「利用者の言語」が既にプロンプトの中に入っている**からである。
Slack スレッドも Issue 本文も利用者の言語で書かれた状態でタスク本文としてエージェントに
渡っており、成果物の宛先も同じ人たちである。追加の入力源は要らない。

**この方針は今後のプロンプト編集を拘束する** — 組み込みプロンプトに言語名を書かない。

## 3. ③と UI 文言は対象外

slack の `body_template` / `body_thread_header` / `body_thread_line` /
`body_thread_unavailable` は日本語のまま残す。ペインでこれを読むのは人間だからである。
同じ理由で、Rust ソースにある人間向け文字列 —— Slack の承認 UI（`approval.rs`）と
macOS 通知のタイトル（`notifier-macos`）—— も触らない。**配管は英語、UI は利用者の言語**。

# Consequences

## 良くなること

- 組み込みプロンプトの言語が core と 4 プラグインで揃う
- 日本語以外のワークスペースで、返信が日本語で返ってこなくなる
- 「言語を指定する」という判断が、totsuka の既定から利用者側へ戻る

## 悪くなること・注意点

- **文脈追従は保証ではなく確率である。** 構造的な強制ではないので、モデルが外す可能性は
  残る。外した場合の被害は「本人名義で投稿される返信の言語が違う」ことなので小さくはない。
  これが実運用で問題になるなら案 C を入れる
- **③を英語化しなかったので、指示（英語）と本文ラベル（日本語）が混在する。** 意図的である。
  ただし将来③も英語化するなら、②の規則文は必須になる（言語を推論する材料が減るため）
- **#318 / #317 のバイト同一テストが 2 つ弱くなった。** `reply_instructions` /
  `reply_style_suffix` / orca の `plan_prefix` は意図的に書き換えたので、期待値を
  `defaults.toml` から貼り直した。これらは**意図しない編集しか検出しない** —
  当該テストのコメントにその旨を明記した。`reply_instructions` を意味で守るのは
  `the_reply_instructions_ask_only_for_a_reply`（#527 の門番。ADR は無い）のほうである

# 検証

`the_reply_instructions_ask_only_for_a_reply` は、#527 の形（成果物 URL の要求）を
差し戻すと落ちることを実測で確認した。肯定チェック（`the only deliverable` /
`do not open a pull request` / `do not attempt it`）と否定チェック（`URL` を含まない）の
両方が独立に発火する。

**実機での言語追従は未検証。** 日本語スレッドに対して日本語の返信が返ることを
実機 E2E で確認するまで、この ADR に `verified` は付けない。
