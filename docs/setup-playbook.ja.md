> 🌐 [English](setup-playbook.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/setup-playbook.md sha256:1fab1b1a68c3133f226c90b1c82ab9234087e9d351f9a3cb11d450a1c554e244 -->

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

[最新リリース](https://github.com/tomoya-k31/totsuka/releases/latest) の macOS ユニバーサル tarball を落とす。**ツリーごと**置くこと — `totsuka` は同梱プラグインを自分の隣から探すので、バイナリだけ移すとセットアップがプラグインを見つけられない。

```bash
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

`xattr` を忘れると Gatekeeper が**プラグインの起動だけ**を黙って止める。本体は動くので原因が見えにくく、`doctor` は「crashed or exited」としか言えない。

### 2. `totsuka setup` を走らせる

```bash
totsuka setup
```

聞かれるのは最大 5 種類で、残りはレシピが持っている。5 つ目は、Status 列の間でカードを動かすレシピを選んだときだけ出る。

1. **どのレシピから始めるか**（GitHub 最小構成 / 設計→実装ハンドオフ / Slack 本人名義返信 / 人間検収必須）
2. **リポジトリのパスと名前**（複数可）
3. **シークレットをどこに置くか**（1Password / Keychain / 環境変数）— **値そのものは一切聞かれない**
4. レシピが要求する項目だけ（GitHub Project の owner や番号、Slack のメンバー ID、LLM のモデル名など）
5. **Project の Status 列名**（そのレシピが列を使う場合のみ）。候補は役割を説明する名前（`Ready to implement` など）だが、**入力した値はボードの Status フィールドの選択肢と完全に一致させる必要がある。** ここを間違えたときが一番厄介で、設定は valid のまま `doctor` も緑、`run` が何も拾わないという無言の失敗になる。だから計画には、名前を埋めた後の trigger をそのまま表示する。answers ファイルがこれを欠いている場合は、選んでいない名前で埋めるのではなく**足すべきキー名を名指しして拒否する**。

計画が表示され、確認して初めて副作用が出る。**質問の途中で Ctrl-C しても何も残らない。**

続けてプラグインの設定ファイル生成、プラグインのインストールと有効化、`doctor` までが走る。先に `totsuka init` を打つ必要はない。

### 3. シークレットを登録する

最後にチェックリストが出る。各行に参照名・何が可能になるか・登録コマンドが載っているので、そのままコピーして実行する。

```bash
security add-generic-password -U -s totsuka -a github-token -w '<paste the value>'
```

**ここに出た参照はすべて必須**である。設定が参照している以上、1 つでも欠けるとそのプラグインは起動しない。「任意の機能だから飛ばしてよい」ものは、そもそもチェックリストに出ない。

Slack の bot トークンは一見任意に見えるが必須である。**本人名義での返信は Slack の通知を一切鳴らさない**ため、レシピは bot からの通知を前提に組まれている。

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
| 通知クリック | `terminal-notifier` の導入と bundle id の設定 |

## 開発機に入れる

チェックアウトからビルドして入れる。tarball は要らない。

```bash
git clone https://github.com/tomoya-k31/totsuka
cd totsuka
cargo build --release --workspace --bins
totsuka plugin install --from-source --all --enable
totsuka setup
```

`--from-source` は現在地から上へ「Cargo ワークスペースのルートかつ `plugins/` を持つ」ディレクトリを探すので、別のリポジトリの中で打っても誤検出しない。チェックアウト内で `totsuka setup` を打つと、同梱ツリーが無い場合は自動で `--from-source` を選ぶ。

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

### 全般

`setup` を再実行する必要はない。参照名は変わっておらず値だけが変わったので、`-U`（既存を更新）付きで上書きしてから `totsuka doctor` を打てばよい。

## 途中で失敗したときの復旧

### 失敗した

**再実行すれば揃う。** `setup` は各ステップが冪等で、どこまで適用したかも表示される。

```bash
totsuka setup
```

既存の設定ファイルはスキップされるので、2 回目は実質「プラグイン導入と `doctor` だけ」が走る。

### 設定を作り直したい

`setup` は既存ファイルを上書きしない。作り直すなら自分で退避する。

```bash
mv ~/.config/totsuka/config.toml{,.bak}
mv ~/.config/totsuka/plugins ~/.config/totsuka/plugins.bak
totsuka setup
```

例外として、`totsuka init` が吐いた全行コメントの雛形だけは未設定として扱われ、`setup` が中身を埋める。退避は要らない。

### 同じ設定を別マシンで再現したい

回答ファイルを保存して持っていく。**`setup` は機密の値をファイルに書かない**（どのバックエンドを使うかを記録し、値の登録コマンドを印字するだけである）ので、`setup` が生成したファイルは dotfiles に置いても安全である。

```bash
totsuka setup --save-answers ~/dotfiles/totsuka-answers.toml
totsuka setup --answers ~/dotfiles/totsuka-answers.toml --yes
```

シークレットの登録だけは各マシンで人間が行う。

このファイルは書いたビルドとは別のビルドが読むので、形式は契約として扱われる。古いファイルの意味が変わるような変更では `version` が上がり、版の違うファイルは**推測されずに拒否される**（作り直すよう案内が出る）。レシピは番号ではなく名前（`recipe = "minimal-github-herdr"`）で指定するので、メニューにレシピが増えても、持ち歩いているファイルの指す先が黙って変わることはない。

### `doctor` が赤いまま

読み方は[運用ガイド](operations-guide.ja.md)にある。導入直後に出やすいものだけ挙げる。

| チェック | よくある原因 |
|---|---|
| `state-db` | まだ `totsuka run` を打っていない（正常） |
| `plugin:<name>` — secret not found | チェックリストの登録漏れ |
| `plugin:<name>` — crashed or exited | `xattr -dr com.apple.quarantine` の実行漏れ |
| `bundled-plugins`（警告） | `cargo install` 由来のビルドには同梱プラグインが無い。`--from-source` を使う |
| `hook-token`（警告） | `[hooks].auth_token_ref` が未設定。フック対応エージェントを使う前に設定する |

---

このページは内部ドキュメント `ai-docs/operations/setup-playbook.md` から生成されている。設計上の判断や実測の経緯はそちらにある。
