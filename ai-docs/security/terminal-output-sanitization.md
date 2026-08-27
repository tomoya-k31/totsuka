---
type: Policy
title: 端末出力の信頼境界（外部由来テキストの無害化）
description: totsuka が第三者の書いたテキスト（Slack 本文・GitHub issue タイトル・author・url・source_task_id）を端末へ出す際の制御シーケンス無害化ポリシー。safe() の置き場所（core の terminal モジュール）と適用範囲、エスケープであって除去ではない理由、--json と JSON ログを通さない理由、one_line の 3 段の順序、menu が足す SwiftBar 書式の第 2 層、未カバー経路を定める。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/terminal.rs
tags: [security, cli, terminal, ansi, escape-sequence, sanitization, output]
generated: { by: claude-code/opus-5, at: 2026-08-28T07:40:00+09:00 }
status: stable
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

`crates/orchestrator-core/src/terminal.rs` の `safe(&str) -> Cow<str>` が唯一の実装。

#280 では CLI の `common.rs` に置いていたが、**印字側が 2 クレートに跨がる**ため

（[orchestrator-cli](/components/orchestrator-cli.md) の human 出力と
[orchestrator-core](/components/orchestrator-core.md) 自身の stderr ログ層）、#297 で core へ移した。
`orchestrator_cli::common::safe` はその **re-export** で、`print_json` の隣に置いたままにしてある
（**JSON 経路と人間経路の分岐が 1 ファイルで見える**ようにするため）。

対象は [orchestrator-cli](/components/orchestrator-cli.md) の human 出力のうち:

| コマンド | フィールド |
|---|---|
| `task list` | `title` |
| `task show` | `title` / `source_task_id` / `url` / `worktree_path` / `branch` / `author` / `body` / `session_id` |
| `status` | `title` / `branch` / `worktree_path` |
| `logs` | `message` / `extras` / `timestamp` / `level` / JSON としてパースできない行 |
| `doctor`（#297） | **すべての `Check`** の `name` / `detail` / `action`、および TTY 時の対話プロンプト（孤児 worktree の削除確認・孤児 pane の解放確認と、その結果行） |
| `menu`（#585） | `title` / `state` / `workflow` と、`bash=` に補間するバイナリのパス。**このコマンドだけは `safe()` の外側にもう 1 層ある** — 下記参照 |

# `menu` の第 2 層（#585）

`totsuka menu` の既定出力は SwiftBar のプラグイン書式で、その行は `text | key=value …` と読まれる。
つまり **`|` がメタ文字**であり、タイトルに `|` が 1 つあれば行にパラメータ（`bash=` を含む）を追加できてしまう。
これは本文書が扱う脅威 —— **第三者が内容を決められるテキストが、表示を乗っ取る** —— の一形態であって、
制御文字とは別の入口である。`safe()` はこれを知らない（知る必要も無い。`|` は端末にとって普通の文字である）。

**しかも SwiftBar が読む構文は 1 層ではない。** 行テキスト内の**バックスラッシュエスケープを SwiftBar 自身が処理する**（2.1.1 で実測: `\n` は本物の改行になり、知らないエスケープは `\u{7c}` → `u{7c}` とバックスラッシュだけ食われる）。**`safe()` の退避は、この 2 層目で元に戻される。**

初版はここを見落として出荷し、実機検収で捕まった —— 改行を含むタイトルが **1 行を 3 行に割り、`---` の区切り線まで偽装できた**。ユニットテストは全部通っていた。制御文字を「見える形」にする `safe()` の出力自体が、別のパーサにとってはまだ命令だった、という形である。

したがって `menu_cmd::menu_text` は 3 段で無害化する。順序に意味がある:

1. `safe()` —— 制御文字を可視のエスケープ形へ
2. **バックスラッシュを全て二重化** —— 1 が作ったものも含む。これが無いと 1 の退避が無効になる
3. `|` を（二重化済みの形で）退避 —— operator が読むのは `\u{7c}`

2 段目も **escape-not-strip** を守る。注入テキストは消えず、行の**テキスト側**に丸ごと残るので、
何が来たのかは読める。`--json` は他のコマンドと同じくバイト完全のまま。

**この防御を Rust の外へ出さないことが、`menu` がサブコマンドである理由そのものである。**
整形を jq / シェルスクリプトに任せる設計も検討したが（[ADR-0065](/decisions/adr-0065-menubar-status.md)）、
それは型もテストも無い場所へこの層を移すことになる。

