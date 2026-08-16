---
type: Playbook
title: セットアップ Playbook（新マシン / 開発機 / ローテーション / 復旧）
description: "ゼロから totsuka が動くまでを通しで示す導入手順。新マシン（tarball 配置 → totsuka setup → シークレット登録 → doctor → run）、開発機（クローン → --from-source）、トークンローテーション、中断・失敗時の復旧を扱う。"
resource: https://github.com/tomoya-k31/totsuka/issues/350
tags: [setup, onboarding, runbook, playbook, secrets, doctor, rotation]
generated: { by: claude-code/opus-5, at: 2026-08-01T09:40:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/setup-playbook.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/setup-playbook.md docs/setup-playbook.ja.md -->

# このドキュメントの位置づけ

「ゼロから動くまで」を**通しで**示す唯一の場所。個別の話題は既存のランブックが持っており、ここはそこへの導線を兼ねる。

| 知りたいこと | 行き先 |
|---|---|
| 各設定キーの意味 | [設定リファレンス](/development/config-reference.md) |
| シナリオ別の config 例 | [設定例](/development/config-examples.md) |
| Slack アプリの作成と scope | [Slack Quickstart](/operations/slack-quickstart.md) |
| doctor の読み方・worktree 掃除 | [運用ガイド](/operations/operations-guide.md) |
| プラグインを自作する | [プラグイン開発ガイド](/development/plugin-dev-guide.md) |

前提として macOS。`totsuka setup` の設計判断は [ADR-0028](/decisions/adr-0028-setup-wizard.md)。

# 新マシン

## 1. 配置

[最新リリース](https://github.com/tomoya-k31/totsuka/releases/latest) の macOS ユニバーサル tarball を落とす。**ツリーごと**置くこと — `totsuka` は同梱プラグインを自分の隣から探すので、バイナリだけ移すと `setup` がプラグインを見つけられない。

```bash
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

`xattr` を忘れると Gatekeeper が**プラグインの起動だけ**を黙って殺し、`doctor` は「crashed or exited」としか言えない。本体は動くので原因が見えにくい。

## 2. `totsuka setup`

```bash
totsuka setup
```

聞かれるのは 4 種類だけで、それ以外はレシピが持つ:

1. **どのレシピから始めるか**（GitHub 最小構成 / 設計→実装ハンドオフ / Slack 本人名義返信 / 人間検収必須）
2. **リポジトリのパスと名前**（複数可）
3. **シークレットをどこに置くか**（Keychain / 1Password / 環境変数）— **値そのものは一切聞かれない**
4. レシピが要求する穴だけ（GitHub Project の owner / owner_type / 番号 / 自分の login、Slack のメンバー ID、LLM の model）

計画が印字され、確認するとそこから先が副作用を持つ。**質問中の Ctrl-C は何も残さない。**

`setup` は続けて `plugins/<name>.toml` の生成 → プラグインの install + enable → `doctor` まで走る。`init` を先に打つ必要はない（ディレクトリ作成は `setup` が内包する）。

## 3. シークレットを登録する

`setup` の最後にチェックリストが出る。各行が「どの参照名」「何を可能にするか」「登録コマンド」を持つので、そのままコピペする:

```bash
security add-generic-password -U -s totsuka -a github-token -w '<paste the value>'
```

**ここに出た参照はすべて必須**である。config が参照している以上、1 つでも欠けるとそのプラグインは起動しない。「任意の機能だから飛ばしてよい」ものは、そもそもチェックリストに出ない。

> Slack の `slack-bot` は例外に見えるが必須。プラグイン単体では opt-in（無ければナッジ無し）だが、**本人名義の返信は Slack 通知を一切上げない**ため、レシピはナッジ前提で構成されている（[ADR-0021](/decisions/adr-0021-slack-bot-notification-nudge.md)）。

## 4. 検証して走らせる

```bash
totsuka doctor          # 未登録シークレットが残っていれば exit 3 で教える
totsuka run --dry-run   # どのタスクがどのリポジトリのどのエージェントに行くか
totsuka run --watch
```

`doctor` の `state-db` は `totsuka run` を一度も実行していなければ fail する。これは正常で、`run` 後に消える。

## 5. 一回きりの対話セットアップ（該当するときだけ）

`setup` が代行できない、対象ツール側の初回操作。

| 対象 | 必要な操作 | 参照 |
|---|---|---|
| Codex | TUI で **hooks trust** を承認。**しないとフックが黙ってスキップされ、全タスクが timeout する** | [Codex ツールのセットアップ](/operations/codex-tool-setup.md) |
| OpenCode | 初回起動と config 配置 | [OpenCode ツールのセットアップ](/operations/opencode-tool-setup.md) |
| 1Password | `op signin`（`op://` 参照を使う場合） | [ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) |
| 通知クリック | `terminal-notifier` の導入と bundle id | [click-to-focus セットアップ](/operations/click-to-focus-setup.md) |

# 開発機

チェックアウトからビルドして入れる。tarball は要らない。

```bash
git clone https://github.com/tomoya-k31/totsuka
cd totsuka
cargo build --release --workspace --bins
totsuka plugin install --from-source --all --enable
totsuka setup
```

`--from-source` は cwd から上へ「Cargo ワークスペースのルート**かつ** `plugins/` を持つ」ディレクトリを探す。別リポジトリの中で打っても誤検出しない。`totsuka setup` をチェックアウト内で打つと、同梱ツリーが無い場合は自動で `--from-source` を選ぶので、上の 2 コマンドは実質 1 つにまとめられる。

プラグインを 1 つ直したときの再導入も同じ経路:

```bash
totsuka plugin install --from-source slack --enable
```

`--print-plan` を付けると cargo を起動せず、何をビルドしてどこから入れるかだけ印字する。

# トークンローテーション

## Slack — scope を変えたら 2 本とも再発行される

**これが一番踏みやすい。** Slack アプリの scope を変更すると再インストールが必要になり、`xoxp-`（User）と `xoxb-`（Bot）が**両方**新しくなる。片方だけ Keychain を更新すると、更新しなかった側の機能だけが壊れる:

```bash
security add-generic-password -U -s totsuka -a slack-user -w 'xoxp-…'
security add-generic-password -U -s totsuka -a slack-bot  -w 'xoxb-…'
```

`xapp-`（App-Level Token）は再インストールでは変わらない。明示的に再生成したときだけ更新する。

scope 自体の落とし穴として、`reactions:read` / `channels:read` / `groups:read` が欠けると**イベントが届かないだけでエラーも出ない**。詳細は [Slack Quickstart](/operations/slack-quickstart.md)。

## 全般

`setup` を再実行する必要はない。参照名は変わっておらず、変わったのは値だけなので、`security add-generic-password -U`（`-U` = 既存を更新）で上書きして `totsuka doctor` を打てばよい。

# 中断・失敗時の復旧

## 途中で失敗した

**再実行すれば揃う。** `setup` は原子性ではなく収束性を保証しており、各ステップが冪等になっている。どこまで適用したかは印字される。

```bash
totsuka setup
```

既存の設定ファイルはスキップされるので、2 回目は実質「プラグイン導入と doctor だけ」が走る。これが `--repair` フラグを用意していない理由（[ADR-0028](/decisions/adr-0028-setup-wizard.md)）。

## 設定を作り直したい

`setup` は既存ファイルを上書きしない。作り直すなら自分で退避する:

```bash
mv ~/.config/totsuka/config.toml{,.bak}
mv ~/.config/totsuka/plugins ~/.config/totsuka/plugins.bak
totsuka setup
```

**例外**: `totsuka init` が吐いた「全行コメント」の雛形だけは未設定として扱われ、`setup` が中身を埋める。退避は要らない。

## 同じ設定を別マシンで再現したい

回答ファイルを保存して持っていく。**機密は構造的に入りえない**（回答の型が `deny_unknown_fields` で、トークンを表すフィールドが存在しない）ので、dotfiles に置いても安全:

```bash
totsuka setup --save-answers ~/dotfiles/totsuka-answers.toml
```

読み込む側:

```bash
totsuka setup --answers ~/dotfiles/totsuka-answers.toml --yes
```

シークレットの登録だけは各マシンで人間がやる。

**別マシン・別バージョンで読まれる前提のファイルなので、形式は契約として扱う**（#466）:

- 意味が変わる変更では `version` を上げ、**版が違うファイルは推測せず拒否する**（`→ regenerate it by running totsuka setup interactively` を案内）。版はファイルの他の部分より先に読むので、フィールドの型が変わった版でも「version が違う」と言える
- `recipe` は**安定キー**（`recipe = "minimal-github-herdr"`）であってメニュー位置ではない。位置だと、レシピを 1 つ挿入するだけで既存ファイルが黙って隣のレシピを選ぶ — 範囲チェックは通り、`version` も動かないので誰も気づけない
- 存在しないキーを書いたときのエラーは、実在するキーを列挙する

## `doctor` が赤いまま

読み方は [運用ガイド](/operations/operations-guide.md)。導入直後に出やすいものだけ:

| チェック | よくある原因 |
|---|---|
| `state-db` | まだ `totsuka run` を打っていない（正常） |
| `plugin:<name>` — secret not found | チェックリストの登録漏れ |
| `plugin:<name>` — crashed or exited | `xattr -dr com.apple.quarantine` の実行漏れ |
| `bundled-plugins`（warning） | `cargo install` 由来のビルドで同梱ゼロ。`--from-source` を使う |
| `hook-token`（warning） | `[hooks].auth_token_ref` 未設定。フック対応エージェントを使う前に設定する |
