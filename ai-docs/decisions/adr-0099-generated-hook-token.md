---
type: Decision
title: ADR-0099 hook の Bearer トークンは run が生成して 0600 のファイルに保存し、[hooks].auth_token_ref を廃止する
description: 利用者が Keychain などで管理していた hook の Bearer トークン（[hooks].auth_token_ref）は、socket の 0600 と ps -E で読める env のせいで守るものが実質無いのに、解決する箇所だけが増えていた。totsuka run が初回起動時に乱数で生成して $XDG_STATE_HOME/totsuka/hook-token（0600）に保存し、以後は使い回す。focus と doctor はこのファイルを読む。auth_token_ref と TOTSUKA_HOOKS_AUTH_TOKEN_REF は猶予なしで廃止し、書いてあれば「この行を消す」専用のエラーにする。
resource: https://github.com/tomoya-k31/totsuka/issues/785
tags: [decision, hook, security, token, uds, breaking-change, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-26T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-785
    resource: https://github.com/tomoya-k31/totsuka/issues/785
    title: "Issue #785"
---

# Status

stable（[#785](https://github.com/tomoya-k31/totsuka/issues/785)）。#754（ネイティブアプリが機密を持つ構成）の設計から切り出した。[ADR-0004](/decisions/adr-0004-hook-completion-signal.md) の「Bearer（keychain 参照）が第二層」と、[ADR-0094](/decisions/adr-0094-task-control-endpoints.md) の決定 2（認証は `[hooks].auth_token_ref`）のうち、**トークンの出どころ**だけを置き換える。2 層の認証そのものは変わらない（[hook-security](/security/hook-security.md) §1）。

# Context

hook の Bearer トークンは、利用者が Keychain などに作って `[hooks].auth_token_ref` で参照する機密だった（`totsuka setup` は `keychain:totsuka/hook-token` を作らせていた）。しかし、このトークンが守っているものは実質的に無い。

- 別ユーザーからの接続は、socket の 0600 が既に防いでいる（第一層）
- 同じユーザーなら、エージェントの env に入っている `TOTSUKA_HOOK_TOKEN` を `ps -E` で読める

それなのに、トークンを解決する箇所は増え続けていた。`run`・`focus`・`doctor` に加え、`task cancel` / `retry` もソケット経由になれば解決する（ADR-0094）。#754 でネイティブアプリが機密を持つ構成にすると、アプリの外から起動した `focus` や `task` はトークンを解決できなくなる。

# Decision

1. **`run` がトークンを自分で作る**（`hooks::token::load_or_create`）。最初に起動したときに 32 バイトの乱数（`/dev/urandom`）を 16 進にして `$XDG_STATE_HOME/totsuka/hook-token` に 0600 で保存し、以後は使い回す。使い回すのは、`run` を再起動しても、生き残っているエージェントの hook が 401 にならないようにするため。既存ファイルのパーミッションは起動のたびに 0600 に戻す
2. **`focus` と `doctor` はこのファイルを読む**（`hooks::token::read`）。アプリがあってもなくても同じように動くので、URL スキームで迂回する必要も、`focus --secrets-stdin` も要らない。`task cancel` / `retry` がソケット経由になるときも同じファイルを読む
3. **`[hooks].auth_token_ref` と `TOTSUKA_HOOKS_AUTH_TOKEN_REF` を廃止する**（破壊的変更）
   - config に書いてあれば「廃止した → この行を消す。トークンは `run` が作る」という専用のエラーにする。`run` では設定エラーなので exit 4（[ADR-0095](/decisions/adr-0095-run-startup-exit-codes.md)）。`config validate` と `doctor` も同じ文言で報告する
   - 環境変数も「unknown」の警告ではなくエラーにする。以前は意味があった変数なので、黙って無視すると利用者は効いていると思い込む
   - 移行の猶予期間は設けない。1.0 前で、直し方は 1 行消すだけのため
   - エージェントに注入する `TOTSUKA_HOOK_TOKEN` という変数名はそのまま使う。[ADR-0009](/decisions/adr-0009-env-override-whitelist.md) の予約リストにも残す（残さないと、エージェントの中で動く `totsuka` が毎回警告を出す）
4. **`setup` は hook トークン用の項目を作らない**（雛形から `# totsuka:secret hook-token` を消した）。既存の `totsuka/hook-token` の Keychain 項目は自動では消さない。消し方は [config-reference](/development/config-reference.md) の移行手順に書く
5. **ローテーションは、ファイルを消して `run` を再起動する**
6. **`doctor` の `hook-token` チェックはファイルを見る。** 無いのは初回 `run` の前なので正常（ok）。他のユーザーが読めるパーミッションなら fail。これに伴い、「フック対応 agent があるのに未設定」を検出していた `config validate` の警告と doctor の fail（#209）は、未設定という状態自体が無くなったので削除した

# 代替案と不採用理由

| 案 | 不採用理由 |
|---|---|
| 今の方式を続ける | アプリの外から起動した `focus` と `task` がトークンを解決できない |
| Bearer を廃止して 0600 だけにする | 防御の層が 1 つ減り、セキュリティポリシーの改訂が大きくなる |
| 書いてあればその値を使い、無ければ生成する | 利用者が管理するトークンが残り、解決する箇所が増える問題がそのまま残る |
| `run` の起動ごとに作り直す | `run` の再起動をまたいで生き残ったエージェントの hook が 401 になる |

# Consequences

- 利用者が管理する機密が 1 つ減った。`[hooks]` に書くものは `socket_path` / `spool_dir` / `block_retry_limit` だけになった
- `run` は `--dry-run` でないかぎり必ず Bearer 付きで受け付ける。「トークン未設定なら Bearer なしで受け付ける」経路は CLI からは使われなくなった（`HookRuntime::auth_token` の `None` はテスト用に残っている）
- 同じユーザーのプロセスはファイルを読めるので、同じユーザーに対する防御は以前と変わらない（以前も env から読めた）
- `run` の起動中にファイルを消すと、`focus` / `doctor` のトークンが受信側とずれる。`doctor` の `hook-socket` は 401 を「`run` 起動後にファイルが変わった → `run` を再起動」と案内する
- 検証: `hooks::token` の `created_once_then_reused_and_kept_0600`、`schema` の `removed_auth_token_ref_says_to_delete_the_line`、`env_overrides` の `the_removed_auth_token_ref_override_is_an_error`、CLI の `a_leftover_auth_token_ref_says_to_delete_the_line` / `a_missing_hook_token_file_is_not_a_problem` / `a_hook_token_file_readable_by_others_fails_doctor`