加えて [orchestrator-core](/components/orchestrator-core.md) の human ログ層（stderr、#297）:

| 経路 | フィールド |
|---|---|
| `logging::layer`（`LogFormat::Human`） | イベントの `message` と**全フィールド値**（`run` の `title = %task.title` 等）。`--debug` は全コマンドでこの層を通る |

`doctor` だけ「特定フィールド」ではなく `Check` 全部を通すのは、外部由来テキストが乗る経路が
pane label（`totsuka {source_task_id}`、[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）と
孤児 worktree パスだけでなく、git の stderr・tmux / プラグインのエラー文にも散っているため。
レンダリングループ 1 箇所に置けば全部覆え、置き場所も 1 つで済む。

ログ層で **redact と escape を別段にしている**のも同じ理屈の裏返しで、
redact は「誰が読んでよいか」（両形式に効く）、escape は「画面に何ができるか」（human だけ）で
守る対象が違う。同じ関数に混ぜると JSON ログまで巻き込む。

`state` / `workflow` / `mode` / `repo` / `source` は **totsuka 自身か config が決める**ので通さない。
自前のテキストまで通すと、将来装飾（色付け）を入れたときに自分のエスケープを自分で壊す。
ログ層の timestamp / level / target / フィールド**名**を通さないのも同じ理由で、
level の ANSI 色を `safe()` に通せば色が付かずエスケープが印字される。

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

## 4. `--json` と JSON ログは**通さない**

`--json` は `print_json` → `serde_json` であり、制御文字は既に `\u00xx` へエスケープ済み。
ここに `safe()` を重ねると**二重エスケープ**になり、機械が読む値が壊れる。
`--json` の値はソースが送ったものと**バイト単位で一致する**（切り詰めもしない）。
`$XDG_STATE_HOME/totsuka/logs/` の JSON Lines も同じ（読むのは `jq` であって端末ではない）ので、
`logging::layer` の無害化は `LogFormat::Human` の分岐の中だけに置く。

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

- `main.rs` の `error:` 行、`focus` の `reason` — git の stderr やプラグインのエラー文が乗る。
  外部由来度は低いが無検査（`doctor` の同種の経路は #297 で `Check` ごと覆われた）

塞いだもの:

- **`totsuka run` 自身の stderr ログ層**（#297）— `logging::layer` の human 形式で
  `message` と全フィールド値を通すようにした
- **`doctor` の孤児 pane / 孤児 worktree の報告**（#297）— `--json` 分岐より後の
  human レンダリングループと対話プロンプトで通すようにした

# 検証

`safe()` のユニットテスト（`orchestrator-core/src/terminal.rs`）が、①通常テキストが不変で `Cow::Borrowed` であること
②`ESC[2J` / `ESC[1A` / OSC 8 / `CR` / bidi / `BEL` / `NUL` が**消えずに**無害化されること
③出力が常に 1 行であることを固定する。

統合テスト `external_text_cannot_repaint_the_terminal_yet_json_stays_verbatim`
（`crates/orchestrator-cli/tests/cli_commands.rs`）が、敵対的な `title` / `url` / `body` /
`author` / `source_task_id` を持つタスクを state.db に仕込み、`task show` / `task list` / `status`
の human 出力に生の `ESC` も `CR` も無いこと・ペイロードの可読部分が消えていないこと・
`task list` の行数が「ヘッダ + 1 行」のままであること、そして **`--json` の値が投稿されたものと
完全一致すること**を検証する。

`doctor` は同型の E2E `doctor_human_output_cannot_repaint_the_terminal_yet_json_stays_verbatim`
（`crates/orchestrator-cli/tests/e2e.rs`）が、敵対的な label を返す mock agent の pane を
孤児として報告させ、human 出力に生の `ESC` も `CR` も無いこと・ペイロードが消えていないこと・
panes の行が 1 行のままであること・`--json` の `detail` が label をそのまま含むことを検証する。

ログ層は `logging::layer` のユニットテスト
`human_stream_escapes_external_text_while_json_keeps_it_verbatim` が、同じ入力に対して
human 形式は無害化され JSON 形式は不変であることを 1 つのテストで並べて固定する。

**どのテストも `safe()` を no-op に潰すと実際に FAILED になることを確認してからマージしている**
（#280 / #297）。修正が無くても通るセキュリティテストは無価値なので、この経路を触るときは
同じ確認をすること。
