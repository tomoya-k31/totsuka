---
type: Decision
title: ADR-0109 ネイティブ macOS メニューバーアプリが run を子プロセスとして監督し、通知と設定 GUI を持つ
description: "SwiftBar 向けの `totsuka menu` だけでは届かない 2 つの要件（アプリ名義のネイティブ通知と、config.toml の GUI 編集）のため、SwiftUI のメニューバーアプリ（apps/macos/）を足す決定。run は `--secrets-stdin` 付きの子プロセスとして起動し終了コードで再起動を判断、通知は `run --events-jsonl` の stdout、設定画面は JSON Schema（core は schemars、プラグインは新メソッド config/schema）から生成し読み書きは CLI 経由。Developer Program に加入しないため ad-hoc 署名の .app を release tarball に同梱して formula で配る。ADR-0065 の「Swift アプリ」却下を部分的に覆す。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/apps/macos
tags: [decision, macos, menubar, notifier, config, protocol, distribution, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-01T21:40:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。gh stack の 4 層で入れる。本 ADR は 1 層目（プロトコル 0.7.7 の `config/schema`）と同時に入り、残りの 3 層が同じ決定を実装する。

1. プロトコル: `config/schema` と capability `config_schema`（0.7.7）
2. core / CLI: `config schema` / `config get` / `config set` / `config unset`、`run --events-jsonl`
3. 同梱 7 プラグイン: `config/schema` に答える
4. `apps/macos/`（SwiftUI アプリ）、CI のビルドジョブ、release tarball への同梱と formula

[ADR-0065](/decisions/adr-0065-menubar-status.md) の「却下した案」のうち **独立した Swift/AppKit `.app`** の行を覆す。SwiftBar 向けの `totsuka menu` はそのまま残し、アプリも状態の取得にその `--json` を使う。

# Context

ADR-0065 は「要件が UI の自由度を必要としない」ことを理由に Swift アプリを却下し、SwiftBar のプラグイン書式を吐く `totsuka menu` を選んだ。その後、次の 2 つが要件になった:

- **通知をアプリ名義で出し、クリックを受ける。** 今の通知は notifier-macos プラグインの osascript（クリック不可）か terminal-notifier（別アプリの名義）で、どちらも totsuka の通知として許可・管理できない
- **`config.toml` を GUI で設定する。** カテゴリごとに 1 項目ずつ手入力し、項目ごとにヘルプ（英語・日本語、OS の言語に合わせる）を出す。設定が通るまで `run` を起動させない

どちらも SwiftBar の上では作れない。一方で、アプリが `run` を監督する前提の下準備は #753〜#756 で済んでいる: SIGTERM / SIGHUP での graceful 停止（#753）、親プロセスから stdin で機密を受け取る `--secrets-stdin` と `secret:<name>`（[ADR-0100](/decisions/adr-0100-secrets-stdin.md)）、起動時エラーの終了コード（[ADR-0095](/decisions/adr-0095-run-startup-exit-codes.md)）、タスク操作のエンドポイント（[ADR-0094](/decisions/adr-0094-task-control-endpoints.md)）。GitHub トークンは無期限トークンで運用する（#756 の選択肢 a）。

Apple Developer Program には**加入しない**。それでも友人に配りたい。

# Decision

## 1. 技術と配置

- **Swift + SwiftUI**（`MenuBarExtra`、`Settings` シーン、`UserNotifications`）、**macOS 15 以上**。CLI 本体の前提（macOS 14 以上、仕様 §3.3）は変えない
- ソースは同じリポジトリの **`apps/macos/`**。CLI のフラグ・JSON の形・config スキーマと同じ PR で追従できるようにするため
- プロジェクトは **XcodeGen（`project.yml`）**。`.xcodeproj` は生成物としてコミットしない（pbxproj の衝突を避ける）
- 表示名 `Totsuka`、bundle ID `io.github.tomoya-k31.totsuka`。Dock に出さず（`LSUIElement`）、ログイン項目は `SMAppService.mainApp`

## 2. run の監督

- `totsuka run --watch --secrets-stdin --events-jsonl` を**子プロセス**として起動し、stdin の 1 行目に機密のマップを書いて開けたままにする（ADR-0100）
- 終了コードで分ける: **1** は指数バックオフで再起動、**4**（設定・機密）と **2**（使い方）は止めてエラーを表示、**5**（lock 競合）は「外部の run が稼働中」として監視だけする
- アプリ起動時は前回の状態（動いていたか）を復元する。停止は SIGTERM で、最大 300 秒（[ADR-0092](/decisions/adr-0092-git-timeout.md) の git 待ち）「停止中…」を出す。強制終了は別メニュー
- 子プロセスの `PATH` は起動時に `$SHELL -lic env` から取り、設定で上書きできる（ADR-0100 の申し送り）
- stderr は直近 N 行をメモリに持ってログウィンドウに出し、JSONL のログフォルダを開ける
- **起動できる条件は `config validate --secrets-stdin` が exit 0 で終わること**。失敗した項目は設定画面に出す

## 3. 状態表示とメニュー

- 状態は `totsuka menu --json`（`MenuModel`）を流用する。SwiftBar 版は残す（CLI だけで使う人の経路）
- メニューの操作は focus・cancel（確認付き）・retry。UDS の `/focus`・`/task/cancel`・`/task/retry` に hook-token の Bearer で直接 POST する（ADR-0094 / [ADR-0099](/decisions/adr-0099-generated-hook-token.md)）。**`task verify` は置かない**（取り消せない操作なので ADR-0065 と同じ判断）

## 4. 通知

- **`run --events-jsonl`** を足す。`notify` と同じ `NotifyParams` を 1 行 1 JSON で stdout に書く。通知の経路（`deliver_notification`）は 1 か所しかないので、そこで書く
- `--events-jsonl` と `--json` は排他（`--json` の stdout は要約 1 文書という契約がある）
- `--events-jsonl` のとき **notifier プラグインは起動しない**。親プロセスが通知者なので、残すと二重に出る
- 絞り込み（workflow × イベント種別、F-92）は **`[macos]` の設定をアプリが読んで適用する**。core が `[macos]` を読むのはプラグインの所有物に触ることになる（[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md)）
- クリックは `task_id` を `userInfo` で受けて `/focus` に POST する

## 5. 設定 GUI

- **全項目を JSON Schema から生成する。** core は自分の部分を **schemars** で書き出し、各プラグインは新メソッド **`config/schema`** で自分のテーブルのスキーマを返す
- `config/schema` は **`initialize` より前に答える**（`config/validate` と同じ）。`initialize` は機密を解決し、ポーリングや接続確認を始めるので、まだ機密が無い設定画面からは呼べない
- task_source は答えに **`project`**（自分が source の `[[projects]]` 要素に読むキー）と **`workflow`**（`[[workflows]]` に読むキー: `trigger` と、claim する平置きのオプション）のスキーマも載せられる（どちらも任意）。agent プラグインは `workflow` に claim するオプションだけを載せる。どちらのテーブルも中身は source ごとに違い、core は解釈しないので、**選んだ source（と agent）のキーだけを出す**。全部を並べて使えない項目を無効にする案は採らない —— source ごとのキーはほとんど重ならず、灰色の行ばかりのフォームになり、別の source のキーが残っていると `deny_unknown_fields` で `initialize` が落ちるのに、その危険が見えなくなる
- **capability `config_schema` で申告し、マニフェストから読む。** プラグインを起動せずに尋ねるべきかが分かる。未申告や失敗のプラグインのテーブルはフォームではなく生の TOML 欄に回す（エラーにしない。スキーマは人の編集を助けるだけで、実行時の挙動を何も変えない）
- 拡張キーワードは 4 つ: **`x-title`** と **`x-help`**（`{en, ja}`、OS の言語に合わせて出す）、**`x-category`**（カテゴリ。`{en, ja}`）、**`x-secret`**（機密の参照を持つフィールド）。未知のキーワードは無視されるので、これらの無いスキーマも有効
- **読み書きは CLI 経由**（Swift 側で TOML を扱わない）:
  - `config schema` — core とプラグインのスキーマを 1 つのルートスキーマにまとめて返す
  - `config get` — 実際に読むファイル（`--config` / `hosts/<host>.toml` / `config.toml`、[ADR-0106](/decisions/adr-0106-per-host-config-file.md)）のパスと中身を JSON で返す
  - `config set <path> <json>` / `config unset <path>` — `toml_edit` でコメントを保ったまま 1 キーずつ書き換え、構文と型が通るときだけ書き込む
- `x-secret` のフィールドは、保存すると config に **`secret:<ドット区切りのパス>`** が自動で書かれ、値は Keychain の 1 項目（JSON マップ）に入る。既存の `op://` などの参照は `--secrets-stdin` の下では拒否されるので、設定画面で「要入力」として出し、入力されたら `secret:` に置き換える

## 6. 配布

- CI（macOS ランナー）で `.app` をビルドし **ad-hoc 署名**、release の universal tarball に同梱する。formula は `prefix/"Totsuka.app"` に置く
- **formula が入れるファイルには quarantine が付かない**（Homebrew が quarantine を付けるのは cask だけ）。今の `totsuka` バイナリが公証なしで動いているのと同じ経路
- **CLI と同じ版で同時にリリースする。** アプリは起動時に `totsuka --version` を見て、メジャーが違えば起動を止め、マイナー・パッチの違いは警告して `brew upgrade` を案内する
- **更新のたびに Keychain の確認が 1 回出るのは受け入れる**（下の実測）。新しい版の初回起動で、確認が出る前に「次の確認で『常に許可』を」と案内する
- `brew upgrade` と cleanup で自分のバンドルが消えたことを検知したら、メニューに「更新済み・再起動」を出す。再起動でログイン項目の登録先も新しい Cellar に移る

# 実測（プロトタイプ、2026-10-01、macOS 26.1）

`swiftc` で組み立てた使い捨ての `.app`（`MenuBarExtra` / `UserNotifications` / `SMAppService` / Keychain）で前提を確かめた:

- **Keychain**: ad-hoc 署名で作った項目の ACL は、信頼済みアプリの条件が `cdhash`、`partition_id` も `cdhash:…`。**自己署名**証明書で署名すると信頼済みアプリの条件は `identifier … and certificate leaf = H"…"`（ビルドし直しても変わらない）になるが、**`partition_id` は `cdhash:…` のまま**で、ビルドし直すと確認ダイアログが出た。partition が `teamid:` になるのは Apple が発行した証明書だけなので、**Developer Program なしでは署名の方法によらず更新ごとに 1 回確認が出る**。自己署名を採らないのはこのため
- **通知**: ad-hoc 署名でも許可が取れ、クリックで `userInfo` の `task_id` が届いた。許可は bundle ID 単位で、同じ ID の新しいビルドに引き継がれた。**`/tmp` 配下に置いた `.app` は、新しい bundle ID でもダイアログなしで即座に拒否された**ので、開発ビルドを `/tmp` から起動しない
- **ログイン項目**: ad-hoc 署名でも `SMAppService.mainApp.register()` が通った（status = enabled）。Homebrew を模した配置（`Cellar/<ver>/Totsuka.app`、`opt` のシンボリックリンク、`~/Applications` からのリンク）で、BTM に記録される URL は**リンクを辿った先の `Cellar/<ver>`** で、新しい版を起動すると新しい Cellar に移った。**更新後に一度も起動せずに再ログインした場合は未確認**で、上の「再起動を促す」はそのための対策
- **署名**: 信頼設定をしていない自己署名証明書でも、キーチェーンが検索リストにあれば `codesign` は通った

# 却下した案

| 案 | 却下の理由 |
|---|---|
| Tauri（Rust + WebView） | WebView の UI になり、通知も plugin 経由。「ネイティブ」の要件に合わない |
| Rust 単体（objc2 / tray-icon） | 言語は揃うが、設定 GUI の実装量が大きく、テストしにくい（ADR-0065 と同じ理由） |
| 別リポジトリ | CLI のフラグ・JSON・スキーマの変更とアプリの追従を同じ PR にできない |
| `SMAppService` で `run` を LaunchAgent として登録 | アプリが終了しても `run` が残るが、stdin で機密を渡す経路（ADR-0100）が使えない |
| 通知を `menu --json` の差分から作る | ポーリングの間の遷移と完了イベントを取りこぼす |
| 新しい notifier プラグインからアプリの UDS へ送る | アプリ側に受信サーバーと認証が要る。親子関係のパイプで足りる |
| core が `[macos]` を読んで絞ってから stdout へ出す | プラグインの所有物を core が読むことになる（ADR-0058） |
| 設定 GUI を主要項目の手書きフォームに絞る | スキーマが変わるたびに Swift 側を追従させる必要があり、プラグインのキーに届かない |
| Swift 側で TOML をパース・書き出し | TOML の実装が 2 つになり、コメントの保持が怪しい |
| プラグインのスキーマを `--print-schema` フラグで取る | プロトコルの外に 2 つ目の規約ができる |
| ヒントを doc コメントから流用 | 英語・開発者向け（仕様番号の参照が混ざる）で、利用者向けのヒントにならない |
| Developer ID + 公証 + cask | Developer Program に加入しない |
| 公証なしの cask | Homebrew は 2026-09-01 に Gatekeeper を通らない cask の扱いをやめ、`--no-quarantine` も廃止した。インストール・更新のたびに起動がブロックされる |
| formula でソースからビルド（SwiftPM） | quarantine は付かないが、利用者の手元で毎回ビルドが走る。CI でビルドして同梱すれば同じ効果で済む |
| 自己署名証明書で署名 | 上の実測のとおり、Keychain の確認を避けられない。CI に鍵を置く手間だけが残る |
| アプリと CLI を別の版で管理 | formula で同時に入るので、版を分けても同じ組み合わせしか出回らない |

# Consequences

- 仕様 §3.2 の「常駐デーモン / サーバ運用」の行を改訂した。常駐するのはメニューバーアプリで、`run` はその子プロセスとしてローカルに起動されるライフサイクルのままである
- プロトコルは 0.7.7 になる（加算的・patch。`Capabilities` を構造体リテラルで組むコードには source break）
- リポジトリに Swift と XcodeGen が入り、CI に macOS ランナーのジョブが増える（`apps/macos/` を触った PR だけでビルドする）
- 機密を `--secrets-stdin` で渡す構成では、ターミナルから単独で `doctor` などを叩くと `secret:` を解決できない（ADR-0100 の帰結のまま）
- 未確認のまま残すもの: 更新後に一度も起動せずに再ログインしたときのログイン項目の挙動
