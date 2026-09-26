---
type: Playbook
title: セットアップ Playbook（新マシン / 開発機 / ローテーション / 復旧）
description: "ゼロから totsuka が動くまでを通しで示す導入手順。新マシン（tarball 配置 → totsuka setup でプラグイン選択と config.toml 生成 → config.toml を編集 → シークレット登録 → doctor → run）、開発機（クローン → チェックアウトからのビルド）、トークンローテーション、中断・失敗時の復旧と別マシンでの再現を扱う。"
resource: https://github.com/tomoya-k31/totsuka/issues/350
tags: [setup, onboarding, runbook, playbook, secrets, doctor, rotation]
generated: { by: claude-code/opus-5.5, at: 2026-09-27T10:00:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/setup-playbook.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/setup-playbook.md docs/setup-playbook.ja.md -->

# このドキュメントの位置づけ

「ゼロから動くまで」を**通しで**示す唯一の場所。個別の話題は既存のランブックが持っており、ここはそこへの導線を兼ねる。

| 知りたいこと | 行き先 |
|---|---|
| 各設定キーの意味 | [設定リファレンス](/development/config-reference.md) |
| シナリオ別の config 例 | [設定例](/development/config-examples.md) |
| Slack アプリの作成と scope | [Slack Quickstart](/operations/slack-quickstart.md) |
| doctor の読み方・worktree 掃除 | [運用ガイド](/operations/operations-guide.md) |
| プラグインを自作する | [プラグイン開発ガイド](/development/plugin-dev-guide.md) |

前提として macOS。`totsuka setup` の設計判断は [ADR-0077](/decisions/adr-0077-setup-writes-the-whole-surface.md)（[ADR-0028](/decisions/adr-0028-setup-wizard.md) を置き換えた）。

# 新マシン

## 1. 配置

```bash
brew install tomoya-k31/tap/totsuka
```

これで終わり。`sudo` も、ツリーの手配置も、`xattr` も要らない（[ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md)）。**同梱プラグインは formula が `libexec/totsuka/plugins` へ置き**、`totsuka` の探索順（`<exe dir>/../libexec/totsuka/plugins`）がそこに当たるので、`setup` はパス指定なしでプラグインを入れられる。

trust について: Homebrew はサードパーティ tap に trust を要求するが、**formula を名指しすれば同じコマンドの中で付与される**。`==> Trusted formula tomoya-k31/tap/totsuka` の 1 行が出て進むだけで、**答えるべきプロンプトは無い**（対話・非対話とも実測）。

更新は `brew upgrade totsuka`。

### Homebrew を使わない場合

[最新リリース](https://github.com/tomoya-k31/totsuka/releases/latest) の macOS ユニバーサル tarball を落とす。**ツリーごと**置くこと — `totsuka` は同梱プラグインを自分の隣から探すので、バイナリだけ移すと `setup` がプラグインを見つけられない。

```bash
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

**この経路でだけ `xattr` が要る。** ブラウザでダウンロードすると `com.apple.quarantine` が付き、Gatekeeper が**プラグインの起動だけ**を黙って殺す。`doctor` は「crashed or exited」としか言えず、本体は動くので原因が見えにくい。brew 経路では素の `curl` が取るので、この属性は付かない（実測）。

## 2. `totsuka setup`

```bash
totsuka setup
```

**聞かれるのは 1 問だけ** —— 使うプラグインの複数選択（矢印キーで移動、スペースで選択、Enter で確定）。
それ以外は聞かない。`setup` は選んだプラグインを導入し、**設定面すべてをコメントで書いた `config.toml`** を
置いて、そのパスと「編集しろ」を案内して終わる。

非対話で回すなら:

```bash
totsuka setup --plugins github,herdr,macos     # あるいは --plugins all / --plugins none
totsuka setup --plugins all --secret-backend bw
```

**TTY が無く `--plugins` も無い場合はエラーで止まる。** 既定の選択を置くと選んでいないプラグインが入り、
空にすると成功した実行と見分けがつかないため。綴り間違い（`--plugins gihub`）も黙って無視せずエラーになる。

`--secret-backend` は `op`（既定）/ `bw` / `keychain` / `cmd` / `env`。**選んだ形が config.toml の参照行にも入る**
（端末の登録コマンドだけ切り替えると、`op://` だらけのファイルの上で `security add-generic-password` を
案内することになる）。参照行の直上には他 4 種の書式が併記されるので、後から乗り換えるのにドキュメントは要らない。

生成されたファイルは **`version = 1` の 1 行以外すべてコメント**である。つまり `totsuka config validate` は
その場で通るが、**まだ何も動かない**。最低限これだけは自分で外す:

1. `[[repositories]]` —— タスクを流し込むローカルクローン
2. 入れたプラグインの `[plugins.<name>] enabled = true`
3. そのプラグインの `[<name>]` テーブル（トークン参照を含む）
4. `[[projects]]` と `[[workflows]]` —— **ファイル末尾のレシピ集**に、コメントを外せばそのまま動く組み合わせが 4 つある

**プラグインは導入されるが有効化されない。** `totsuka config validate` は enabled なプラグインを実際に起動するので、
`[github].token` がまだコメントのまま `github` を有効にすると、セットアップを確認するためのコマンドが
そのセットアップ自身で落ちる。

**既に `config.toml` があれば、足りない節だけが追記される。** 既存の行は 1 バイトも変わらない。
後から notion を使いたくなったときは `totsuka setup --plugins notion` を打てば、`[notion]` の
コメント付き雛形がファイル末尾に足される。

## 3. シークレットを登録する

`setup` の最後に、**選んだプラグインが参照するシークレットの一覧**が出る。各行が「参照名」「何を可能にするか」
「登録コマンド」を持つので、そのままコピペする:

```bash
security add-generic-password -U -s totsuka -a github-token -w '<paste the value>'
```

**登録するのは、実際にコメントを外した行のぶんだけでよい。** 一覧は config.toml が*言及している*参照を全部出すが、
コメントのままの行は解決されない。逆に、コメントを外した参照が未登録ならそのプラグインは起動しない。

`--secret-backend cmd` と `env` には登録コマンドが無い（値は別のツールと環境が持つ）ので、代わりにその旨が出る。

Bitwarden を選んだ場合、**先に `bw login` → `bw unlock` を済ませ、表示された `BW_SESSION` を export しておくこと**。
登録コマンドは vault を書き換えるので、アンロック済みのセッションが無いと実行できない（下の「一回きりの対話セットアップ」に
同じことが書いてあるが、ここでの手順がそれより前に来るため再掲する）。登録コマンドは
`bw get template item | jq … | bw encode | bw create item` の形になる（`jq` が要る）。`bw` に `op item edit` 相当の
1 行が無いためで、**このコマンドは常に新規作成する**。同名のアイテムが既にあるなら作らずそちらを編集すること ——
重複すると `bw get` が「複数ヒット」で失敗し、参照が解決できなくなる。アカウントごとに 1 アイテム
（`bw:totsuka-<name>/password`）にするのは**運用上の取り決め**であって、Bitwarden の制限ではない ——
1 アイテムは `username` / `password` / `uri` / `totp` を持てる。ただし `bw:` はカスタムフィールドに届かないので、
任意個の秘密を 1 アイテムに詰めることはできず、`setup` が案内するのはどれもトークン（= `password`）なので、
結果として 1 つずつになる。

> Slack で本人名義の返信を使うなら `slack-bot` も登録すること。プラグイン単体では opt-in（無ければナッジ無し）だが、
> **本人名義の返信は Slack 通知を一切上げない**ので、ナッジが無いと返信案が来たことに気づけない
> （[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)）。

## 4. 検証して走らせる

**config.toml を編集してから**回す。`setup` は `doctor` を自動実行しない —— 編集前の config は実質空なので、
「まだ何も設定されていない」を報告するだけになるため。

```bash
totsuka config validate # 設定がパースでき、辻褄が合っている
totsuka doctor          # 未登録シークレットが残っていれば exit 3 で教える
totsuka run --dry-run   # どのタスクがどのリポジトリのどのエージェントに行くか
totsuka run --watch
```

`doctor` の `state-db` は `totsuka run` を一度も実行していなければ fail する。これは正常で、`run` 後に消える。

## 5. 一回きりの対話セットアップ（該当するときだけ）

`setup` が代行できない、対象ツール側の初回操作。

| 対象 | 必要な操作 | 参照 |
|---|---|---|
| Codex | TUI で **hooks trust** を承認。**しないとフックが黙ってスキップされ、全タスクが timeout する** | [Codex ツールのセットアップ](/operations/codex-tool-setup.md) |
| OpenCode | 初回起動と config 配置 | [OpenCode ツールのセットアップ](/operations/opencode-tool-setup.md) |
| 1Password | `op signin`（`op://` 参照を使う場合） | [ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) |
| Bitwarden | `bw login` → `bw unlock` し、表示された `BW_SESSION` を export（`bw:` 参照を使う場合）。**`totsuka run` はその export をしたシェルから起動する** —— `bw` は常駐セッションを持たないので、セッションが無いとマスターパスワードを標準入力から訊き、常駐プロセスは画面に何も出ないまま止まる | [ADR-0076](/decisions/adr-0076-bitwarden-secret-backend.md) |
| 通知クリック | `terminal-notifier` の導入と bundle id | [click-to-focus セットアップ](/operations/click-to-focus-setup.md) |

# 開発機

チェックアウトからビルドして入れる。tarball は要らない。

```bash
git clone https://github.com/tomoya-k31/totsuka
cd totsuka
cargo build --release --workspace --bins
totsuka setup --plugins all
```

`totsuka setup` をチェックアウト内で打つと、同梱ツリーが無い場合は自動でチェックアウトからのビルドを選ぶ。探索は cwd から上へ「Cargo ワークスペースのルート**かつ** `plugins/` を持つ」ディレクトリを辿るので、別リポジトリの中で打っても誤検出しない。

プラグインを 1 つ直したときの再導入も同じ経路:

```bash
totsuka plugin install --from-source slack --enable
```

`--print-plan` を付けると cargo を起動せず、何をビルドしてどこから入れるかだけ印字する。

# トークンローテーション

## Slack — scope を変えたら 2 本とも再発行される

**これが一番踏みやすい。** Slack アプリの scope を変更すると再インストールが必要になり、`xoxp-`（User）と `xoxb-`（Bot）が**両方**新しくなる。片方だけ保管先を更新すると、更新しなかった側の機能だけが壊れる:

```bash
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'
```

`xapp-`（App-Level Token）は再インストールでは変わらない。明示的に再生成したときだけ更新する。

scope 自体の落とし穴として、`reactions:read` / `channels:read` / `groups:read` が欠けると**イベントが届かないだけでエラーも出ない**。詳細は [Slack Quickstart](/operations/slack-quickstart.md)。

## hook トークン — 登録しない

hook の Bearer トークンは `totsuka run` が初回起動時に `$XDG_STATE_HOME/totsuka/hook-token`（0600）へ生成し、以後使い回す
（[ADR-0099](/decisions/adr-0099-generated-hook-token.md)）。ローテーションはこのファイルを消して `run` を再起動するだけ。

## 全般

`setup` を再実行する必要はない。参照名は変わっておらず、変わったのは値だけなので、`security add-generic-password -U`（`-U` = 既存を更新）で上書きして `totsuka doctor` を打てばよい。

# 中断・失敗時の復旧

## 途中で失敗した

**再実行すれば揃う。** `setup` は原子性ではなく収束性を保証しており、各ステップが冪等になっている。どこまで適用したかは印字される。

```bash
totsuka setup --plugins <同じ選択>
```

既存の `config.toml` からは足りない節だけが追記されるので、2 回目は実質「プラグイン導入だけ」が走る。
これが `--repair` フラグを用意していない理由（[ADR-0077](/decisions/adr-0077-setup-writes-the-whole-surface.md)、
判断自体は [ADR-0028](/decisions/adr-0028-setup-wizard.md) からの引き継ぎ）。

## 設定を作り直したい

`setup` は既存ファイルの行を書き換えない。まっさらにするなら自分で退避する:

```bash
mv ~/.config/totsuka/config.toml{,.bak}
totsuka setup --plugins all
```

**節を足すだけなら退避は要らない。** 後から使いたくなったプラグインは `totsuka setup --plugins <name>` で、
その `[<name>]` のコメント付き雛形がファイル末尾に足される。既存の行は 1 バイトも変わらない。

## 同じ設定を別マシンで再現したい

**`config.toml` そのものを持っていく。** `setup` は機密の**値**を一切書かないので（書くのは
`op://…` / `keychain:…` のような*参照*だけ）、そのファイルは dotfiles に置いても安全である。

```bash
cp ~/.config/totsuka/config.toml ~/dotfiles/totsuka-config.toml
```

読み込む側:

```bash
totsuka setup --plugins <同じ選択>     # ディレクトリ作成とプラグイン導入
cp ~/dotfiles/totsuka-config.toml ~/.config/totsuka/config.toml
```

シークレットの登録だけは各マシンで人間がやる。リポジトリのパスがマシンごとに違うなら
`[[repositories]].path` を直すこと。

**マシンごとに中身が違うなら `hosts/<host>.toml` に分ける**（#832）。`~/.config/totsuka/hosts/<host>.toml`
があればそのマシンでは `config.toml` の代わりにそれが読まれるので、全マシンぶんを 1 つの dotfiles で管理できる。
`<host>` はホスト名の最初の `.` より前の小文字（`M2.local` → `m2`）:

```bash
mkdir -p ~/.config/totsuka/hosts
mv ~/.config/totsuka/config.toml ~/.config/totsuka/hosts/"$(hostname -s | tr '[:upper:]' '[:lower:]')".toml
totsuka doctor    # config-file 行が hosts/<host>.toml を指していること
```

選択順と注意点は [設定リファレンス](/development/config-reference.md) の「config ファイルの選択」。

> 以前あった回答ファイル（`--save-answers` / `--answers`）は #705 で無くなった。運ぶべきものが
> 「回答」から「設定ファイルそのもの」に変わり、中間形式が要らなくなったため。

## `doctor` が赤いまま

読み方は [運用ガイド](/operations/operations-guide.md)。導入直後に出やすいものだけ:

| チェック | よくある原因 |
|---|---|
| `state-db` | まだ `totsuka run` を打っていない（正常） |
| `plugin:<name>` — secret not found | チェックリストの登録漏れ |
| `plugin:<name>` — crashed or exited | `xattr -dr com.apple.quarantine` の実行漏れ |
| `bundled-plugins`（warning） | `cargo install` 由来のビルドで同梱ゼロ。`--from-source` を使う |
| `hook-token`（fail） | `run` が生成したトークンファイル `$XDG_STATE_HOME/totsuka/hook-token` を他のユーザーが読める。消して `totsuka run` を再起動する（無いのは初回 `run` 前なので正常） |
| `config` — `[hooks].auth_token_ref was removed` | 0.9 までの設定が残っている。その行を消す（hook トークンは `run` が作る。[config-reference](/development/config-reference.md) の「移行」） |
