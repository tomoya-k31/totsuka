---
type: Decision
title: ADR-0044 ローテートする credential は cmd:<command> で毎回取得する（コピーを保管しない）
description: "秘密参照の第 4 形式として cmd:<command> を追加した決定。設定に書いたコマンドを解決時に /bin/sh -c で実行し stdout を秘密値に使う。gh auth token のように別ツールが管理・ローテートする credential を op/keychain へ写すとコピーが黙って死ぬため、毎回現在値を取る形にする。実行は resolve 時のみで、doctor は op:// と同じ非対話原則で probe を skip する。stdout は SecretString 直行でログにもエラーにも出さない。"
resource: https://github.com/tomoya-k31/totsuka/issues/444
tags: [decision, secrets, config, security, adr]
generated: { by: claude-code/fable-5, at: 2026-08-13T19:40:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-444
    resource: https://github.com/tomoya-k31/totsuka/issues/444
    title: "feat(core): 秘密参照に cmd:<command> スキームを追加する"
---

# Status

stable（[#444](https://github.com/tomoya-k31/totsuka/issues/444)）。[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)（op:// バックエンド）の非対話原則を継承する。

# Context

#440 の実運用設定で、task-source-github の token に **gh の OAuth token** を使いたくなった（scopes に `project` があり、専用 PAT を増やさなくて済む）。既存の 3 形式はどれも噛み合わなかった:

- **`op://` / `keychain:` にコピーする**: gh を再ログインすると元 token が無効化され、**保管庫内のコピーが黙って死ぬ**。ローテートする credential の複製は、複製である限りこの問題から逃れられない
- **`${GITHUB_TOKEN}`**: `GITHUB_TOKEN=$(gh auth token) tt run --watch` と起動コマンドを毎回汚す

「取得コマンドそのものを設定に書く」のは git / docker の credential helper と同じ確立されたパターンで、**解決のたびに現在値を取るのでコピーが存在しない**。前例はアーキテクチャ内にもある — `op://` の解決は実体として `op read` へのシェルアウトであり、`cmd:` はその一般化である。

# Decision

秘密参照の第 4 形式として **`cmd:<command>`** を追加する。

- **構文**: 接頭辞 `cmd:`、残り全部がコマンド文字列（`token = "cmd:gh auth token"`）。`keychain:` と同じ「接頭辞 + 中身」形式 — `op://` の `//` は op の native URI 由来で、こちらには意味がない。空・空白のみは `InvalidReference`
- **実行**: `/bin/sh -c <command>`、orchestrator のプロセス環境を継承（PATH は起動シェル由来なので mise / homebrew のツールが見える）。**末尾の改行（`\n` / `\r\n`）はすべて除去** — 大半の CLI は改行付きで出力し、残すと Authorization ヘッダが壊れる。op が `--no-newline` で解いた問題を、フラグの無い任意コマンドでは resolve 側で解く
- **エラー**（cause + next action、§7）: 非ゼロ exit → stderr の**先頭行だけ**引用。exit 0 かつ空出力 → エラー（空トークンで API を叩いて謎の 401 になるより起動時に落とす）
- **§5.2 不変条件**（op と同一）: stdout は平文の秘密であり、即 `SecretString` に包む。ログにもエラーにも決して出さない（失敗時の stdout は書きかけの秘密でありうる）
- **doctor**: `op://` と同じ非対話原則（#289）で **probe を skip**。doctor はコマンドが対話プロンプトを出さないことを判別できない（`cmd:op read …` という綴りが実在しうる）
- **実行タイミング**: resolve 時のみ（`totsuka run` の起動経路）。config の parse・`config show` はコマンドを実行しない

# Consequences

- **config に新しい権限は生まれない**: config は既にプラグインバイナリのパスを持ち、totsuka はそれを実行する。config を書ける者は既にコマンド実行能力を持っている
- gh 再ログイン後も設定変更・値の入れ直しは不要になる（毎回 `gh auth token` が現在値を返す）
- タイムアウトは付けない（op バックエンドにも無い。一貫性優先）。ハングする コマンドは起動停止として顕在化する。必要になったら別 issue
- 任意の文字列 leaf で有効なので、`cmd:` で始まる平文値は書けなくなる（`op://` / `keychain:` と同じ制約の追加）
- **コマンド文字列に秘密を直書きしないこと**。参照文字列（コマンド全文）は op:// URI と同様に「秘密の在処の名前」としてエラーメッセージへ引用される。`cmd:curl -H "Bearer xoxp-…"` のような綴りは「設定に平文の秘密を書かない」規則の違反であり、この形式の目的（コマンドに秘密を取得させる）の取り違えでもある。エラーからの参照 redact は採らない — どの参照が失敗したかが分からなくなり、op:// が URI を引用する既存の一貫性も壊れるため（Copilot レビュー指摘への回答として記録）

# 不採用案

- **`cmd://` 表記**: `//` は op の native URI 由来で、こちらには意味がない
- **`${GITHUB_TOKEN}` 運用の継続**: 動くが、起動コマンドの汚れと export 忘れは構造的に消えない
- **gh token の op コピー**: ローテートで黙って死ぬ（本決定の起点。実際に一度この形で配線しかけ、レビューで指摘される前にユーザーの「なぜコピーするのか」で止まった）
