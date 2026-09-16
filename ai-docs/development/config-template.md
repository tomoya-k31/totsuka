---
type: Guide
title: config.toml 雛形とその網羅性検査
description: "totsuka が書き出す config.toml 雛形の置き場（crates/orchestrator-cli/templates/config.toml）と、そこに全設定キーが載っていることを機械検証する scripts/config-template-lint.sh の仕組み・キーを増減したときの手順。Rust の文字列リテラルではなく実ファイルに置く理由（クレート境界を跨いで検査できる唯一の場所）も含む。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/scripts/config-template-lint.sh
tags: [config, toml, template, lint, fitness-function, ci]
generated: { by: claude-code/opus-5, at: 2026-09-17T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 雛形の置き場

`totsuka` が書き出す `config.toml` の雛形は **`crates/orchestrator-cli/templates/config.toml`** にある。
`orchestrator_cli::init_cmd` が `include_str!` でバイナリに焼く（`orchestrator_core::hooks` が
7 本のフックスクリプトに使っているのと同じ手口）。

雛形の全キーの意味・既定値は [設定リファレンス](/development/config-reference.md) を、
貼って動く組み合わせは [設定例集](/development/config-examples.md) を参照。

## なぜ Rust の文字列リテラルではないのか

網羅を機械検証できる場所が、**ファイルしか無いため**である。

**Rust には struct のフィールドを列挙する手段が無い。** リフレクションが無く、導出マクロを新設しない
限り、テストは「このキーの一覧」を手で書き写すことになる —— 写した一覧こそが次にズレるものなので、
検査の意味が消える。ソースをテキストとして読めば、その一覧は書き写さずに得られる。

副次的な理由として、雛形に載るべきキーの過半は `plugins/*` の config struct が決めるが
（`[github]` / `[slack]` / `[notion]` / `[herdr]` …）、`orchestrator-cli` がそれらに張っている依存は
github / slack の 2 本だけで、しかも dev-dependency である
（[ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md)）。検査のために 7 本ぶん
張ると、CLI の dev ビルドに全プラグインが入る。

雛形をファイルにすると、どちらの問題も踏まずに、両方をテキストとして読める場所（シェルスクリプト）から
照合できる。

# 網羅性検査（`scripts/config-template-lint.sh`）

[ワークスペース依存境界ルール](/architecture/workspace-dependency-rules.md) の `arch-lint.sh` と
同じ fitness function の枠組みに置く。違反 1 件以上で exit 1、前提ツール欠如・検査自体の失敗で exit 2。

| チェック | 内容 |
|---|---|
| `missing-key` | config struct にあるフィールドが雛形に現れない（足し忘れ） |
| `unknown-key` | 雛形にあるキーがどの config struct にも無い（タイポ・削除残り） |

**双方向に見るのが要点である。** 片方向（足し忘れのみ）だと、削除・改名されたキーが雛形に残り続け、
そこからコピーしたユーザーの `config.toml` が `totsuka config validate` で落ちる。

## 何をどう抽出しているか

- **コード側**: `crates/orchestrator-core/src/config/schema.rs` と `plugins/*/src/config.rs`。
  プラグインはパスのグロブで拾うので、**新プラグインを足してもこのスクリプトの更新は要らない**
  （`arch-lint.sh` が `plugins/` 配下をパスで判定しているのと同じ方針）。
  - `Deserialize` を導出する `pub struct` / `pub enum` の本体だけを読む。これを見ないと
    `ConfigError::EnvOverride { var, reason }` のようなエラー enum のフィールドまで設定キーとして数える
  - `pub x: T` に加えて、enum の struct variant の中の `x: T` も拾う。後者は
    `cleanup = { retention_days = 5 }` のように TOML のキーとして書かれるが `pub` が付かない。
    **行単位で読むので、1 行に畳まれた variant（`Retention { retention_days: u32 }`）は拾えない** ——
    rustfmt がフィールド付き variant を展開する前提に乗っている
  - `#[derive(…)]` は `)]` が来るまで読む。1 行しか見ないと、rustfmt が折り返した瞬間にその型の
    フィールドが丸ごと検査から消える（fail-open）。非 pub の型に付いた derive は次の型へ持ち越さない
  - `#[serde(flatten)]` は除外し（`plugin_settings` / `options` はキーではなく容れ物）、
    `#[serde(rename = "…")]` は差し替える
  - `#[cfg(test)]` 以降は読まない
- **雛形側**: 行頭の `#` を 1 つ剥がしてから、`key = …` で始まる行（同じ行の inline table の内側キーも）と、
  `[a.b]` / `[[a]]` のテーブル見出しの各セグメントだけを拾う。散文のコメントを誤って拾わないための制限。
- **方向によって見出しの扱いが違う**。`missing-key`（コード → 雛形）は見出しも「載っている」と数えるが、
  `unknown-key`（雛形 → コード）は**代入行のキーだけ**を見る。末尾のセグメントがユーザーの決める
  インスタンス名になる見出しがあるためで（`[tools.claude]` / `[notion.dynamic.sprint]` /
  `[macos.filter.workflows.<name>]`）、これらを「どの struct にも無いキー」として報告させない。
  見出しの綴り間違い自体は `totsuka config validate` が別途弾く —— ロスターに無い名前の
  トップレベルテーブルは検証エラーになる。
- **自由キーのテーブルは例を工夫して避ける**。`[herdr.kind_map]`（実行ファイル名 → kind）と
  `[notion.priority_map]`（option 名 → 数値）はキーがスキーマでは決まらないので、雛形の例には
  ハイフンを含む名前や大文字始まりの名前を使う（抽出器は `^[a-z_][a-z0-9_]*$` にしか反応しない）。
- **inline table の走査は `{` 〜 最後の `}` に閉じ込める**。`cleanup = { retention_days = 3 }` の
  内側は設定キーだが、値の後ろに続く散文（`# … unset = no -activate`）は設定キーではない。
- **`lint:raw` を含む行は読まない**。第三者の DSL をそのまま渡す値（`trigger.filter` に書く
  Notion のフィルタ）は、totsuka の設定スキーマではないので数えない。

# 3 つの宣言リスト

スクリプト冒頭に 3 つある。**どれも「理由付き 1 行」で書く** —— 忘れと意図の区別が検査の主眼なので、
理由なしで足すとその区別が消える。

| リスト | 向き | 何を宣言するか |
|---|---|---|
| `TEMPLATE_EXEMPT` | コード → 雛形 | struct にはあるが、雛形に**載せない**と決めたキー |
| `OPAQUE_ALLOWED` | 雛形 → コード | 雛形に書くが、**どの struct にも現れない**キー。`[[workflows]].trigger` と、プラグインが `[[workflows]]` へフラットに足す追加プロパティは core が `toml::Table` のまま保持して渡すので（#554）、解釈するコードはあってもフィールドは無い |
| `lint:raw`（行内マーカー） | 両方向 | その行を丸ごと読まない。第三者の DSL 用 |

# 雛形は 1 行も有効行を持たない

生成しただけでは**何も設定されない**のがこのファイルの契約である。だから `totsuka init` 直後の
`config.toml` は読み込めて、中身はすべて人間がコメントを外すまで説明文でしかない。

400 行のコメント例を編集している最中に有効行が 1 本紛れ込むと、それはそのリリース以降 `init` を
走らせた全員に効く。`orchestrator_cli::init_cmd` のユニットテスト
`the_skeleton_activates_nothing` が、雛形を TOML としてパースして**空のテーブル**になることを
検査する —— 網羅性検査はファイルをテキストとして読むので、コメント行と有効行を区別できない。
両方あって初めて「全部載っていて、何も効いていない」が言える。

この照合は **「Rust のフィールド名 ≒ TOML のキー名」** に寄りかかっている。config の struct は
フィールドに `serde(rename)` をほぼ使っていないため（`rename_all` は enum の変種名向け、
フィールドの `rename` は `keep_7d` / `keep_28d` のみ）実用的な精度が出る。

# キーを増減したときの手順

1. `schema.rs` または `plugins/*/src/config.rs` を直す
2. `crates/orchestrator-cli/templates/config.toml` に**コメント付きで**追記する（または消す）
3. [設定リファレンス](/development/config-reference.md) の表を直す
4. `bash scripts/config-template-lint.sh` を 0 error にする

雛形に**載せない**ことを意図して選ぶ場合だけ、スクリプト冒頭の `TEMPLATE_EXEMPT` に
`<key>=<理由>` を 1 行で書く。**このリストの存在が検査の主眼である** —— 載せ忘れと、意図した除外とを
区別できるようにするためにあるので、理由なしで足さないこと。
