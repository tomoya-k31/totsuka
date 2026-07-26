---
type: Policy
title: 端末出力の信頼境界（外部由来テキストの無害化）
description: CLI が第三者の書いたテキスト（Slack 本文・GitHub issue タイトル・author・url・source_task_id）を端末へ出す際の制御シーケンス無害化ポリシー。safe() の適用範囲、エスケープであって除去ではない理由、--json を通さない理由、one_line の 3 段の順序、未カバー経路を定める。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/src/common.rs
tags: [security, cli, terminal, ansi, escape-sequence, sanitization, output]
timestamp: 2026-07-26T21:00:00Z
status: active
owner: tomoya-k31
---

# 前提: タスクソースは信頼境界の外側

totsuka のタスクは Slack のメッセージや GitHub の issue から生まれる。したがって
`title` / `body` / `author` / `url` / `source_task_id` は **第三者が内容を決められる**。
Slack のチャンネルには社外ゲストが入りうるし、public リポジトリの issue は誰でも立てられる。

これらは [state.db](/data/state-db.md) に**検証も加工もせずそのまま**保存される（保存側は正しい。
監査は「実際に何が投稿されたか」を読む必要がある）。危険なのは**取り出して端末へ描くとき**である。

# 脅威: 任意コード実行ではなく「出力への信頼」の破壊

ANSI 制御シーケンスはコード実行を与えない。壊すのは
**「CLI の出力を読めば実態が分かる」という前提**の方である。

| シーケンス | できること |
|---|---|
| `ESC[2J` / `ESC[1A` / `ESC[2K` | 画面消去・カーソル移動・行消去。**既に印字した行を上書きできる**ので、`task list` である行の state を別タスクのものに見せかけられる |
| OSC 8 | ハイパーリンクの**表示テキストと実リンク先を食い違わせる**。`url:` 行はまさにこれから踏まれる |
| OSC 52 | 端末によっては**クリップボードへ書き込む** |
| `CR`（`\r` 単独） | 現在行を桁 0 から書き直す。行末に付ければ手前の内容を消せる |
| U+202E 等の bidi override | **読み順を反転**させる。`invoice[U+202E]gnp.exe` は `invoiceexe.png` と読める |

いずれも「オペレータが出力を見て判断する」運用を成立しなくさせる。

# ポリシー

## 1. 外部由来フィールドは端末へ出す直前に `safe()` を通す

`crates/orchestrator-cli/src/common.rs` の `safe(&str) -> Cow<str>` が唯一の入口。
`print_json` の隣に置いてあるのは、**JSON 経路と人間経路の分岐が 1 ファイルで見える**ようにするため。

対象は [orchestrator-cli](/components/orchestrator-cli.md) の human 出力のうち:

| コマンド | フィールド |
|---|---|
| `task list` | `title` |
| `task show` | `title` / `source_task_id` / `url` / `worktree_path` / `branch` / `author` / `body` / `session_id` |
| `status` | `title` / `branch` / `worktree_path` |
| `logs` | `message` / `extras` / `timestamp` / `level` / JSON としてパースできない行 |

`state` / `workflow` / `mode` / `repo` / `source` は **totsuka 自身か config が決める**ので通さない。
自前のテキストまで通すと、将来装飾（色付け）を入れたときに自分のエスケープを自分で壊す。

## 2. 除去ではなく**エスケープ**する

消された文字は「そこに何かがあったこと自体」がオペレータに見えない。それは内容についての
別種の嘘になる。`safe()` は `ESC` を `\u{1b}`、`CR` を `\r` のように**可視の綴りへ置き換える**。
`char::escape_debug` を使うので、慣用のあるものは `\n` / `\r` / `\t`、それ以外は `\u{...}` になる。

日本語・絵文字・`https://...`・`C:\path` などの通常のテキストは**1 バイトも変わらない**。
何も書き換える必要が無い入力では `Cow::Borrowed` を返す（`task list` は 1 行ごとに呼ぶため）。

## 3. 対象は C0/C1/DEL **に加えて** bidi override

`char::is_control()` は C0・DEL・C1（`Cc`）だけで、bidi override（`Cf`）を含まない。
しかし読み順の偽装は**同じ画面に対する同じ攻撃**なので、`is_screen_control()` が
`U+202A..U+202E` と `U+2066..U+2069` を明示的に足している。

## 4. `--json` は**通さない**

`--json` は `print_json` → `serde_json` であり、制御文字は既に `\u00xx` へエスケープ済み。
ここに `safe()` を重ねると**二重エスケープ**になり、機械が読む値が壊れる。
`--json` の値はソースが送ったものと**バイト単位で一致する**（切り詰めもしない）。

構造上これは守られている: `TaskDetail` / `TaskRow` の**構築は JSON 分岐より前**にあり、
`safe()` は**分岐より後の print サイトだけ**に置かれている。
**構造体フィールド側を無害化してはならない** — `--json` まで巻き込む。

## 5. `one_line` の 3 段は順序が意味を持つ

会話本文のプレビュー（`task show`）は fold → escape → clip の順で、入れ替えできない。

1. **whitespace を畳む** — `split_whitespace` が `\n` / `\r` / `\t` をトークン区切りの空白 1 個に
   変えるので、これらは段 2 に到達せずエスケープも不要になる
2. **残りをエスケープ** — `ESC` や `BEL` は whitespace ではないので段 1 を素通りする。ここが本命
3. **切り詰め** — 最後。`limit` が**実際に描かれる幅**を縛る。escape より前に切ると、72 個の `ESC` が
   後段で 432 文字の `\u{1b}` に膨らんで結局行が折り返す

# カバーしていない経路（既知）

- **`totsuka run` 自身の stderr ログ層** — `orchestrator-core/src/logging/layer.rs` は
  フィールド値を生で追記し、`run` は `title` をそこへ出す。`--debug` は全コマンドでこの層を通る。
  CLI クレートの外なので #280 の対象外とした
- `main.rs` の `error:` 行、`focus` の `reason`、`doctor` の detail — git の stderr やプラグインの
  エラー文が乗る。外部由来度は低いが無検査

# 検証

`safe()` のユニットテスト（`common.rs`）が、①通常テキストが不変で `Cow::Borrowed` であること
②`ESC[2J` / `ESC[1A` / OSC 8 / `CR` / bidi / `BEL` / `NUL` が**消えずに**無害化されること
③出力が常に 1 行であることを固定する。

統合テスト `external_text_cannot_repaint_the_terminal_yet_json_stays_verbatim`
（`crates/orchestrator-cli/tests/cli_commands.rs`）が、敵対的な `title` / `url` / `body` /
`author` / `source_task_id` を持つタスクを state.db に仕込み、`task show` / `task list` / `status`
の human 出力に生の `ESC` も `CR` も無いこと・ペイロードの可読部分が消えていないこと・
`task list` の行数が「ヘッダ + 1 行」のままであること、そして **`--json` の値が投稿されたものと
完全一致すること**を検証する。
