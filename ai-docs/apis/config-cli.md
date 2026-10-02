---
type: API Endpoint
title: totsuka config schema / get / set / unset（設定画面向けの CLI 契約）
description: メニューバーアプリの設定画面が config.toml を読み書きするための CLI 契約（ADR-0109）。schema は core（schemars）と各プラグイン（config/schema）のスキーマを 1 つのルートスキーマにまとめ、get はファイルの中身を JSON で返し、set / unset は JSON Pointer のキーパスで 1 キーずつコメントを保ったまま書き換える。読めていたファイルを読めなくする書き込みは拒否し、シンボリックリンクは辿って書く。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/src/config_cmd.rs
tags: [api, cli, config, json-schema, menubar, macos]
generated: { by: claude-code/opus-5.5, at: 2026-10-01T22:56:00+09:00 }
status: stable
owner: tomoya-k31
---

# 概要

メニューバーアプリ（[ADR-0109](/decisions/adr-0109-native-menubar-app.md)）は TOML を自分で扱わない。`config.toml` の読み書きは次の 4 つのサブコマンドを子プロセスとして呼んで行う。どれも `--config` と `hosts/<host>.toml` の選択（[ADR-0106](/decisions/adr-0106-per-host-config-file.md)）に従い、実際に読まれるファイルを対象にする。

エラーは常に stderr の 1 行の JSON エンベロープ `{"error":{"message":…,"action":…}}` で返る（`--json` を付けたほかのコマンドと同じ形）。

# Schema

## `totsuka config schema`

```json
{ "config_path": "/…/totsuka/config.toml", "schema": { "type": "object", "properties": { … } } }
```

- `schema` は JSON Schema（draft 2020-12）。**`$ref` は使わず全部インライン**、doc コメント由来の `title` / `description` は落としてある（開発者向けの文なので）
- core のキーは schemars で `RootConfig` から導出する（`config::json_schema::core_schema`）。serde が読む構造体そのものから作るので、ローダが受け付けないキーは載らない
- **インストール済みの各プラグイン**のテーブルを、その名前のプロパティとして足す。マニフェストで `config_schema` を宣言したプラグインだけを起動し、`initialize` を送らずに `config/schema` を尋ねる（`plugins::plugin_schemas`）。設定ファイルは読まないので、機密が無くても、未設定・未有効でも答えが返る
- プラグインには**並行して**尋ね、1 つあたり 10 秒で打ち切る。止まったプラグインがあっても、待つのは全体で 10 秒程度
- マニフェストが壊れたプラグインは、そのプラグインだけが `x-raw` + `x-schema-error` になる
- **core のキーと同じ名前のプラグイン**（`log` など）は足さない。core の設定を上書きさせないため（その名前は設定の検証でも拒否される）
- **`$ref` を含む答えは `x-raw`** になる。プロトコルはサブスキーマをインラインで書くことを求めている（埋め込むとローカル参照が別の根に対して解決されるため）
- 拡張キーワード:

| キーワード | 型 | 意味 |
|---|---|---|
| `x-title` / `x-help` | `{en, ja}` | ラベルとヘルプ。core の全プロパティに付いている（テストで検査） |
| `x-category` | `{en, ja}` | カテゴリ。core はトップレベルのプロパティに付く。プラグインのテーブルは、無ければプラグイン名が入る |
| `x-secret` | `true` | 機密の参照を持つフィールド（core では `llm.api_key_ref`） |
| `x-placeholder` | string | 未設定のときに入力欄へ薄く出す既定値（例: `max_concurrency` の `4`）。serde が知っている既定値はスキーマ標準の `default` に出るので、これはコードの側で決まる既定値のためにある。ヘルプには既定値を書かない（テストで検査） |
| `x-raw` | `true` | フォームにせず生の TOML として編集する。`config_schema` を宣言していない、または答えが使えなかったプラグインのテーブル |
| `x-schema-error` | string | `x-raw` になった理由（起動失敗・エラー応答・object でない答え） |

1 つのプラグインの失敗でコマンド全体は失敗しない（`x-raw` + `x-schema-error` になるだけ）。

## `totsuka config get`

```json
{ "config_path": "/…/config.toml", "exists": true, "config": { "version": 1, "log": { "level": "info" } } }
```

ファイルを書かれたとおりに返す（`TOTSUKA_*` の上書きは混ぜない。設定画面が編集するのはファイルだから）。ファイルが無ければ `exists: false` と空の `config`。

## `totsuka config set <path> <json>` / `totsuka config unset <path>`

- `path` は **JSON Pointer**（RFC 6901）: `/log/level`、`/repositories/0/tool`、`/tools/my.tool/kind`。キーの中の `/` は `~1`、`~` は `~0` と書く。ドット区切りにしなかったのは、ツール名やプラグイン名に `.` を含められるため
- **数字のセグメントが添字になるのは、そこがすでに配列のときだけ**。テーブルの上では普通のキー（`/tools/123/kind` は `[tools.123]`）。**`-` は配列の末尾への追加**
- `json` は値の JSON（`'"debug"'`、`4`、`true`、`{"name":"a","path":"/x"}`）。`null` は拒否（消すのは `unset`）
- 途中のテーブルが無ければ作る。**配列を作るのは次のセグメントが `-` のときだけ**（`/repositories/-` で `[[repositories]]` を作る）。数字から配列を推測しないのは、テーブルの数字キーと区別できないため
- オブジェクトは `[table]`、オブジェクトの配列は `[[array of tables]]` になる。置き換える値がインラインで書かれていればインラインのまま
- 書き換えは `toml_edit`（`config::set_path` / `config::unset_path`）で、ほかの行のコメント・順序・空白は保つ。値を置き換えたキーの行末コメントも残る
- `unset` で無いものを消そうとしても成功（ファイルはすでに頼まれた状態）

# 呼び出し側の契約

- **読めていたファイルを読めなくする書き込みは拒否する**（型の誤りなど。`RootConfig::from_toml_str` が通るかで判定）。ファイルは変わらない。逆に、すでに読めないファイルは編集できる —— 拒否すると直す手段が無くなる
- 1 キーずつ書くので、途中の状態が `config validate` を通るとは限らない（新しいワークフローに必須キーが揃う前など）。設定が有効かどうかは `config validate` で別に確かめる。アプリは起動の条件にこれを使う
- 書き込みは対象と同じディレクトリの一時ファイルから rename する。**`config.toml` がシンボリックリンクなら辿った先に書く**（dotfiles を Stow で管理していてもリンクが壊れない）。リンク先がまだ無い（壊れたリンク）ときも、リンクを普通のファイルで置き換えず、指す先に作る。既存ファイルのパーミッションは引き継ぎ、失敗したら一時ファイルは消す
- 何も変わらない書き込みはファイルに触れない（ファイルが無いときの `unset` はファイルを作らない）

# 既知の制限

- `[[projects]]` と `[[workflows]]` に書くプラグイン所有のキー（`owner` や `project_number`、`trigger` の中身）は、プロトコルがスキーマを運ばないので載っていない。`trigger` / `on_*` は任意の object として載る
- `toml::Datetime` の値は TOML の内部表現の JSON で返る（設定に日時を書くキーは今のところ無い）

# 関連

- [ADR-0109](/decisions/adr-0109-native-menubar-app.md)
- [plugin-protocol](/components/plugin-protocol.md)（`config/schema`）
- [orchestrator-cli](/components/orchestrator-cli.md)
