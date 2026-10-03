---
type: Decision
title: ADR-0110 反応するグループメンションを [slack] mention_groups で事前に限定する
description: プラグイン設定 `[slack] mention_groups = ["S…"]` で、workflow と無関係にタスク化するユーザーグループを限定できるようにした決定。省略は所属全グループ、[] はグループメンション無効、指定は所属との積集合。所属外の値と、リスト外を名指す to_group は initialize で拒否する。Gateway は変えない。
resource: https://github.com/tomoya-k31/totsuka/blob/main/plugins/task-source-slack/src/config.rs
tags: [decision, slack, mention, config, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-03T15:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[ADR-0081](/decisions/adr-0081-slack-group-mention-routing.md) の `to_group`（グループ → workflow の振り分け）はそのまま残し、その手前に「そもそもどのグループに反応するか」の絞り込みを足す。

# Context

所属グループ宛のメンションは、`to_group` で名指ししていなくても catch-all（素の `mention = true`）に落ちてタスクになる。人数の多い全体連絡用グループに入っていると、そのメンションがすべてタスクになる。[ADR-0109](/decisions/adr-0109-slack-bot-mentions.md) で bot のメンションも通るようになったので、この問題は大きくなった。

workflow 側だけで止める方法は catch-all を外すことしかなく、それをすると個人メンションも止まる。「個人メンションは全部受け、グループは指定したものだけ」は、workflow の設定では書けなかった。

# Decision

## 1. `[slack] mention_groups` を足す。workflow の語彙にはしない

反応するかどうかは「このプラグインが何を自分宛とみなすか」の問題で、どの workflow に渡すかとは別の軸である。判定表④（名指し判定）が参照する所属グループ集合を、このリストとの積集合にする。

| 値 | 意味 |
|---|---|
| 省略 | 所属する全グループ（従来どおり） |
| `[]` | グループメンション無効。個人メンションだけ |
| `["S…", …]` | 書いたグループだけ |

個人メンション（`<@自分>`）には効かない。

## 2. 黙って効かない書き方は起動時に拒否する

- `S…` の形をしていない値（ハンドル・`U…`）は `config validate` で拒否する（オフラインで分かる）
- 所属していないグループは `initialize` で拒否する。`to_group` と同じ所属照合に乗せるので、非空のときは `usergroups:read` が必須になる
- `mention_groups` に無いグループを名指す `to_group` は `initialize` で拒否する。判定表④で先に落ちるので、その workflow は決して動かない

## 3. Gateway は変えない

Gateway は所属を知らず、`<!subteam^…>` を含む投稿をすべて publish している（ADR-0072 決定 8）。絞り込みはプラグインの判定表④で行う。Gateway に持たせると設定の写しが 2 箇所になり、古いほうが黙ってメンションを落とす。

# Consequences

- 除外されたグループ宛のメンションは、どの workflow にも渡らず判定表④で捨てられる。ログには残らない（名指しされていないのと同じ扱い）
- チャンネル監視・リアクション経路には効かない。メンション経路だけの設定である
