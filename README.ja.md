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

バイナリだけでなくディレクトリごと移動します。プラグインは
`/usr/local/lib/totsuka/plugins/` に入っており、必要なものをそこから
インストールします（クイックスタートの手順 2）。ツリーを残しておけば、後から
プラグインを追加・再インストールするときに再ダウンロードが要りません。

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
# 1. 設定ファイルの雛形生成と環境チェック。
totsuka init

# 2. 必要なプラグイン（task source / agent / notifier）を install して enable。
#    プラグインは tarball 同梱で /usr/local/lib/totsuka/plugins/ 配下にある。
totsuka plugin install /usr/local/lib/totsuka/plugins/github
totsuka plugin enable github

# 3. シークレットを Keychain に保存し、設定から参照
#    （例: api_key_ref = "keychain:totsuka/github"）。
#    ~/.config/totsuka/config.toml を編集 — repositories / workflows / [llm]。

# 4. 配線を検証。
totsuka doctor

# 5. 1 サイクル実行（常駐ポーリングは --watch）。
totsuka run --dry-run   # プレビュー: どのタスク -> どのリポジトリ -> どのエージェント
totsuka run             # 実行: fetch -> dispatch -> 監視 -> publish
```

進捗は `totsuka status`、個別タスクは `totsuka task show <id>`、ログ追尾は
`totsuka logs -f` で確認します。

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
