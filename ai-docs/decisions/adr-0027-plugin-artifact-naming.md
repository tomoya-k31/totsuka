---
type: Decision
title: ADR-0027 プラグインの bin 名は plugin.toml の name に一致させる
description: "プラグインの Cargo bin 名と plugin.toml の name が全 6 個で食い違い、install のたびに手作業のリネームと dist ディレクトリ組み立てを強いていた問題に対し、bin 名を name 側へ揃える決定。target/{profile}/<name> がそのまま install 可能・配布可能になる。plugin.toml への binary フィールド追加と store 側での緩和は不採用。再発防止は arch-lint の plugin-bin-name チェックで行う。"
resource: https://github.com/tomoya-k31/totsuka/issues/343
tags: [decision, plugins, packaging, distribution, fitness-function, adr]
generated: { by: claude-code/opus-5, at: 2026-07-31T20:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。[#343](https://github.com/tomoya-k31/totsuka/issues/343) の実装とともに確定した。エピック [#342](https://github.com/tomoya-k31/totsuka/issues/342)（インストール・セットアップの摩擦をゼロにする）の最初の 1 段で、後続の同梱配布・`--bundled` / `--from-source` はすべてこの決定を前提にする。

[ADR-0011](/decisions/adr-0011-arch-fitness-function.md)（ワークスペース不変条件の Fitness Function）を拡張する。

# Context

同梱プラグイン 6 個すべてで、Cargo の bin 名と `plugin.toml` の `name` が食い違っていた。

| ディレクトリ | Cargo パッケージ / 旧 bin 名 | `plugin.toml` の `name` |
|---|---|---|
| `plugins/task-source-github` | `task-source-github` | `github` |
| `plugins/task-source-slack` | `task-source-slack` | `slack` |
| `plugins/task-source-notion` | `task-source-notion` | `notion` |
| `plugins/agent-ide-herdr` | `agent-ide-herdr` | `herdr` |
| `plugins/agent-ide-orca` | `agent-ide-orca` | `orca` |
| `plugins/notifier-macos` | `notifier-macos` | `macos` |

`plugins::store::prepare_install` は「`plugin.toml` の `name` と同名のバイナリがソースディレクトリ内にあること」を要求し、`commit_install` も `<plugin dir>/<name>` として配置する。つまり**インストール後のレイアウトは `name` 側で固定されている**。にもかかわらずビルド成果物は Cargo パッケージ名で出るため、導入のたびに次の手作業が必要だった。

```bash
cargo build --release -p task-source-slack
mkdir -p dist/slack
cp target/release/task-source-slack dist/slack/slack   # ← リネーム
cp plugins/task-source-slack/plugin.toml dist/slack/
totsuka plugin install ./dist/slack
```

実害は 2 つ。

1. **README の Quickstart が成立していなかった。** `totsuka plugin install ./path/to/task-source-github` と書かれていたが、そのディレクトリにバイナリは存在せず、コピーしてそのまま実行すると必ず失敗する。
2. **リリースへの同梱が書けない。** ビルド成果物の名前と配布時の名前が違うため、ワークフローにマッピング表を持たせるか、プラグインごとに個別の `cp` を並べる必要がある。プラグインを増やすたびにワークフローを編集することになる。

`plugin.toml` の `name` 側は動かせない。これは `[plugins.<name>]` の設定キー、ワークフローの `source` / `agent`、`plugins/<name>.toml` のファイル名、ストアのディレクトリ名、起動パスすべての識別子であり、変更すると既存ユーザーの設定が全部壊れる。

# Decision

## 1. `[[bin]] name` を `plugin.toml` の `name` に揃える

Cargo パッケージ名は据え置き、bin ターゲット名だけを変更する。`target/{profile}/<name>` がそのまま install 可能・配布可能な名前になり、リネーム工程が消える。

プラグインは `[[bin]]` と暗黙の lib を両方持ち、lib 名（`agent_ide_orca` 等）はパッケージ名由来なので `main.rs` の `use` は影響を受けない。

## 2. 「ソースの名前」と「インストール後の名前」の規則を 1 本に保つ

インストール後のレイアウトが `<plugin dir>/<name>` である以上、ソース側も同じ規則にする。2 つの命名規則が併存する状態を作らない。

## 3. 再発防止は arch-lint に持たせる

`scripts/arch-lint.sh` に `plugin-bin-name` チェックを追加した。`plugins/*` の各パッケージについて「bin ターゲットがちょうど 1 つあり、その名前が同ディレクトリの `plugin.toml` の `name` と一致する」ことを `cargo metadata --no-deps` から検証する。新しいプラグインを足したときに黙って不一致が復活するのを防ぐ。

規約だけでは守られないことは実証済み（6 個中 6 個が食い違っていた）なので、機械検証に落とす。CI では既存の `clippy / rustfmt` ジョブ内のステップとして走る（[ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) の「ジョブを増やさずステップを足す」に従う）。

# 検討した選択肢

## `plugin.toml` に `binary = "task-source-github"` を追加する

不採用。`Manifest` は `plugin-protocol` クレートの `deny_unknown_fields` 構造体、つまり**バージョン付きのワイヤ契約**である。フィールド追加はプロトコル変更にあたり、互換性チェック（F-54）の対象になる。

加えて、「ソースのファイル名」と「インストール後のファイル名」が乖離しうる設計を正式に認めることになり、サードパーティのプラグイン作者にも同じ選択を強いる。解くべき問題は「不一致をどう記述するか」ではなく「不一致をなくすこと」だった。

## store 側で緩和する（ディレクトリ内で唯一の実行可能ファイルを採用する等）

不採用。曖昧さが増える。`validate_plugin_name` によるパストラバーサル防止と「`name` と同名のバイナリが 1 つ」という不変条件は、現状セットで読めるようになっている。「実行可能ファイルを探す」ロジックを挟むと、この読みやすさが失われるうえ、ディレクトリに余計なファイルが混ざったときの挙動を新たに定義する必要が出る。

## Cargo パッケージ名ごと変える（`task-source-slack` → `slack`）

不採用。`Cargo.lock`・`cargo-deny`・`cargo-machete`・ワークスペースメンバー宣言・`-p` を使う全コマンドに波及する一方、得られるものは bin 名の変更だけで足りる。パッケージ名は開発者向けの識別子として `task-source-` / `agent-ide-` / `notifier-` の接頭辞で種別が読めるほうがよい。

# Consequences

## 良くなること

- `cargo build --release -p task-source-slack` の出力 `target/release/slack` がそのまま install できる。リネームと dist ディレクトリ組み立てが不要になる
- リリースワークフローが `plugins/*/plugin.toml` の `name` を舐めるループで書ける。プラグインを追加してもワークフローの編集が要らない（[#344](https://github.com/tomoya-k31/totsuka/issues/344)）
- `--from-source`（[#346](https://github.com/tomoya-k31/totsuka/issues/346)）がマッピング表なしで書ける
- README の Quickstart を実際に動くコマンドにできる

## 受け入れるコスト・リスク

- **`target/{profile}/orca` と `target/{profile}/herdr` が外部 CLI と同名になる。** とくに `agent-ide-orca` は外部の `orca` CLI を PATH ルックアップで spawn する（`orca_bin`、既定 `orca`）ため、target ディレクトリを PATH に入れている環境では自分自身を起動しうる。該当する場合は `orca_bin` に絶対パスを設定する。各 `Cargo.toml` の `[[bin]]` にこの注意をコメントで残した
- `target/` に旧名のバイナリが残る。cargo は古い成果物を掃除しないので、`cargo clean` するまで `target/debug/task-source-slack` 等が居座る。参照する経路はもう無いため無害
- `sibling_bin(package, bin)` を使うテストは `bin` 引数の更新が必要（`crates/orchestrator-cli/tests/slack_e2e.rs` の 1 箇所のみ）

# 実装

- `plugins/*/Cargo.toml` × 6 — `[[bin]] name` を変更し、不変条件の理由をコメントで併記
- `scripts/arch-lint.sh` — `plugin-bin-name` チェック
- `crates/orchestrator-cli/tests/slack_e2e.rs` — `build_bin("task-source-slack", "slack")`
- `.github/workflows/ci.yml` — 兄弟バイナリを列挙するコメントの追随
