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
- **エージェント**: herdr、orca（プラグインプロトコル越しに駆動する agent IDE）
- **隔離**: 1 タスク = 1 リポジトリ = 1 worktree = 1 ブランチ
- **出力ポリシー**: プルリクエスト作成 / ソースへ書き戻し / なし
- **ローカルファースト**: 単一の CLI バイナリ、デーモンなし、シークレットは Keychain

> ステータス: v1。現状 macOS のみ。コードは XDG 準拠で、将来の Linux 移植に向け
> プラットフォーム境界を抽象化済み。

## インストール

### ビルド済み tarball（GitHub Releases）

[最新リリース](https://github.com/tomoya-k31/totsuka/releases/latest) から macOS
ユニバーサル tarball をダウンロードします。`totsuka` **と同梱プラグイン**が入って
いるので、ツリーごと配置してバイナリを `PATH` に symlink します:

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
[プラグイン開発ガイド](docs/development/plugin-dev-guide.md) を参照してください。

## クイックスタート（5 分・1 タスク）

```sh
# 1. いくつか質問に答える。開始レシピを選び、リポジトリを登録し、シークレットの
#    置き場所を指定すると、設定の書き出し・レシピが必要とするプラグインの
#    install + enable・最後に `doctor` の実行まで一気に済む。
totsuka setup

# 2. 印字されたシークレットを登録する。値そのものは setup が扱わないので、
#    そのまま貼れるコマンドが 1 件ずつ出る。例:
security add-generic-password -U -s totsuka -a github-token -w '<トークン>'

# 3. 1 サイクル実行（常駐ポーリングは --watch）。
totsuka run --dry-run   # プレビュー: どのタスク -> どのリポジトリ -> どのエージェント
totsuka run             # 実行: fetch -> dispatch -> 監視 -> publish
```

印字されたシークレットが 1 つでも未登録のうち、`setup` は終了コード 3 で終わり
ます。これは `doctor` が「まだ人間がやることが残っている」と報告しているので
あって、setup 自体の失敗ではありません。

**シークレットを全部登録しても、`totsuka run` を一度打つまで `doctor` は赤の
ままです。** 状態 DB が無いあいだ `state-db` チェックが fail し、これを作るのは
`run` だけだからです。上の順番どおり「登録 → 1 回実行 → `totsuka doctor`」で
終了コード 0 になります。`warn:` の行（hook トークン未設定、同梱プラグイン無し
など）は残ることがありますが、これらは助言であって失敗ではありません。

進捗は `totsuka status`、個別タスクは `totsuka task show <id>`、ログ追尾は
`totsuka logs -f` で確認します。

`totsuka init` は CI・スクリプト用に残しています。**絶対に対話しない**代わりに、
ディレクトリと全行コメントの雛形しか書きません。`setup` はその雛形を埋めるので、
先に `init` を打っても害はありませんが不要です。

新マシン・開発機・トークンローテーション・復旧は
[セットアップ Playbook](docs/operations/setup-playbook.md) が通しで扱います。

## ドキュメント

このリポジトリの知識はすべて [`docs/`](./docs/)（OKF v0.2 準拠の Knowledge
Bundle）で管理しています。

- **仕様書**: [docs/product/orchestrator-spec.ja.md](./docs/product/orchestrator-spec.ja.md)
- **設定リファレンス**: [docs/development/config-reference.md](./docs/development/config-reference.md)
- **プラグイン開発ガイド**: [docs/development/plugin-dev-guide.md](./docs/development/plugin-dev-guide.md)
- **運用ガイド**（doctor / worktree 掃除 / FAQ）: [docs/operations/operations-guide.md](./docs/operations/operations-guide.md)
- **目次**: [docs/index.md](./docs/index.md) · **変更履歴**: [CHANGELOG.md](./CHANGELOG.md)

## コントリビュート

Conventional Commits 必須（`type(scope): description`）。リリースは
[release-please](https://github.com/googleapis/release-please) の Release PR を
マージして切ります。docs 変更は `bash scripts/okf-lint.sh docs` で検証されます。

## ライセンス

[MIT](./LICENSE)。
