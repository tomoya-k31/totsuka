* **Update**: [ADR-0109](/decisions/adr-0109-native-menubar-app.md) — 実機での確認を受けて、アプリとスキーマの文言を英語だけにした。
* **Update**: [ADR-0109](/decisions/adr-0109-native-menubar-app.md) / [Totsuka.app](/components/macos-app.md) — 設定画面を外した。Settings… は config.toml を `$TERMINAL -e $EDITOR` で開き、Logs は run の stderr を書いたファイルを `$TERMINAL` で `tail -F` する。機密は Start 時に Keychain に無い `secret:<名前>` を尋ねる。スキーマと config CLI の層は残した。
* **Update**: [プラグイン開発ガイド](/development/plugin-dev-guide.md) — `config/schema` の説明から「メニューバーアプリの設定画面が尋ねる」前提を外した（設定画面を外したため）。
* **Update**: [ADR-0109](/decisions/adr-0109-native-menubar-app.md) / [Totsuka.app](/components/macos-app.md) — config に `secret:` が無ければ `run` を `--secrets-stdin` なしで起動し、`op://` / `cmd:` / `bw:` を `run` 自身に解決させる。1Password / Bitwarden をアプリ側で解決する案を採らない理由を記録した。
* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) / [Totsuka.app](/components/macos-app.md) — `menu --json` の各行に `repo` と `created_at` を足し、アプリの行にリポジトリと経過時間を出した。
* **Update**: [Totsuka.app](/components/macos-app.md) — Quit 以外の経路でアプリが終わっても `run` に SIGTERM を送る（取り残された `run` が次の起動をロック競合にしていた）。
