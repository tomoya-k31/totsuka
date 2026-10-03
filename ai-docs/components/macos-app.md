---
type: Component
title: Totsuka.app（macOS メニューバーアプリ）
description: "apps/macos/ の SwiftUI メニューバーアプリ（ADR-0113）。totsuka run --watch --events-jsonl（config に secret: があれば --secrets-stdin 付き）を子プロセスとして監督し（終了コードで再起動を判断）、通知をアプリ名義で出す。設定画面は持たず、config.toml を $TERMINAL の $EDITOR で開き、run の stderr を $TERMINAL で tail -F する。機密は Start 時に Keychain に無い secret:<名前> を尋ねる（[github].token の secret は、選べば Start のたびに gh auth token から取る。ADR-0114）。ロジックは SwiftPM の TotsukaKit（swift test）、出荷する .app は XcodeGen の project.yml から CI がビルドする。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/apps/macos
tags: [macos, swift, swiftui, menubar, app, notifier, config]
generated: { by: claude-code/opus-5.5, at: 2026-10-04T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

[ADR-0113](/decisions/adr-0113-native-menubar-app.md) のメニューバーアプリ。CLI の上に立つ薄い GUI で、TOML は自分では解釈しない。機密の参照は `secret:<名前>` の名前を集める（`secretNames`。CLI と同じ `[A-Za-z0-9_.-]` に合わない名前は尋ねない）ところまでで、解決は `run` に任せる —— 読むのは `totsuka` の CLI 契約（`config get`・`config validate`、[run --events-jsonl](/apis/run-events-jsonl.md)、`menu --json`）だけで、config.toml の編集は `$EDITOR` に任せる。

# 構成

| 場所 | 中身 |
|---|---|
| `Package.swift` | SwiftPM パッケージ。`TotsukaKit`（ライブラリ）・`TotsukaApp`（アプリ本体の実行ターゲット。ソースは `Sources/Totsuka`。Xcode プロジェクトのアプリターゲット `Totsuka` と同名にすると `xcodebuild -scheme Totsuka` がパッケージ側を選んで素のバイナリを作るので、名前を分けている）・`TotsukaKitTests`。Swift 5 言語モード（`Process` のコールバックを actor に隔離する手間に見合う挙動が無いため） |
| `Sources/TotsukaKit/` | ウィンドウ無しで試せるものすべて: `CLI`（`totsuka` と `gh` の実行、ログインシェルの環境 `$SHELL -lic env`、バイナリの探索 `locateExecutable`、`$XDG_STATE_HOME`）、`Contracts`（`MenuModel` / `RunEvent` / `ConfigDocument`）、`Policy`（終了コードの方針、版の比較、`[macos]` の通知フィルタ）、`RunProcess`（子プロセスの監督）、`AgentIDE`（有効な herdr / orca の抽出 `enabledAgentIDEs`、config の文字列の `${VAR}` 展開 `expandingEnv`（`config get` は書かれたままを返す。プラグインと同じく `~` は展開しない）、herdr ソケットの解決 `herdrSocketPath`と接続確認 `unixSocketAccepts`、`orca status --json` の判定 `orcaRuntimeReachable`、focus で前面に出すアプリ `focusApp`）、`SecretStore`（Keychain の 1 項目の JSON マップ）、`Launch`（config の `secret:` の名前を集める `secretNames`、`[github].token` の secret 名と `gh auth token` のホスト・アカウントを決める `githubTokenSecretName` / `ghHostname` / `ghAccount`、`$TERMINAL` / `$EDITOR` のシェル行 `editorCommand` / `tailCommand`、stderr の書き出し先 `RunLogFile`） |
| `Sources/Totsuka/` | SwiftUI: `AppModel`（状態と動作）、`TotsukaApp`（`MenuBarExtra` と、通知クリックの受け口） |
| `Resources/Assets.xcassets` | アプリアイコン（`AppIcon`）とメニューバーのテンプレート画像（`StatusBarTemplate`、18pt）。`MenuLabel` の表示は、`menu --json` の `attention`（Needs you）が空でない間は 1Hz で点滅、そうでなく `working` が空でない間は刃を光が上っていくコマ（`StatusBarWorking1Template`〜`8`、7〜8 は光の無い休止）を 4fps で切り替える。コマごとにアプリが起きるので fps は低く抑える。`run` が動いていない間は薄く表示する（外部の `run` が動いているときは通常の明るさ）。コマを進めるのは `AppModel.iconTick` のタイマーで、点滅と薄い表示は画像の透明度に焼き込む — ラベルの中の `TimelineView` は `MenuBarExtra` が描き直しを止めずに固まり（CPU 100%・メモリ 2GB 超）、ラベルの `.opacity` はメニューバーのボタンに反映されなかった（実測） |
| `project.yml` | XcodeGen。出荷する `.app` のビルド定義（ad-hoc 署名、`LSUIElement`、`MARKETING_VERSION` は release-please が CLI と同じ版に上げ、`CFBundleShortVersionString` はそれを参照する。XcodeGen の既定の `1.0` のままだと版の照合と Keychain の事前案内が働かない） |
| `build-app.sh` | Xcode 無しで `.app` を組み立てる（`apps/macos/build/Totsuka.app`、bundle ID は `io.github.tomoya-k31.totsuka.dev` で、設定・通知の許可・Keychain の項目が本番のアプリと分かれる）。`open --env XDG_CONFIG_HOME=…` で起動すると、その `XDG_*` / `TOTSUKA_*` がログインシェルの環境より優先されるので、隔離した環境で試せる。SwiftPM でビルドし、アセットカタログの代わりに `iconutil` で `.icns` を作ってメニューバーの PNG をそのまま同梱し、ad-hoc 署名する。手元で試す用で、出荷物は CI の `macos-app.yml` がビルドする |
| `test.sh` | `swift test`。Command Line Tools だけの環境では swift-testing のフレームワークの場所を渡し、モジュールの無い `_Testing_Foundation` を避けるため cross-import overlay を切る |

# 振る舞い

- **起動**: `config get` で読んだ config に `secret:` の参照があれば（`usesSuppliedSecrets`）、Keychain のマップを読み（新しい版での初回は「次の確認で『常に許可』を」と先に言う）、マップに無い名前をパスワード欄のダイアログで尋ねて保存し（取り消すと起動しない）、`[github].token` の secret のダイアログには「Use gh auth token」も出し（選ぶと UserDefaults の `githubTokenFromGh` に印を置くだけで、以後は尋ねない）、印があれば `gh auth token --hostname <api_url のホスト> --user <github_login>` の結果をマップに入れ（保存しない。失敗したら起動しない。[ADR-0114](/decisions/adr-0114-macos-app-gh-token.md)）、`config validate --secrets-stdin` が通ったら `run --secrets-stdin` を子プロセスとして起動し、stdin の 1 行目にマップを書いて開けたままにする。`secret:` が無ければ Keychain には触れず、`--secrets-stdin` なしで検証・起動して、`op://` / `cmd:` / `bw:` / `keychain:` を `run` 自身に解決させる。終了コードは `exitDecision`: 0 は停止、1 とシグナルは 2 秒から倍々で最大 5 分のバックオフ再起動（1 分以上健全に動いたら数え直す）、2 と 4 は止めて表示、5 は外部の `run` として監視だけ
- **アプリの終了**: Quit に限らず、ログアウトや外からの quit でも `applicationWillTerminate` で `run` に SIGTERM を送る（`run` は自分で正常に止まる。送らないと監督されない `run` が残り、次の起動がロック競合（exit 5）になる）
- **停止**: SIGTERM、300 秒待っても終わらなければ SIGKILL（メニューの「すぐに停止」でも）。起動中（設定の検証を待っている間）やバックオフ待ちの停止も効く。終了の通知は stdout / stderr の両方が EOF になってから出すので、exit 4 の理由の行が先に届く
- **外部の run**: exit 5 の後は `menu --json` がロックの解放（`down`）を見たところで引き継ぐ
- **通知**: `run` の stdout の `notify` 行を、`config get` で読んだ `[macos]` のフィルタ（ワークフロー別 → 全体 → 既定オン）に通してから `UserNotifications` で出す。クリックはタスク行のクリックと同じ focus（下記）
- **メニュー**: 2 秒ごと（1 回 10ms 未満）と通知のたびに `menu --json`。要対応・作業中の各行は 1 行目が `#<ID> <タイトル>`、2 行目が状態 · 取り込みからの経過時間 · リポジトリ · workflow（`MenuRow.detail`。パネルの幅で末尾が切れるので、重要なものから並べる。古い CLI で `repo` / `created_at` が無ければ省く）。行の領域のクリックが focus、右端のメニューに retry / cancel（確認付き）。verify は置かない
- **focus**: タスクの workflow の `agent` が orca なら起動中の「Orca」アプリを、それ以外（herdr）なら `[macos].activate_bundle_id` のアプリ（herdr を動かしているターミナル。通知の click-to-focus と同じキー）を `NSWorkspace.openApplication` で前面に出してから `totsuka focus <task_id>` を呼ぶ（`focusApp`）。Orca のバンドル ID は未実測なのでアプリ名で探す
- **エージェント IDE の稼働表示**: 10 秒ごとに `config get` を読み直し、`[plugins]` で有効な herdr / orca だけを調べて Running の横に ✓ / ✗ で出す（`enabledAgentIDEs`）。herdr はプラグインと同じ解決順のソケット（`herdrSocketPath`）に接続できるか、orca は `orca status --json` の `result.runtime.reachable`（CLI はアプリが落ちていても答える。10 秒で打ち切り、止まった呼び出しが以後の検査を塞がないようにする）。`run` の状態とは独立に、止まっている間も出す
- **設定**: 設定画面は無い（ADR-0113 §5）。Settings… は `config get` が返す config.toml を `$TERMINAL -e $EDITOR <path>` で開く（どちらもログインシェルの環境の値をシェル断片として使う。どちらかが無ければ `open -t`）。ファイルがまだ無ければ `totsuka init` を案内する。`$TERMINAL` が見つからない（シェルが 126 / 127 で終わる）ときはメニューに出す。`$EDITOR` の失敗はシェルがターミナルに置き換わった後なので、ターミナルの中に出る。変更は次の起動から効く
- **ログ**: `run` の stderr を `$XDG_STATE_HOME/totsuka/app-run.log` に書き（起動ごとに見出し行、5 MB を超えたら次の起動で書き直す）、Logs は `$TERMINAL -e tail -n 200 -F` で開く（`$TERMINAL` が無ければ Console）
- **パネル下段**: 左下にアプリの版（`CFBundleShortVersionString`。無ければ出さない）、右に `⋯` メニューと Quit。`⋯` には Settings… と Logs、区切りの下にログイン項目のオン・オフと、Keychain のマップを空にする「Forget saved secrets…」（`githubTokenFromGh` の印も消し、次の起動でまた尋ねる）。`totsuka` の場所と `PATH` は `defaults write <bundle ID> totsukaPath` / `pathOverride` で上書きできる
- **更新**: 自分のバンドルが消えたら（`brew upgrade` と cleanup）メニューに「更新済み・再起動」を出し、CLI の隣の `Totsuka.app` を開いて自分は終わる。起動時に、ログイン項目が有効なら登録し直して新しい場所へ移す

# テスト

- `apps/macos/test.sh`（`TotsukaKit` の swift-testing。終了コードの方針、版の比較、イベントの解釈、通知フィルタ、`secret:` の名前の収集、`gh auth token` の引数（ホストの変換と、決められない値の除外）、`$TERMINAL` / `$EDITOR` のシェル行と引用、ログファイルの書き直し、ログインシェルの環境、整数の往復、エージェント IDE の抽出・herdr ソケットの解決順・orca の稼働判定・focus 先アプリ）
- CI の `macos-app.yml`（`apps/macos/**` を触った PR と手動実行だけ。`on: paths` はワークフロー単位でしか効かないので `ci.yml` とは分けた）: `swift test` と、XcodeGen + `xcodebuild` での `.app` のビルド。ビルドした `.app` は `ditto` で zip にして artifact `Totsuka.app`（7 日）に残す —— 実機で試すにはこれを `~/Applications` に展開する（`/tmp` に置くと通知が許可されない）
- UI と、実機の Keychain・通知・ログイン項目の挙動はテストが無い。ADR-0113 の「実測」がプロトタイプでの確認の記録

# 関連

- [ADR-0113](/decisions/adr-0113-native-menubar-app.md)
- [config CLI 契約](/apis/config-cli.md) / [run --events-jsonl](/apis/run-events-jsonl.md)
- [Homebrew tap](/infrastructure/homebrew-tap.md)
