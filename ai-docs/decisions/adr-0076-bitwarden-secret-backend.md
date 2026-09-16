---
type: Decision
title: ADR-0076 シークレット参照に Bitwarden (bw:) を第 5 のスキームとして追加する
description: "設定のシークレット参照へ bw:<item>/<field> を第 5 のスキームとして追加し、解決は公式 Bitwarden CLI（bw get <field> <item> --nointeraction）へのシェルアウトで行う決定。bw:// を採らない理由（Bitwarden に native URI が存在しない）、分割を最後の / にする理由、カスタムフィールドを射程外にして cmd: へ委ねる理由、BW_SESSION 未設定を spawn 前に検知して無人ハングを防ぐ設計、doctor ゲートをスキーム非依存に畳んでから足した理由を記録する。"
resource: https://github.com/tomoya-k31/totsuka/issues/699
tags: [decision, secrets, bitwarden, config, security, adr]
generated: { by: claude-code/opus-5, at: 2026-09-16T23:55:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-699
    resource: https://github.com/tomoya-k31/totsuka/issues/699
    title: "feat(core): シークレット参照に Bitwarden (bw:) バックエンドを追加する"
  - id: bw-cli
    resource: https://bitwarden.com/help/cli/
    title: "Bitwarden CLI — get / status / global options"
---

# Status

stable（[#699](https://github.com/tomoya-k31/totsuka/issues/699)）。[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)（`op://`）と [ADR-0044](/decisions/adr-0044-cmd-secret-scheme.md)（`cmd:`）の非対話原則を継承する。

# Context

パスワードマネージャとして一級市民なのは 1Password だけで、Bitwarden 利用者は同等の体験を得られなかった。

**前提として、`cmd:bw get password <item>` は ADR-0044 以降すでに書ける。** ADR-0044 自身が「`op://` の解決は実体として `op read` へのシェルアウトであり、`cmd:` はその一般化である」と書いている以上、「別ツールから秘密を取って起動する」だけなら追加実装は要らない。したがって専用スキームは、`cmd:` が**構造的に持てないもの**でしか正当化されない:

- **doctor**: `cmd:` を含むプラグインの probe は #289 の非対話原則で**無条件 skip** される（doctor はそのコマンドが対話プロンプトを出すか判別できない）。`bw` は `bw status` で状態を非対話に読めるので、`op whoami` と同精度のゲートが作れる。`cmd:` のままではこの精度を永久に捨てることになる
- **エラーの次アクション**（§7）: locked / 未ログイン / 複数ヒットを分類して具体的な回復手順を出せる
- **setup ウィザードの導線**

# Decision

## 1. 参照構文は `bw:<item>/<field>`

**`bw://` は採らない。** ADR-0006 が `op://` を採用した理由は「ユーザーが既に持つ native URI をそのまま貼れる」ことであり、`op://` は `op read` が実際に受理する本物の URI である。**Bitwarden に URI 表記は存在しない** —— `bw get` は object 名と item を引数に取るだけなので、`bw://` を作ることは存在しない URI の発明になる。これは ADR-0044 が `cmd://` を不採用にしたのと同じ理由（「`//` は op の native URI 由来で、こちらには意味がない」）である。`keychain:` と同じ「接頭辞 + 中身」形式に揃える。

`bw get` に vault 次元は無いので、`op://<vault>/<item>/<field>` の 3 セグメントも真似ない（埋めるだけの階層になる）。

## 2. 分割は**最後の** `/`

`<item>` は `bw get` に渡すものそのもの（item id か検索文字列）で、`github.com/myorg` のように `/` を含みうる。一方 `<field>` は `bw get <object>` の閉じた語彙で `/` を含まない。したがって最後の `/` で割る。

**これは `keychain:` と規則が逆である**（`keychain:<service>/<account>` は最初の `/` で割り、`/` を含みうるのは account 側）。同じ形に揃えると `/` を含むアイテム名が永久に参照できなくなるので、一貫性より到達可能性を採った。ドキュメントに明記する。

## 3. `<field>` は公式 CLI の指定方法に揃え、eager 検証しない

`bw:item/password` は `bw get password item` になる。語彙（`item|username|password|uri|totp|exposed|…`）を totsuka 側にハードコードしない —— Bitwarden が object を増やしたとき totsuka のリリースを待たせることになる。未知の object は `bw` 自身が報告する（ADR-0006 の「存在検証は CLI の仕事」を語彙にも適用）。

## 4. カスタムフィールドは v1 の射程外

公式 CLI に単一コマンドの取得手段が無く（`bw get item` の JSON を jq で引く必要がある）、「公式 CLI と指定方法を揃える」という上の原則から外れる。必要な場合は **`cmd:` を使う** —— `cmd:bw get item x | jq -r '.fields[]|select(.name=="y").value'` は今日そのまま書ける。ADR-0044 が定めた責務分担をそのまま使う形であり、専用スキームで JSON 経路まで抱え込むと `cmd:` が既に埋めている領域を二重に持つことになる。

## 5. アンロックは環境の `BW_SESSION` 継承。spawn **前**に検知する

`op` はデスクトップアプリが解錠できるセッションを持つが、`bw` は持たない。`bw unlock` が吐くセッションキーを `BW_SESSION`（または `--session`）で渡す必要があり、**セッションが無いと `bw` は stdin でマスターパスワードを対話プロンプトする**。

`totsuka run` は常駐プロセスなので、これを踏むと**画面に何も出ないまま停止する**（`op` の biometric プロンプトと違い、気づく手がかりが無い）。したがってバックエンドは **spawn する前に `BW_SESSION` を検査**し、無ければ回復手順付きのエラーで落とす。加えて `--nointeraction` を必ず渡し、検査と spawn の間に失効したセッションもハングではなくエラーになるようにする。

**不採用**: 設定に `session_ref` を置いてセッションキーを保管庫から渡す案。セッションキーは失効するので、失効するものを保管庫へコピーする形は ADR-0044 が「コピーは黙って死ぬ」と名指しして蹴った構造そのものである。**不採用**: マスターパスワード参照からの自動 unlock。vault 全体のマスターパスワードを設定経路に載せることになり、リスクが段違いで、解決のたびに KDF が走る。

無人運用は、1Password の Service Account 対応と同じく後続 issue に送る。Bitwarden 側だけ無人運用まで面倒を見るのは非対称である。

## 6. 不変条件は既存バックエンドと同一

stdout は平文なので即 `SecretString` に包み、ログにもエラーにも出さない（§5.2）。分類は stderr のみ。`bw` に `--no-newline` 相当が無いので末尾改行は `cmd:` と同じく resolve 側で除去する。exit 0 かつ空出力は起動時エラー（空トークンで API を叩いて謎の 401 になるより落とす）。タイムアウトは付けない（既存 2 バックエンドに無い。ADR-0044 の一貫性判断を踏襲）。

## 7. doctor は `bw --version` + `bw status`

`bw status` は `op whoami` の対応物で、状態を報告するだけでプロンプトしない。ただし **`bw status` は locked でも exit 0 を返す**ので、判定は JSON の `status` フィールドが `unlocked` であること。exit code を見ると locked を ready と誤判定し、ゲートされた probe をそのまま stdin プロンプトへ送り込む。**パースできない出力は locked 扱い**（probe を 1 つ飛ばすコストより、無人実行がプロンプトで止まるコストの方が高い）。

## 8. ゲートを畳んでから足す（先行リファクタ）

追加の**前に**、doctor の非対話ゲートをスキーム非依存に一般化した。各 probe が自前で `starts_with("op://")` / `starts_with("cmd:")` を見る形だったため、#444 で `cmd:` を足したとき 3 経路のうち 1 経路にしかゲートが入らず、残り 2 経路を後から塞いでいる。

一般化後は `SecretScheme::of` が参照文字列を `SecretRef` へ parse して分類し、その `match` は `SecretRef` に対して**網羅的**である。core にスキームを足すと doctor がコンパイルエラーになるので、「doctor に教え忘れる」ことが可能ではなくなる。同じ理由で、resolver が持っていた独立の接頭辞リストも `is_secret_reference`（parser から導出）に置き換えた。

この作業で、**報告に見えて実はゲートだった 2 箇所**（`hook-token` / `llm`）も判明した。接頭辞にマッチしなかった参照は最後の腕で実解決されるので、`bw:` をここで忘れていれば `bw` が起動して stdin で待っていた。

# Consequences

- 非 macOS で動くシークレットストアが 2 つ目になる（`keychain:` は従来どおり macOS 専用）
- `bw:` で始まる平文値が書けなくなる（`op://` / `cmd:` と同種の制約追加）
- 解決は参照ごとに `bw get` を起動する（`op://` と同じ per-ref のプロセス起動コスト）
- **`bw` のセッションはシェルに紐づく**ので、`totsuka run` は `bw unlock` して `BW_SESSION` を export したシェルから起動する必要がある。launchd 等での常駐は無人運用対応（後続）まで `keychain:` / `${ENV}` / `cmd:` を使う
- `bw get` は 1 件しか返せないため、アイテム名が曖昧だとエラーになる。エラーは item id で指定するよう案内する
- `SecretError::BackendUnavailable` は #699 の地ならしで `install_hint` を持つようになった。これが無いと Bitwarden 利用者に「`brew install 1password-cli` しろ」という誤った案内が出る

# 不採用案

| 案 | 不採用理由 |
|---|---|
| `cmd:bw …` のレシピをドキュメントに書くだけ | 実装ゼロで済むが、doctor の精度（`bw status` による非対話ゲート）とエラーの次アクションを永久に捨てる |
| `bws`（Bitwarden Secrets Manager CLI） | アンロック不要で無人運用に強いが**別製品**で、個人 vault のパスワードは入っていない。「普段使っている Bitwarden の中身を参照したい」という期待に応えられない |
| `rbw`（非公式クライアント + 常駐 agent） | `BW_SESSION` 問題が消え UX は `op` に一番近いが、非公式ツールを一級市民として名指しすることになる |
| `bw://<item>/<field>` | `op://` と見た目は揃うが、Bitwarden に存在しない URI 表記の発明になる |
| `<field>` の語彙を eager 検証 | タイポを parse 時に弾けるが、`bw` が object を増やすたび totsuka のリリースが必要になる |
