---
type: Decision
title: ADR-0100 run が親プロセスから解決済みの機密情報を stdin で受け取る（secret:<name> と --secrets-stdin）
description: "メニューバーアプリが run を子プロセスとして起動する構成のため、secret:<name> スキームと run --secrets-stdin を足した決定。値は stdin の 1 行目の JSON で受け取り、EOF は待たない。フラグを付けたプロセスは Keychain・op・bw・cmd: を backend を呼ばずに拒否する（プロセス全体で 1 か所の関門）。取得元はアプリが持ち、config には名前だけを書く。env・[secrets] テーブル・アプリへの問い合わせ窓口は却下した。"
resource: https://github.com/tomoya-k31/totsuka/issues/754
tags: [decision, config, secrets, macos, menubar, adr]
generated: { by: claude-code/opus-5, at: 2026-09-26T11:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#754）。層 1 で `secret:` と `run --secrets-stdin`、層 2 で `doctor` / `config validate` の `--secrets-stdin` を入れた。hook トークンを利用者の機密から外す件は #785 に切り出した。

# Context

ネイティブ macOS メニューバーアプリが `totsuka run --watch` を子プロセスとして起動・監視する構想がある。この構成では、機密情報をアプリがまとめて持つ（Keychain、GitHub の OAuth Device Flow、1Password の SDK）。一方 `run` は `config.toml` の参照を自分で解決していたので、GUI から起動すると次の理由で動かない:

- GUI から起動したプロセスには TTY が無く、`op` の CLI 連携（tty 単位のセッション）が使えない
- ad-hoc 署名は Keychain の上でビルドごとに別のアプリとして扱われる。手元の `totsuka` の designated requirement は `cdhash H"…"`（バイナリのハッシュそのもの）で、項目ごとの確認ダイアログがビルドのたびに出直す

値を env で渡すことはできない。同じユーザーの `ps -E` で平文のまま見え（macOS 15.7.3 で実測）、しかもエージェントに継承される。

# Decision

1. **`secret:<name>` スキームを足す。** ストアを指さず、親プロセスが渡すマップのキーを指す。名前は `[A-Za-z0-9_.-]+` で、それ以外はパースエラーにする（参照としては認識したまま報告する。リテラル文字列として API に渡さないため）。接頭辞の一覧は従来どおり `parse_scheme` の 1 か所だけにある
2. **`run --secrets-stdin` は、stdin の 1 行目を `{"<name>": "<value>", …}` として読む。** 改行で終わる 1 行を読んだら EOF は待たない
   - 2 行目以降は読まない。常駐中の値の差し替え（#756）で行を足すときに、アプリとの互換を壊さないため
   - 改行より前に EOF が来たら、値が文字列でなければ、stdin が端末なら、起動エラーにする（exit 4、#775）
   - エラーの文言に値は出さない。そのため JSON は汎用の値に一度デコードしてから型を検査する。serde の型不一致の文言は値を引用するため
   - config をロードするより前に読む。`--dry-run` でも読む（読まないとアプリの書き込みがブロックするか EPIPE になる）
3. **フラグを付けたプロセスは、どのストアにも触らない。** 読んだマップを**プロセス全体で 1 つ**の `platform::supplied` に置き、`PlatformSecretStore` は置かれていれば全参照をそこへ回す。そこでは `secret:` をマップから引き、`keychain:` / `op://` / `cmd:` / `bw:` を **backend を呼ばずに** `StoreRefused` で拒否する
   - プロセス全体の状態にしたのは、`plugin_spec` の中の解決器のように、呼び出し元がストアを渡さない経路まで含めて必ず通る関門が `PlatformSecretStore` しかないため。引数で配り回すと、配り忘れた経路がストアを開けられてしまう
   - 判定は解決する時点で行い、config を事前に走査しない。どちらでもストアには触らないので不変条件は同じで、判定が 1 か所に閉じる
   - `${ENV}` は従来どおり展開する（パスなど機密でない値にも使うため）
   - マップにあって使われない名前は無視する。アプリは config を読まずに手持ちを全部渡せる
4. **フラグなしで `secret:` を解決しようとしたら `NotSupplied`**（「`--secrets-stdin` で渡すか、別のスキームを使う」）。`run` では exit 4
5. **`doctor` と `config validate` も `--secrets-stdin` を受け付ける。** 値を持っているのはランチャーだけなので、ランチャーの利用者がプラグインを診断できる経路はこれしかない
   - 付けたときは `run` と同じくプロセス全体に値を置く。`doctor` の `SecretScheme::of` は `secret:` を `Silent`（メモリ上のマップを読むだけでプロンプトは出ない）に分類するので、全チェックが実際の値で走り、ストアは開かない
   - 付けないときは解決しない。`doctor` は `SecretScheme::Supplied` として `cmd:` と同じく常にゲートし、`config validate` はテーブルに `secret:` を含むプラグインのオンライン検査を `note:` 付きで飛ばす。値はそのプロセスには無いので、解決させると正しい config を失敗として報告することになる
   - ADR-0065 は「`doctor` を定期実行すると `op://` を解決してしまう」ことを理由にポーリングを却下したが、フラグを付けた `doctor` はストアに触れないので、その理由は当てはまらない
6. **取得元はアプリが持つ。** config には `secret:<name>` だけを書き、どの値をどこから取るかはアプリの設定と保管庫の話にする。既存の `op://` / `keychain:` / `cmd:` / `bw:` / `${ENV}` はターミナルから単独で使う人のために全部残す
7. **`[tools.X].env_file` は変えない。** 値に `secret:<name>` を書けば共通の解決器で解決される
8. プロトコルは変えない。プラグインは解決済みの値を受け取り、それがどこから来たかは知らない（F-65）

# 代替案と不採用理由

- **env で渡す** — 同じユーザーの `ps -E` で平文のまま見え、エージェントに継承される
- **stdin を EOF まで読む** — 実装の手間は同じだが、#756 で差し替えの行を足すときに受け渡しの形を変えることになる
- **2 行目以降の更新プロトコルまで今決める** — プラグインへ値を渡し直す仕組み（#756 の本題）が決まる前に、受け口だけを作ることになる
- **`config.toml` に名前付きの `[secrets]` テーブルを作り、インライン参照を廃止する** — 利点は「アプリとターミナルで同じ config が使える」ことだけで、アプリから起動するときに Keychain の参照を使う場面は無い。代わりに、アプリが totsuka の参照の文法（`cmd:` と `bw:` まで）を解決する実装をもう 1 つ持つことになる
- **フラグなしのプロセスがアプリの UDS に値を問い合わせる** — 同じユーザーのどのプロセスでも、全ての機密を引き出せる窓口になる。env を避けた動機と矛盾する
- **混在を許す（フラグを付けても `op://` などを解決する）** — GUI の下で `op` や Keychain のプロンプトが突然出る。この決定が消したい状態そのもの
- **config を事前に走査して拒否する** — ストアに触らない点は同じで、走査する処理をもう 1 本持つことになる。代償として、`[llm]` などでの違反はプラグインを起動した後で見つかるが、従来の「解決に失敗したら止める」と同じタイミングである

# Consequences

- アプリは `totsuka run --watch --secrets-stdin` を起動し、1 行を書いて stdin を開けたままにしておけばよい。起動時の機密のエラーは exit 4、lock の競合は exit 5 で見分けられる
- フラグを付けた `run` のプロセスは Keychain・1Password・Bitwarden・`cmd:` のどれも開かない。これはテストで、`cmd:` がマーカーファイルを作らないことで確かめている
- **dry run もプラグインを起動し、そのテーブルを解決する**（解決しないのは `env_file` だけ、ADR-0090）。したがって dry run でも、プラグインのテーブルが使う名前は渡す必要がある
- アプリへの申し送り: ログインシェル相当の `PATH` を渡すこと。`claude`・`herdr` / `orca`・`git`・`gh`、Slack の gateway モードの `gcloud` は、どれも `PATH` から探される
- 関連: [ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)（`op://`）、[ADR-0044](/decisions/adr-0044-cmd-secret-scheme.md)（`cmd:`）、[ADR-0076](/decisions/adr-0076-bitwarden-secret-backend.md)（`bw:`）、[ADR-0090](/decisions/adr-0090-tools-env-file.md)（`env_file`）
