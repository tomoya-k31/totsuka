---
type: Decision
title: ADR-0106 ホスト別の config ファイル（hosts/<host>.toml）を自動で選ぶ
description: "dotfiles で ~/.config を全マシン共有すると、ほぼ全体がホスト依存の config.toml を 1 本しか置けない。--config が無いとき hosts/<host>.toml があればそれを、無ければ config.toml を読む方式にした決定。include/merge・TOTSUKA_HOST・macOS の LocalHostName は採らなかった。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/src/common.rs
tags: [decision, config, cli, dotfiles, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-27T10:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。#832 で実装。

# Context

dotfiles（GNU Stow）で `~/.config` を全 PC で共有していると、`$XDG_CONFIG_HOME/totsuka/config.toml` は 1 本しか置けない。ところが中身は `[[repositories]]` のパス・プラグインのロスター・workflows など、ほぼ全体がホストに依存する。切り替える手段は `--config` だけで、`TOTSUKA_*`（[ADR-0009](/decisions/adr-0009-env-override-whitelist.md)）は許可リストに載ったスカラー値の上書きしかできない。

# Decision

`--config` が無いとき、`Cx::resolve` が次の順に 1 本だけ選ぶ。全コマンドがここを通るので、変更点はこの 1 か所になる。

1. `--config <path>`
2. `<config_dir>/hosts/<host>.toml`（存在すれば）
3. `<config_dir>/config.toml`

- `<host>` は `gethostname(2)` の最初の `.` より前を ASCII 小文字にしたもの（`common::host_config_key`）。取れない・空・`/` などを含むときは 2 を飛ばし、エラーにはしない
- **黙ったフォールバックを見えるようにする。** macOS のホスト名はネットワークで変わりうる。`doctor` は `config-file` 行に選んだパスと host キーを出し、`hosts/` があるのに一致せず `config.toml` に落ちたら warn で `hosts/` の中身を列挙する。`run` は起動ログに `config: <path> (host=<key>)` を出す
- 書き込み系（`setup` / `plugin …`）は選ばれたファイルへ書く。`hosts/` のファイルは新規作成しない

採らなかった案:

- **共通部分 + 差分の include/merge**: どのキーがどこから来たかが追えなくなり、配列（`[[workflows]]` 等）のマージ規則という新しい仕様が要る。現状ほぼ全体がホスト依存なので、共通化で得るものが少ない
- **`TOTSUKA_HOST` 環境変数での上書き**: `--config` で代替できる（YAGNI）
- **macOS の `LocalHostName`**: macOS 専用で外部コマンド（`scutil`）を呼ぶ必要がある。`gethostname(2)` は libc だけで全 Unix で動く

# Consequences

- `config_dir` には config 以外の実行時ファイルが無いので、ディレクトリごと symlink してよい。`setup` の一時ファイル + rename も実体ディレクトリ内で完結する
- Windows は対象外（`gethostname` は Unix 実装のみ）
