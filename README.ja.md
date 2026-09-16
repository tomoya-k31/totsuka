> 🌐 [English](README.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

# totsuka

**AI 駆動の開発フロー自動化ツール。** totsuka はタスクソース（GitHub Issues、
Notion、Slack メンション）からタスク指示を検知し、ワークフローにマッチさせ、
AI コーディングエージェント（herdr、orca）へ — それぞれ専用の git worktree 上
で — オーケストレーションします。成果はプルリクエスト作成、またはソースへの
書き戻しとして publish します。

- **タスクソース**: GitHub Issues / Projects、Notion データベース、Slack
  メンション（返信案を承認すると本人名義で返信）
- **エージェント**: [herdr](https://herdr.dev/)、[orca](https://www.onorca.dev/)
  — いずれもサードパーティの agent IDE。totsuka のプラグインプロトコル越しに駆動する
- **隔離**: 1 タスク = 1 リポジトリ = 1 worktree = 1 ブランチ
- **出力ポリシー**: プルリクエスト作成 / ソースへ書き戻し / なし
- **ローカルファースト**: 単一の CLI バイナリ、デーモンなし、シークレットは手元の
  保管先（1Password / Keychain / 環境変数 / コマンド）に置いたまま

> ステータス: 1.0 前（v1 スコープを目標）。現行バージョンは
> [リリースページ](https://github.com/tomoya-k31/totsuka/releases)を参照。
> 現状 macOS のみ。コードは XDG 準拠で、将来の Linux 移植に向け
> プラットフォーム境界を抽象化済み。

## 前提

totsuka はエージェントをオーケストレーションするツールで、エージェント自体は
同梱しません。いずれかの agent IDE を別途インストールし、ワークフローから
指定してください。

- **[herdr](https://herdr.dev/)** — **0.7.5 以降が必要**。プラグインが
  `initialize` で herdr 自身のバージョン（`ping` の `version`）を検査し、
  それ未満は `CONFIG_INVALID` で拒否します。上限はありません — 新しい herdr を
  拒否することはありません。
- **[orca](https://www.onorca.dev/)** — `orca` CLI 経由で駆動します。

## インストール

### Homebrew

```sh
brew install tomoya-k31/tap/totsuka
```

これだけです。`sudo` も、ツリーを手で配置することも、quarantine 属性を除去する
ことも要りません。Homebrew は素の `curl` でリリースアセットを取り、`curl` は
`com.apple.quarantine` を書かないためです（macOS 15.7.3 で本体・同梱プラグイン
とも実測）。

Homebrew はサードパーティ tap に trust を要求しますが、
**formula を名指しすれば同じコマンドの中で付与されます**。
`==> Trusted formula tomoya-k31/tap/totsuka` という 1 行が出て、そのまま進みます。
答えるべきプロンプトはありません。

更新は `brew upgrade totsuka` です。リリースのたびにワークフローが formula を
新しいリリースへ向けます。

### ビルド済み tarball（GitHub Releases）

Homebrew を使わないマシン向けです。[最新リリース](https://github.com/tomoya-k31/totsuka/releases/latest)
から macOS ユニバーサル tarball をダウンロードします。
`totsuka` **と同梱プラグイン**が入っているので、ツリーごと配置してバイナリを
`PATH` に symlink します:

```sh
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
```

バイナリだけでなくディレクトリごと移動します。`totsuka` は同梱プラグインを
自分自身の隣から探すため、`totsuka setup` がパス指定なしでプラグインを入れられ
ます。ツリーを残しておけば、後からプラグインを追加・再インストールするときに
再ダウンロードも要りません。

すべて ad-hoc 署名です。Gatekeeper にブロックされた場合、ツリー全体に対して一度
だけ quarantine 属性を除去してください:

```sh
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

### ソースから

```sh
cargo install --git https://github.com/tomoya-k31/totsuka orchestrator-cli
```

こちらは CLI のみです。プラグインはチェックアウトからビルドします —
[プラグイン開発ガイド](docs/plugin-dev-guide.ja.md) を参照してください。

## クイックスタート（5 分・1 タスク）

```sh
# 1. 使うプラグインを選ぶ。質問はこれだけ。選んだものを導入し、totsuka が解釈する
#    設定を全部コメントで書いた config.toml を置いて、その場所を教えてくれる。
totsuka setup                                  # あるいは: --plugins github,herdr,macos

# 2. そのファイルを開いて、必要な行のコメントを外す。最低限 [[repositories]]、
#    導入したプラグインの `[plugins.<name>] enabled = true`、そのプラグインの
#    [<name>] テーブル、そして [[workflows]]。ファイル末尾のレシピ集に、
#    コメントを外せばそのまま動く組み合わせが入っている。
$EDITOR ~/.config/totsuka/config.toml

# 3. 印字されたシークレットを登録する。値そのものは setup が扱わないので、
#    そのまま貼れるコマンドが 1 件ずつ出る。例:
security add-generic-password -U -s totsuka -a github-token -w '<トークン>'

# 4. 検証してから 1 サイクル実行（常駐ポーリングは --watch）。
totsuka config validate # 設定がパースでき、辻褄が合っている
totsuka doctor          # まわりの環境が整っている
totsuka run --dry-run   # プレビュー: どのタスク -> どのリポジトリ -> どのエージェント
totsuka run             # 実行: fetch -> dispatch -> 監視 -> publish
```

**編集するまで何も有効になりません。** `setup` が書く有効行は `version = 1` の 1 行だけで、
残りは全部ドキュメントとして置かれます。それが狙いです —— ウィザードがたまたま聞いた質問
からしか到達できない代わりに、選択肢の全体が、いま開いているファイルの中にあります。
導入したプラグインを **enabled にしない**のも同じ理由です: `totsuka config validate` は
enabled なプラグインを実際に起動するので、トークン参照がまだコメントのうちに有効化すると、
セットアップを確認するためのコマンドがそのセットアップ自身で落ちます。

**シークレットを全部登録しても、`totsuka run` を一度打つまで `doctor` は赤のままです。**
状態 DB が無いあいだ `state-db` チェックが fail し、これを作るのは `run` だけだからです。
上の順番がそのまま緑になる順番です。`warn:` の行（hook トークン未設定、同梱プラグイン
無しなど）は残ることがありますが、これらは助言であって失敗ではありません。

進捗は `totsuka status`、個別タスクは `totsuka task show <id>`、ログ追尾は
`totsuka logs -f` で確認します。

あとから `setup` を打ち直すと、まだ無い節だけが追記され、自分で書いた行は 1 バイトも
変わりません。`totsuka setup --plugins notion` が、何か月も後に `[notion]` の
コメント付き雛形を docs 抜きで手に入れる方法です。

2 台目には **config.toml そのもの**を持っていきます。中身は
シークレットの*参照*（`op://…` / `keychain:…`）だけで値は入らないので、dotfiles に
置いても安全です。

```sh
cp ~/.config/totsuka/config.toml ~/dotfiles/totsuka-config.toml
```

新マシン・開発機・トークンローテーション・復旧は
[セットアップ Playbook](docs/setup-playbook.ja.md) が通しで扱います。

## ドキュメント

- **totsuka とは**: [docs/orchestrator-spec.ja.md](./docs/orchestrator-spec.ja.md)
- **セットアップ Playbook**: [docs/setup-playbook.ja.md](./docs/setup-playbook.ja.md)
- **Slack セットアップ**: [docs/slack-setup.ja.md](./docs/slack-setup.ja.md)
- **設定リファレンス**: [docs/config-reference.ja.md](./docs/config-reference.ja.md)
- **運用ガイド**（doctor / worktree 掃除 / FAQ）: [docs/operations-guide.ja.md](./docs/operations-guide.ja.md)
- **プラグイン開発ガイド**: [docs/plugin-dev-guide.ja.md](./docs/plugin-dev-guide.ja.md)
- **目次**: [docs/index.ja.md](./docs/index.ja.md) · **変更履歴**: [CHANGELOG.md](./CHANGELOG.md)

これらのページは [`ai-docs/`](./ai-docs/)（OKF v0.2 準拠の Knowledge Bundle）から
生成しています。設計判断・実測・経緯まで含むリポジトリの全知識はそちらにあるので、
totsuka 自体を開発する場合はそちらを読んでください。

## コントリビュート

Conventional Commits 必須（`type(scope): description`）。リリースは
[release-please](https://github.com/googleapis/release-please) の Release PR を
マージして切ります。docs 変更は `bash scripts/okf-lint.sh ai-docs` で検証されます。

## ライセンス

[MIT](./LICENSE)。
