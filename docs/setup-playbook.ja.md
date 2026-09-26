> 🌐 [English](setup-playbook.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/setup-playbook.md sha256:41e005da8a030b44df992c395c130fe2089438b8597bcf2fe04b19ca3a6532ba -->

# セットアップ Playbook

ゼロから totsuka が動くまでを通しで示す。新しいマシンへの導入、開発機でのビルド導入、トークンのローテーション、途中で失敗したときの復旧を扱う。

対象は macOS。個別の話題は次を参照。

| 知りたいこと | 行き先 |
|---|---|
| 各設定キーの意味 | [設定リファレンス](config-reference.ja.md) |
| doctor の読み方・worktree 掃除 | [運用ガイド](operations-guide.ja.md) |
| プラグインを自作する | [プラグイン開発ガイド](plugin-dev-guide.ja.md) |

## 新しいマシンに入れる

### 1. 配置する

```bash
brew install tomoya-k31/tap/totsuka
```

これで終わり。`sudo` も、ツリーの手配置も、`xattr` も要らない。同梱プラグインは `libexec/totsuka/plugins` に置かれ、そこは `totsuka` が探す場所のひとつなので、セットアップはパス指定なしでプラグインを入れられる。

Homebrew はサードパーティ tap に trust を要求するが、formula を名指しすれば同じコマンドの中で付与される。`==> Trusted formula tomoya-k31/tap/totsuka` の 1 行が出て、そのまま進む。答えるべきプロンプトは無い。

更新は `brew upgrade totsuka`。

#### Homebrew を使わない場合

[最新リリース](https://github.com/tomoya-k31/totsuka/releases/latest) の macOS ユニバーサル tarball を落とす。**ツリーごと**置くこと — `totsuka` は同梱プラグインを自分の隣から探すので、バイナリだけ移すとセットアップがプラグインを見つけられない。

```bash
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

**`xattr` が要るのはこの経路だけ。** ブラウザでダウンロードすると `com.apple.quarantine` が付き、Gatekeeper が**プラグインの起動だけ**を黙って止める。本体は動くので原因が見えにくく、`doctor` は「crashed or exited」としか言えない。Homebrew は素の `curl` で取るので、この属性は付かない。

### 2. `totsuka setup` を走らせる

```bash
totsuka setup
```

**聞かれるのは 1 問だけ** —— 使うプラグインの複数選択（矢印キーで移動、スペースで選択、Enter で確定）。それ以外は聞かれません。選んだプラグインを導入し、**totsuka が解釈する設定を全部コメントで書いた `config.toml`** を置いて、そのパスを教えて終わります。

非対話で回すなら:

```bash
totsuka setup --plugins github,herdr,macos     # あるいは --plugins all / --plugins none
totsuka setup --plugins all --secret-backend bw
```

**TTY が無く `--plugins` も無い場合はエラーで止まります。** 既定の選択を置くと選んでいないプラグインが入り、空にすると成功した実行と見分けがつかないためです。綴り間違い（`--plugins gihub`）も同じ理由でエラーになります。

`--secret-backend` は `op`（既定）/ `bw` / `keychain` / `cmd` / `env`。**選んだ形は `config.toml` の参照行にも入ります**（端末に出る登録コマンドだけ切り替えると、`op://` だらけのファイルの上で `security add-generic-password` を案内することになり、何も読まないシークレットを登録させてしまいます）。参照行の直上には他 4 種の書式が併記されるので、後から乗り換えるのにドキュメントは要りません。

生成されるファイルの**有効行は `version = 1` の 1 行だけ**で、残りは全部コメントです。したがって `totsuka config validate` はその場で通りますが、**まだ何も動きません**。最低限これだけは自分でコメントを外します。

1. `[[repositories]]` —— タスクを流し込むローカルクローン
2. 導入したプラグインの `[plugins.<name>] enabled = true`
3. そのプラグインの `[<name>]` テーブル（トークン参照を含む）
4. `[[projects]]` と `[[workflows]]` —— **ファイル末尾のレシピ集**に、コメントを外せばそのまま動く組み合わせが 4 つあります

**プラグインは導入されますが有効化されません。** `totsuka config validate` は有効なプラグインを実際に起動するので、`[github].token` がまだコメントのまま `github` を有効にすると、セットアップを確認するためのコマンドがそのセットアップ自身で落ちます。

**既に `config.toml` があれば、足りない節だけが追記されます。** 既存の行は 1 バイトも変わりません。後から Notion を使いたくなったら `totsuka setup --plugins notion` で、`[notion]` のコメント付き雛形がファイル末尾に足されます。

### 3. シークレットを登録する

`setup` の最後に、**選んだプラグインが参照するシークレットの一覧**が出ます。各行が参照名・何を可能にするか・登録コマンドを持つので、そのままコピペできます。

```bash
security add-generic-password -U -s totsuka -a github-token -w '<値を貼る>'
```

**登録するのは、実際にコメントを外した行のぶんだけで構いません。** 一覧はファイルが*言及している*参照を全部出しますが、コメントのままの行は解決されません。逆に、コメントを外した参照が未登録ならそのプラグインは起動しません。

`--secret-backend cmd` と `env` には登録コマンドがありません（値は別のツールと環境が持つため）ので、代わりにその旨が出ます。

Bitwarden を選んだ場合、**先に `bw login` → `bw unlock` を済ませ、表示された `BW_SESSION` を export しておいてください** —— 登録コマンドは vault を書き換えるので、アンロック済みのセッションが要ります。（下の前提条件の表にも同じことがありますが、この手順が先に来るので再掲します。）コマンドは `bw get template item | jq … | bw encode | bw create item` の形になります（`jq` が要ります）。`bw` に `op item edit` 相当の 1 行が無いためで、**このコマンドは常に新規作成します**。同名のアイテムが既にあるなら作らずそちらを編集してください —— 重複すると `bw get` が「複数ヒット」で失敗し、参照が解決できなくなります。アカウントごとに 1 アイテム（`bw:totsuka-<name>/password`）になるのは運用上の取り決めであって制限ではありません（1 アイテムは `username` / `password` / `uri` / `totp` を持てます）。ただし `bw:` はカスタムフィールドに届かないので任意個の秘密を 1 アイテムに詰めることはできず、`setup` が案内するのはどれもトークン（= `password`）なので結果として 1 つずつになります。

Slack で本人名義の返信を使うなら bot トークンも登録してください。**本人名義の返信は Slack 通知を一切上げない**ので、bot のナッジが無いと返信案が来たことに気づけません。

### 4. 検証して走らせる

```bash
totsuka doctor          # 未登録のシークレットが残っていれば教えてくれる
totsuka run --dry-run   # どのタスクがどのリポジトリのどのエージェントに行くか
totsuka run --watch
```

`doctor` の `state-db` は `totsuka run` を一度も実行していないと失敗する。これは正常で、`run` 後に消える。

### 5. ツール側の初回操作（該当するときだけ）

`setup` が代行できない、対象ツール側の操作。

| 対象 | 必要な操作 |
|---|---|
| Codex | TUI で hooks の信頼を承認する。**しないとフックが黙ってスキップされ、全タスクがタイムアウトする** |
| OpenCode | 初回起動と設定の配置 |
| 1Password | `op://` 参照を使うなら `op signin` |
| Bitwarden | `bw:` 参照を使うなら `bw login` → `bw unlock` し、表示された `BW_SESSION` を export する。**`totsuka run` はその同じシェルから起動する** —— `bw` は常駐セッションを持たず、セッションが無いとマスターパスワードを標準入力から訊くので、常駐プロセスは画面に何も出ないまま止まってしまう |
| 通知クリック | `terminal-notifier` の導入と bundle id の設定 → [click-to-focus セットアップ](click-to-focus-setup.ja.md) |

## 開発機に入れる

チェックアウトからビルドして入れる。tarball は要らない。

```bash
git clone https://github.com/tomoya-k31/totsuka
cd totsuka
cargo build --release --workspace --bins
totsuka setup --plugins all
```

チェックアウト内で `totsuka setup` を打つと、同梱ツリーが無い場合は自動でチェックアウトからのビルドを選ぶ。探索は現在地から上へ「Cargo ワークスペースのルートかつ `plugins/` を持つ」ディレクトリを辿るので、別のリポジトリの中で打っても誤検出しない。

プラグインを 1 つ直したときの再導入も同じ経路。

```bash
totsuka plugin install --from-source slack --enable
```

`--print-plan` を付けると cargo を起動せず、何をビルドしてどこから入れるかだけ表示する。

## トークンのローテーション

### Slack — scope を変えると 2 本とも再発行される

**これが一番踏みやすい。** Slack アプリの scope を変更すると再インストールが必要になり、User トークン（`xoxp-`）と Bot トークン（`xoxb-`）が**両方**新しくなる。片方だけ更新すると、更新しなかった側の機能だけが壊れる。

```bash
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'
```

App-Level Token（`xapp-`）は再インストールでは変わらない。明示的に再生成したときだけ更新する。

scope 自体にも落とし穴があり、`reactions:read` / `channels:read` / `groups:read` が欠けると**イベントが届かないだけでエラーも出ない**。

### hook トークン — 登録しない

hook の Bearer トークンは `totsuka run` が初回起動時に `$XDG_STATE_HOME/totsuka/hook-token`（0600）へ生成し、以後使い回す。ローテーションはこのファイルを消して `run` を再起動するだけ。

### 全般

`setup` を再実行する必要はない。参照名は変わっておらず値だけが変わったので、`-U`（既存を更新）付きで上書きしてから `totsuka doctor` を打てばよい。

## 途中で失敗したときの復旧

### 失敗した

**再実行すれば揃う。** `setup` は各ステップが冪等で、どこまで適用したかも表示される。

```bash
totsuka setup --plugins <同じ選択>
```

既存の `config.toml` からは足りない節だけが追記されるので、2 回目は実質「プラグイン導入だけ」が走る。

### 設定を作り直したい

`setup` は既存ファイルの行を書き換えない。まっさらにするなら自分で退避する。

```bash
mv ~/.config/totsuka/config.toml{,.bak}
totsuka setup --plugins all
```

**節を足すだけなら退避は要らない。** `totsuka setup --plugins notion` は `[notion]` のコメント付き雛形をファイル末尾に足すだけで、自分で書いた行は変えない。

### 同じ設定を別マシンで再現したい

**`config.toml` そのものを持っていく。** `setup` は機密の**値**を一切書かない（書くのは `op://…` / `keychain:…` のような*参照*だけ）ので、そのファイルは dotfiles に置いても安全である。

```bash
cp ~/.config/totsuka/config.toml ~/dotfiles/totsuka-config.toml
```

別マシン側:

```bash
totsuka setup --plugins <同じ選択>     # ディレクトリ作成とプラグイン導入
cp ~/dotfiles/totsuka-config.toml ~/.config/totsuka/config.toml
```

シークレットの登録だけは各マシンで人間が行う。クローンの置き場所が違うなら `[[repositories]].path` を直すこと。

**マシンごとに中身が違うなら `hosts/<host>.toml` に分ける。** `~/.config/totsuka/hosts/<host>.toml` があれば、そのマシンでは `config.toml` の代わりにそれが読まれるので、全マシンぶんを 1 つの dotfiles で管理できる。`<host>` はホスト名の最初の `.` より前の小文字（`M2.local` → `m2`）:

```bash
mkdir -p ~/.config/totsuka/hosts
mv ~/.config/totsuka/config.toml ~/.config/totsuka/hosts/"$(hostname -s | tr '[:upper:]' '[:lower:]')".toml
totsuka doctor    # config-file 行が hosts/<host>.toml を指していること
```

選択順と注意点は[設定リファレンス](config-reference.ja.md)の「どのファイルが読まれるか」を参照。

### `doctor` が赤いまま

読み方は[運用ガイド](operations-guide.ja.md)にある。導入直後に出やすいものだけ挙げる。

| チェック | よくある原因 |
|---|---|
| `state-db` | まだ `totsuka run` を打っていない（正常） |
| `plugin:<name>` — secret not found | チェックリストの登録漏れ |
| `plugin:<name>` — crashed or exited | `xattr -dr com.apple.quarantine` の実行漏れ |
| `bundled-plugins`（警告） | `cargo install` 由来のビルドには同梱プラグインが無い。`--from-source` を使う |
| `hook-token`（失敗） | `run` が生成したトークンファイル `$XDG_STATE_HOME/totsuka/hook-token` を他のユーザーが読める。消して `totsuka run` を再起動する（初回 `run` 前に無いのは正常） |
| `config` — `[hooks].auth_token_ref was removed` | 0.9 までの設定が残っている。その行を消す（hook トークンは `run` が作る。[設定リファレンス](config-reference.ja.md) の「移行」） |

---

このページは内部ドキュメント `ai-docs/operations/setup-playbook.md` から生成されている。設計上の判断や実測の経緯はそちらにある。
