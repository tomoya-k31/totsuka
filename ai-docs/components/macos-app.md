---
type: Component
title: Totsuka.app（macOS メニューバーアプリ）
description: "apps/macos/ の SwiftUI メニューバーアプリ（ADR-0109）。totsuka run --watch --secrets-stdin --events-jsonl を子プロセスとして監督し（終了コードで再起動を判断）、通知をアプリ名義で出し、config schema / get / set / unset で config.toml を GUI 編集する。ロジックは SwiftPM の TotsukaKit（swift test）、出荷する .app は XcodeGen の project.yml から CI がビルドする。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/apps/macos
tags: [macos, swift, swiftui, menubar, app, notifier, config]
generated: { by: claude-code/opus-5.5, at: 2026-10-01T23:31:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

[ADR-0109](/decisions/adr-0109-native-menubar-app.md) のメニューバーアプリ。CLI の上に立つ薄い GUI で、TOML も機密の参照も自分では解釈しない —— 読み書きは全部 `totsuka` の CLI 契約（[config CLI 契約](/apis/config-cli.md)、[run --events-jsonl](/apis/run-events-jsonl.md)、`menu --json`）を通す。

# 構成

| 場所 | 中身 |
|---|---|
| `Package.swift` | SwiftPM パッケージ。`TotsukaKit`（ライブラリ）・`TotsukaApp`（アプリ本体の実行ターゲット。ソースは `Sources/Totsuka`。Xcode プロジェクトのアプリターゲット `Totsuka` と同名にすると `xcodebuild -scheme Totsuka` がパッケージ側を選んで素のバイナリを作るので、名前を分けている）・`TotsukaKitTests`。Swift 5 言語モード（`Process` のコールバックを actor に隔離する手間に見合う挙動が無いため） |
| `Sources/TotsukaKit/` | ウィンドウ無しで試せるものすべて: `CLI`（`totsuka` の実行、ログインシェルの環境 `$SHELL -lic env`、バイナリの探索、`$XDG_STATE_HOME`）、`Contracts`（`MenuModel` / `RunEvent` / `SchemaDocument` / `ConfigDocument` / JSON Pointer）、`Policy`（終了コードの方針、版の比較、`[macos]` の通知フィルタ、機密の名前）、`RunProcess`（子プロセスの監督）、`SecretStore`（Keychain の 1 項目の JSON マップ）、`Schema`（スキーマの節点から編集方法を決める `fieldKind`、追加時の初期値 `newValue`） |
| `Sources/Totsuka/` | SwiftUI: `AppModel`（状態と動作）、`SettingsView`（スキーマから組み立てるフォーム）、`TotsukaApp`（`MenuBarExtra`・`Settings`・ログの `Window`・通知クリックの受け口） |
| `Resources/Assets.xcassets` | アプリアイコン（`AppIcon`）とメニューバーのテンプレート画像（`StatusBarTemplate`、18pt） |
| `project.yml` | XcodeGen。出荷する `.app` のビルド定義（ad-hoc 署名、`LSUIElement`、`MARKETING_VERSION` は release-please が CLI と同じ版に上げ、`CFBundleShortVersionString` はそれを参照する。XcodeGen の既定の `1.0` のままだと版の照合と Keychain の事前案内が働かない） |
| `test.sh` | `swift test`。Command Line Tools だけの環境では swift-testing のフレームワークの場所を渡し、モジュールの無い `_Testing_Foundation` を避けるため cross-import overlay を切る |

# 振る舞い

- **起動**: Keychain のマップを読み（新しい版での初回は「次の確認で『常に許可』を」と先に言う）、`config validate --secrets-stdin` が通ったら `run` を子プロセスとして起動し、stdin の 1 行目にマップを書いて開けたままにする。終了コードは `exitDecision`: 0 は停止、1 とシグナルは 2 秒から倍々で最大 5 分のバックオフ再起動（1 分以上健全に動いたら数え直す）、2 と 4 は止めて表示、5 は外部の `run` として監視だけ
- **停止**: SIGTERM、300 秒待っても終わらなければ SIGKILL（メニューの「すぐに停止」でも）。起動中（設定の検証を待っている間）やバックオフ待ちの停止も効く。終了の通知は stdout / stderr の両方が EOF になってから出すので、exit 4 の理由の行が先に届く
- **外部の run**: exit 5 の後は `menu --json` がロックの解放（`down`）を見たところで引き継ぐ
- **通知**: `run` の stdout の `notify` 行を、`config get` で読んだ `[macos]` のフィルタ（ワークフロー別 → 全体 → 既定オン）に通してから `UserNotifications` で出す。クリックは `totsuka focus <task_id>`
- **メニュー**: 10 秒ごとと通知のたびに `menu --json`。要対応・作業中の各行に focus / retry / cancel（確認付き）。verify は置かない
- **設定**: `config schema` の `x-category` ごとに並べ、節点の種類（`fieldKind`）でフォームを作る。入力は 1 キーずつ `config set` / `unset`（JSON Pointer）。`x-secret` は Keychain に保存して `secret:<名前>` を書く（名前は `secretName`: キーパスのセグメントを `.` でつなぎ、`[A-Za-z0-9-]` 以外は `_XX` に逃がす一対一の符号化）。設定の書き込みは 1 本ずつ順に流す（同時に走ると、後の書き込みが先の編集を消すため）。`x-raw` や型の決まらないテーブルは JSON で編集する。「確認」で `config validate --secrets-stdin` を流す
- **更新**: 自分のバンドルが消えたら（`brew upgrade` と cleanup）メニューに「更新済み・再起動」を出し、CLI の隣の `Totsuka.app` を開いて自分は終わる。起動時に、ログイン項目が有効なら登録し直して新しい場所へ移す

# テスト

- `apps/macos/test.sh`（`TotsukaKit` の swift-testing。終了コードの方針、版の比較、イベントの解釈、通知フィルタ、スキーマの分類、機密の名前、整数の往復）
- CI の `macos app` ジョブ（`apps/macos/` を触った PR だけ）: `swift test` と、XcodeGen + `xcodebuild` での `.app` のビルド
- UI と、実機の Keychain・通知・ログイン項目の挙動はテストが無い。ADR-0109 の「実測」がプロトタイプでの確認の記録

# 関連

- [ADR-0109](/decisions/adr-0109-native-menubar-app.md)
- [config CLI 契約](/apis/config-cli.md) / [run --events-jsonl](/apis/run-events-jsonl.md)
- [Homebrew tap](/infrastructure/homebrew-tap.md)
