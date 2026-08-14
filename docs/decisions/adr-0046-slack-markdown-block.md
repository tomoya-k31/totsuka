---
type: Decision
title: ADR-0046 Slack 返信は Block Kit markdown ブロックで投稿する
description: "エージェントの返信（GitHub-flavored Markdown）を chat.postMessage の text に素のまま渡すと Slack が mrkdwn として解釈して崩れるため、標準 Markdown を受け付ける Block Kit の markdown ブロックで投稿する決定。text は通知・検索用フォールバックとして全文を残し、累計 12,000 字上限を超える返信は従来どおり text のみに縮退する。GFM→mrkdwn 変換器の自作と指示文での mrkdwn 強制は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/454
tags: [decision, slack, plugin, block-kit, markdown, adr]
generated: { by: claude-code/fable-5, at: 2026-08-14T21:30:00+09:00 }
verified: { by: human:tomoya-k31, at: 2026-08-14T21:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-454
    resource: https://github.com/tomoya-k31/totsuka/issues/454
    title: "fix(slack): 返信の Markdown が mrkdwn として崩れる — markdown ブロックで投稿する"
  - id: slack-markdown-block
    resource: https://docs.slack.dev/reference/block-kit/blocks/markdown-block
    title: "Slack Block Kit — Markdown block"
---

# Status

stable（[#454](https://github.com/tomoya-k31/totsuka/issues/454)）。

# Context

承認済み返信は `chat.postMessage` の `text` に**エージェントの出力そのまま**で投稿していた。エージェントは自然に GitHub-flavored Markdown（GFM）で書くが、Slack の `text` は **mrkdwn** という別方言で解釈される。実機で観測した崩れ:

- `**太字**` が文字のまま表示される（mrkdwn の太字は `*一重*`）
- コードフェンスの言語タグ（` ```python `）がコードブロック本文の 1 行目になる
- `[text](url)` がリンクにならない（mrkdwn は `<url|text>`）
- 見出し・表が表現されない
- さらに**正しい mrkdwn である一重バッククォートすら日本語隣接で描画されないことがある**（`` `sorted()` `` が生のまま表示された）— mrkdwn の日本語文中パースは不安定で、方言変換では品質上限に届かない

指示文（`defaults.toml`）にはフォーマット指定が無く、承認前プレビュー（`draft_blocks` の mrkdwn section）も同じ生テキストで崩れていた。

# Decision

返信を Block Kit の **`markdown` ブロック**（`{"type": "markdown", "text": <GFM>}`）で投稿する。標準 Markdown を受け付け、Slack がサーバー側で `rich_text` / `table` ブロックへ変換する、公式に「LLM 応答の投稿」用と案内されているブロックである。

- **`text` は全文のまま残す**: blocks があるとき `text` は通知・検索用フォールバックになる
- **プレビューも同じブロック**: `draft_blocks` の返信案本文を markdown ブロックにし、承認前後で見た目を一致させる
- **12,000 字上限の縮退**: markdown ブロックの累計上限（12,000 字/payload）を超えうる返信は従来どおり `text` のみで投稿し、プレビューも従来のクリップ付き mrkdwn section に戻る。判定は**バイト長**で行う — Slack がどの単位（バイト/UTF-16/文字）で数えていても、バイト長 ≤ 12,000 なら超過し得ない保守的判定で、「ローカル判定を通ったのに API に拒否され、承認ボタンが永遠に失敗する」事故を構造的に排除する
- **指示文は変更しない**: エージェントは自然な GFM のままでよく、プロンプトでの mrkdwn 強制が不要になる

実機検証済みの事実（2026-08-14、user token で実測）:

- `chat.postMessage` + **user token（xoxp）** + markdown ブロック → `ok: true`（公式ドキュメントは token 種別を明記しておらず、ここが採用の最終関門だった）
- `<@USERID>` メンションは `rich_text` の `user` 要素へ変換され、**メンションとして機能する**（返信先頭の機械的メンション前置が生きる）
- 太字・リスト・インラインコード・リンク・言語タグ付きコードフェンス・表の描画を目視確認

# Consequences

- 1 つの markdown ブロックは投稿後に**複数ブロックへ分割されうる**（実測: `rich_text` + `table`）。投稿後に blocks を読み戻して同一性を検証するコードは書けない
- 12,000 字超の返信だけは従来どおり崩れた表示になるが、頻度は低く（返信は通常数千字未満）、全文は失われない
- mrkdwn の日本語隣接 quirk はヘッダ・context 等の**プラグイン自作文言**には残るが、これらは固定文言で mrkdwn として正しく書かれている

# 不採用案

- **GFM → mrkdwn 変換器の自作**（pulldown-cmark）: 決定的だが、mrkdwn は見出し・表・ネストリストを表現できず、正しい mrkdwn ですら日本語隣接で崩れる quirk は変換では直せない。保守コストも継続的にかかる
- **指示文で mrkdwn を強制**: モデルは高頻度で GFM に戻り信頼性が低い。markdown ブロック採用でそもそも不要になった
