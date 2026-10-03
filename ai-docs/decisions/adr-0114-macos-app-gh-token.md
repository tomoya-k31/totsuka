---
type: Decision
title: ADR-0114 メニューバーアプリは github.token の secret を起動のたびに gh auth token から取れる
description: "メニューバーアプリ（ADR-0113）が --secrets-stdin で run を起動する構成で、[github].token の secret: を PAT の発行なしに埋めるため、入力ダイアログに「Use gh auth token」を足す決定。選ぶと UserDefaults に印だけを置き、Start のたびに gh auth token --hostname <api_url のホスト> --user <github_login> を実行して map に入れる（Keychain には保存しない）。gho_ に有効期限は無いが 10 本の上限・承認の取り消し・1 年未使用で失効するので、コピーは持たない。CLI は従来どおり cmd:gh auth token を使う。OAuth device flow・値のコピー・--secrets-stdin で cmd: を許す案は却下した。"
resource: https://github.com/tomoya-k31/totsuka/issues/858
tags: [decision, macos, menubar, secrets, github, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-03T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: gh-token-revocation
    resource: https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/token-expiration-and-revocation
    title: GitHub Docs — Token expiration and revocation
---

# Status

stable。#858 で実装した。

# Context

CLI から `totsuka run` を起動するなら `[github].token = "cmd:gh auth token"` で足りる。困るのはメニューバーアプリ（[ADR-0113](/decisions/adr-0113-native-menubar-app.md)）から起動する場合である。

- config に `secret:` が 1 つでもあると、アプリは `run --secrets-stdin` で起動する。このモードは `cmd:` を含むストアの参照をすべて拒否する（[ADR-0100](/decisions/adr-0100-secrets-stdin.md)）
- したがって Slack などを `secret:` にしていると、`[github].token` も `secret:<name>` にするしかなく、アプリはその値を入力ダイアログで尋ねる
- そこで貼れるものは PAT しかなく、発行（scope の選択、有効期限、保存）が面倒だった。fine-grained PAT は user 所有のボードにそもそも届かない（#514）

`gh auth token` が返す `gho_` トークンには有効期限が無い（`gh api -i user` に `github-authentication-token-expiration` ヘッダが付かないことを実測）。ただし GitHub は次の場合に失効させる[^gh-token-revocation]: 1 年間使われない、同じユーザー・アプリ・scope で 10 本を超える（`gh auth login` のやり直しで起こる）、公開リポジトリや gist に push される、承認が取り消される。

# Decision

1. **入力ダイアログに「Use gh auth token」を足す。** `[github].token` が参照している `secret:<name>` を尋ねるときだけ出す（`githubTokenSecretName`）。他の secret には出さない
2. **選ばれたら UserDefaults の `githubTokenFromGh` に `true` を置くだけ。** Keychain の map には何も入れない（番兵値を map に入れると、取り除き忘れたときにそのまま `run` へ渡るため）。印が立っている間は、その名前をダイアログで尋ねない
3. **Start のたびに `gh auth token --hostname <host> --user <github_login>` を実行し、map の `<name>` を上書きして `config validate` と `run` に渡す。** 保存はしない。失敗後の自動再起動も Start を通るので、そのたびに取り直す。印があれば map の同名の値より gh を優先する
   - `--user` は `[github].github_login` から取る。`gh auth switch` で別のアカウントのトークンが渡るのを防ぐ
   - `--hostname` は `[github].api_url` のホストから決める（`ghHostname`）: `api.github.com` → `github.com`、`api.<sub>.ghe.com` → `<sub>.ghe.com`、それ以外（GHES）はホストそのもの
4. **決められないものは推測しない。** `api_url` が http(s) の URL として読めない（`${ENV}` や `secret:` を含む）、または `github_login` が空・参照のときはボタンを出さない。印が既にある場合は、Start で起動せずに理由を出す
5. **`gh` が無い・非ゼロで終了した・出力が空のときは起動しない。** 状態表示に `gh auth login --hostname <host>` の案内と stderr の先頭行を出す。stdout（トークン）はどこにも出さない。印は残すので、gh にログインすれば次の Start でそのまま動く
6. **「Forget saved secrets…」は印も消す。** 次の Start でダイアログが再び出るので、そこで値の入力に切り替えられる
7. gh の探索と実行は既存の部品を使う。`locateTotsuka` を `locateExecutable(named:)` に一般化し、`TotsukaCLI` を gh のパスで作ってログインシェルの環境で実行する

# 代替案と不採用理由

- **ダイアログで一度 `gh auth token` を実行し、値を Keychain に保存する** — 実装は一番小さいが、Context の条件で失効すると黙って認証エラーになり、Forget からやり直すしかない
- **OAuth device flow をアプリ（または CLI の `totsuka auth login github`）に実装する** — gh が要らなくなるが、OAuth App の登録と、org がサードパーティの OAuth App を制限している場合の承認が要る。gh を使えば「GitHub CLI」の承認を流用できる。#858 の最初の設計はこれだったが、CLI は `cmd:gh auth token` で困っていないと分かって取り下げた
- **`--secrets-stdin` でも `cmd:` を許す** — ADR-0100 の「フラグを付けたプロセスはどのストアにも触らない」という 1 か所の関門を崩す
- **印を Keychain の map の予約キーに置く** — map はそのまま `run` に渡るので、渡す前に取り除く処理が要る
- **印を secret の名前で持つ** — config で名前を変えると印が効かなくなる。対象は `[github].token` の 1 つだけなので真偽値で足りる
- **gh を引数なし（アクティブなアカウント）で呼ぶ** — CLI の `cmd:gh auth token` と同じ動きだが、`gh auth switch` で黙って別のアカウントに切り替わる
- **classic PAT の作成画面の URL を案内する** — 発行・期限の管理・貼り付けの手間が残る。それがこの決定の動機である

# Consequences

- `secret:` を使う config でも、gh にログインしていれば PAT を発行せずに済む
- 使える scope は gh のトークンのものになる。**`gh auth login` の既定の scope（`repo, read:org, gist`）には `project` が無い**ので、ボードを読むには `gh auth refresh -s project` で足しておく必要がある。足していないと Start は通るが、ボードの取得で失敗する。アプリはトークンの scope を検査せず、ボタンを出すダイアログの説明文で `gh auth refresh -s project` を案内するにとどめる。必要以上の scope を持つ点は、CLI で `cmd:gh auth token` を使うのと同じである
- `run` の実行中にトークンが失効した場合は扱わない（`--secrets-stdin` は起動時にしか値を読まない）。次の Start で取り直される
- 実機の UI（ダイアログのボタン、gh が見つからない場合の表示）にはテストが無い。純粋な部分（`githubTokenSecretName` / `ghHostname` / `ghTokenArguments`）は `apps/macos/test.sh` で試す
- 関連: [ADR-0100](/decisions/adr-0100-secrets-stdin.md)、[ADR-0113](/decisions/adr-0113-native-menubar-app.md)、[macOS アプリ](/components/macos-app.md)

[^gh-token-revocation]: GitHub Docs — Token expiration and revocation
