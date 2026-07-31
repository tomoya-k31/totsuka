---
type: Decision
title: ADR-0028 totsuka setup は対話ウィザードにし、機密は一切扱わない
description: "init が全行コメントの雛形しか書かず config を手書きするしかなかった問題に対し、対話ウィザード totsuka setup を追加する決定。init は非対話・CI 用として残す。既存の設定ファイルは上書きせずスキップし、全行コメントの雛形だけを未設定として扱う。setup は機密の値を一切扱わず参照だけを書いて登録コマンドを印字する。宣言ファイル駆動・SecretWriter ポート・setup --repair・doctor --fix は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/348
tags: [decision, cli, setup, onboarding, secrets, adr]
generated: { by: claude-code/opus-5, at: 2026-08-01T09:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[#348](https://github.com/tomoya-k31/totsuka/issues/348) の実装とともに確定した。エピック [#342](https://github.com/tomoya-k31/totsuka/issues/342)（インストール・セットアップの摩擦をゼロにする）の設定層の中核で、配布層（[ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md) と #344〜#346）の上に載る。

[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)（シークレットバックエンドと非対話原則）を前提とし、変更しない。

# Context

`totsuka init` が生成する `config.toml` は**全行がコメント**である。ディレクトリと雛形を作るだけで、`[[repositories]]` / `[plugins.*]` / `[[workflows]]` / `[llm]` を手で書かなければ何も動かない。

手で書くのが難しいのは記法ではなく**意味論**のほうだ。`trigger` / `mode` / `output` / `verification` の組み合わせは、[設定リファレンス](/development/config-reference.md)と[設定例](/development/config-examples.md)を読まないと決められない。つまり「設定ファイルの書き方が分からない」のではなく「どの組み合わせが自分のやりたいことなのかが分からない」。

さらに全体の導線が存在しない。手順は README の 5 ステップと `docs/operations/` の 4 ランブックに散っており、「ゼロから動くまで」を通しで示すものがなかった。

# Decision

## 1. 対話ウィザード `totsuka setup` を新設し、`init` は残す

|  | `init` | `setup` |
|---|---|---|
| 対話 | しない（絶対） | する（hidden `--answers` で非対話） |
| 用途 | CI・最小ブートストラップ | 人間の導入 |
| 書くもの | XDG ディレクトリ + コメント雛形 | 同上 + 実際の値 |
| 既存ファイル | スキップ | スキップ |
| 機密 | 触らない | **触らない** |

`init` の非対話性は CI が依存する契約なので変えない。`setup` は内部で `init_cmd::ensure_dirs` を呼ぶので、**新マシンで `init` を先に打つ必要はない**。

## 2. レシピを選ばせ、穴だけ聞く

`[[workflows]]` を 1 項目ずつ自由入力で聞いても、結局ドキュメントを読まないと答えられない。それでは対話にする意味がない。そこで [設定例](/development/config-examples.md) のシナリオ別レシピを選択肢として提示し、選んだレシピが要求する穴（リポジトリのパス、Slack のメンバー ID、LLM の model）だけを聞く。`trigger` / `mode` / `output` / `verification` はレシピが持つ。

レシピはコード内のデータ（`setup::recipes`）として持ち、`plugin.toml` やドキュメントから読まない。テストが「各レシピが生成する config は実スキーマを通り `validate` が clean」を固定するので、レシピの追加は必ず検証される。

## 3. interview（純粋）→ 計画表示 → apply（冪等）の 2 フェーズ

インタビューは何も書かない。回答をメモリに組むだけなので、**質問中に Ctrl-C しても痕跡が残らない**。計画を印字して 1 回だけ確認を取り、そこから先だけが副作用を持つ。

apply の各ステップは冪等にし、途中で失敗したらどこまで適用したかを印字する。**原子性は諦めて収束性を取る** — 再実行すれば揃う、を保証する。`config.toml` は `.new` に書いて `rename` する（`commit_install` と同じステージング作法）。

## 4. 既存ファイルはスキップする。ただし「全行コメント＝未設定」は例外

上書きも差分マージもしない。ユーザーが手で書いたものは触らない。

**唯一の例外が `init` の吐いた雛形**である。「ファイルが存在したらスキップ」を素直に実装すると、ドキュメントの指示どおり `init` を先に打った人は `setup` が永久に何もしない状態になる。判定は「全行が空行かコメント」で、実キーが 1 つでもあれば手を触れない。

## 5. `setup` は機密の値を一切扱わない

バックエンド（Keychain / 1Password / 環境変数）を**一度だけ**選ばせ、個々の参照名は規約値（`keychain:totsuka/slack-user` 等）で自動生成する。値そのものはこのプロセスを通らない。最後に登録コマンドのチェックリストを印字し、実体があるかの検証は `doctor` に委ねる。

参照名を聞かないのは、名前は `config.toml` と印字するコマンドの間で一貫していればよく、命名規則の発明はプロンプトを 1 つ使うほどの決定ではないため。

## 6. 非対話経路は hidden の `--answers` に限る

`stdin` が TTY でなく `--answers` も無ければ **exit 2（usage）** で止める。パイプから既定値を黙って採用することはしない — 誰も選んでいない config を書くことになる。

`--answers` を用意する理由は再現性ではなく**テスト可能性**である。CLI の E2E は `totsuka` を子プロセスとして起動するので端末がなく、これが無いと「端末が無いときに正しく落ちる」しか書けず、**「書いた config が実際に読み込める」が一切検証されないまま**になる。`--help` には出さない。

# 検討した選択肢

## 宣言ファイル駆動を主軸にする（対話は補助）

不採用。dotfiles に回答ファイルを置いて再生する運用は再現性で勝るが、初回の導入体験としては対話のほうがよい。再現性は「既存設定はスキップ」の性質（再実行が収束する）と、配布層で入った `plugin install --bundled` / `--from-source` の 1 コマンド化でおおむね代替できる。hidden `--answers` は残るので、必要な人は使える。

## `SecretWriter` ポートを新設し、setup が Keychain へ書く

不採用。`ports/secret.rs` の `SecretStore` は「Orchestrator は読むだけ」（F-65）を doc コメントで明示的な契約にしている。ウィザードだけがそれを破る唯一の場所になるのは避けたい。加えて `op://` は `op read` で読むだけなので 1Password 派には書き込みを提供できず、バックエンドによって体験が割れる。

機密を扱わない判断の副作用として、回答ファイルは**トークンを含みえない**（`deny_unknown_fields` がそれを強制する）。dotfiles に置いても安全である、という性質はこの判断から出ている。

## `setup --repair` / `doctor --fix`

不採用。「既存設定はスキップ」の副産物として、**設定済みマシンでの `setup` 再実行が事実上の repair になる**（config はスキップされ、プラグイン導入と doctor だけが走る）。別フラグを作ると、再実行との違いを説明する責任だけが増える。

`doctor --fix` を採らない理由は別にもある。残っている fail のほとんど（未登録のシークレット、`op://` が解決できない、孤児 pane）は人間の入力か判断を要し、機械的には直せない。直せるものは `doctor` が既に無条件で直しているので、`--fix` はほぼ no-op のフラグになり、名前が実態より多くを約束する。`doctor` は exit code 3 でスクリプトから読まれるコマンドでもあり、検査と修復は分けておきたい。

## 回答ファイルを正規機能として公開する

不採用。`--help` に載せると「宣言的セットアップ」という第 2 の使い方を約束することになり、対話を本体とする方針とぶつかる。テストのための経路である、という位置づけを hidden で表明する。

# Consequences

## 良くなること

- 新マシンで `totsuka setup` を完走すれば、`config validate` を通る `config.toml` ができる。ドキュメントを読みながら手で書く工程が消える
- `init` を先に打っていても `setup` が続きを引き受ける
- 生成物が実スキーマと `validate` を通ることがテストで固定される。レシピを足すときも同じテストが効く
- 機密がプロセスを通らないので、回答ファイルを dotfiles に置ける

## 受け入れるコスト・リスク

- **レシピは「よくある形」でしかない。** 5 番目のシナリオ（複数リポジトリ + 並列制御）のように、リポジトリごとの `max_concurrency` や `tool` を要するものは表現していない。ウィザードは出発点を作るだけで、細かい調整は手編集に戻る
- **対話部分は CLI の E2E で覆えない。** 端末が無いため、プロンプト自体の挙動は `setup::interview` の単体テスト（reader/writer 注入）でしか検証されない
- `plugins/<name>.toml` の生成とプラグイン導入・doctor 実行は本 ADR の範囲外で、[#349](https://github.com/tomoya-k31/totsuka/issues/349) が引き受ける。それまで `setup` の最後は「次にこれを実行」と案内して終わる

# 実装

- `crates/orchestrator-cli/src/setup/`（`mod` / `interview` / `recipes` / `answers`）
- `crates/orchestrator-cli/src/init_cmd.rs` — `ensure_dirs` の切り出しと案内文
- `crates/orchestrator-cli/src/main.rs` — `Setup` サブコマンド
- 編集そのものは [#347](https://github.com/tomoya-k31/totsuka/issues/347) の `config::edit` ヘルパが行う
