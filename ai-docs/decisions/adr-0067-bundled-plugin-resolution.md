---
type: Decision
title: ADR-0067 同梱プラグインはコピーせず、実行中のバイナリのツリーへ毎回解決する
description: "brew upgrade がインストール済みプラグインを更新せず、更新が要ることも教えてくれない問題への決定。--bundled を「コピーする」から「同梱由来と記録するだけ」に変え、起動のたびに current_exe から同梱ツリーを計算して解決する。パスを保存しないので Cellar が入れ替わっても腐らない。--bundled-dir とパス/--from-source は従来どおりコピー。post_install での入れ直し・受領書＋plugin upgrade・symlink・版の無条件拒否は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/611
tags: [decision, plugins, cli, install, homebrew, upgrade, adr]
generated: { by: claude-code/opus-5, at: 2026-09-02T14:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。#611 で実装した。

# Context

**`brew upgrade totsuka` はインストール済みプラグインを更新しない。そして更新が要ることも教えてくれない。**

`plugin install` はバイナリと manifest を `$XDG_DATA_HOME/totsuka/plugins/<name>/` へ**コピー**する。CLI とその同梱ツリー（`<exe>/../libexec/totsuka/plugins`）は一緒に上がるが、コピーは残る:

| | upgrade 後 |
|---|---|
| CLI | 新しい |
| 同梱ツリー（Cellar） | 新しい |
| **インストール済みプラグイン** | **古いまま** |

古いまま動き続けることに気づけないのは、次の 3 つが揃っているため:

1. **由来が記録されていない。** インストール済みの `plugin.toml` は元と完全一致で、`--bundled` / `--from-source` / パス指定のどれで入れたか残らない
2. **陳腐化を検出しない。** `doctor` の `bundled-plugins` チェックは同梱**数**を数えるだけで、インストール済みの版と比較しない
3. **`plugin upgrade` が無い**

唯一の安全網はプロトコル互換検査（F-54）で、**`protocol_version` の範囲が動いたときだけ** launch を拒否する。範囲内のバグ修正・機能追加は黙って取り残される。

# Decision

**`--bundled` を「コピーする」から「同梱由来であると記録するだけ」に変える。** 起動のたびに、実行中のバイナリから同梱ツリーを**計算して**解決する。

```text
$XDG_DATA_HOME/totsuka/plugins/notion/
  bundled            ← マーカー 1 個だけ。バイナリも manifest も無い
```

## 中心にある不変条件: パスを保存しない

**同梱ツリーのパスは記録しない。** `current_exe` から毎回計算する。これが upgrade に耐える理由である:

- Homebrew は upgrade で**旧 Cellar ディレクトリを削除する**ので、インストール時に記録したパス（`/opt/homebrew/Cellar/totsuka/0.6.1/libexec/...`）は必ず腐る
- 一方「走っているバイナリの隣」は、走っているバイナリが常に現行版であるがゆえに常に正しい

**陳腐化しうる複製が存在しなくなる**ので、入れ直しは構造的に不要になる。検査で鮮度を担保するのではなく、食い違える状態を作らない。

## コピーを残す 3 つの経路

| 経路 | 扱い | 理由 |
|---|---|---|
| `--bundled`（既定のツリー） | **記録のみ** | CLI の一部であって、運用者が選んだものではない |
| `--bundled --bundled-dir <path>` | **コピー** | 実行時解決は `current_exe` から導くので、運用者が指定した別ツリーを指さない。黙って違うものに差し替わるほうが、スナップショットを取るより悪い |
| パス指定 / `--from-source` | **コピー** | 運用者が選んだスナップショットである。upgrade が開発ビルドを黙って置き換えてはならない |

## 「未インストール」と「ツリーが無い」を分ける

同梱由来の記録があるのに同梱ツリーが無い（`cargo install` ビルド等）場合は、**`is_installed` は true のまま**にし、解決時に専用のエラー（`NoBundledTree`）を返す。

「未インストール」と報告すると運用者を `plugin install` へ送るが、それは誤った修理である —— **宣言は健全で、無いのはツリーのほう**。エラー文は「ディレクトリから入れ直せ、または同梱付きのビルドを使え」と言う。

`plugin list` は診断コマンドなので、解決できない 1 件で一覧全体を失敗させず、その行を飛ばす。

## 却下した案

| 案 | 破れ方 |
|---|---|
| **Homebrew formula の `post_install` で入れ直す** | formula から `$HOME` を書くのは Homebrew の作法に反する。かつ **brew 以外の導入経路（リリース tarball / `cargo install`）を救わない** |
| **受領書に由来を記録し、`doctor` で陳腐化を検出、`plugin upgrade` で直す** | 変更は小さいが「気づける」までしか到達しない。`run` 起動時に自動更新まで踏み込めば自動になるが、**起動時にバイナリを差し替える副作用**を持つ。複製を持たないほうが不変条件として強い |
| **同梱ツリーへの symlink を張る** | **成立しない。** symlink はパスを保存するのと同じで、Homebrew が旧 Cellar を消した瞬間に dangling になる。`/opt/homebrew/opt/<formula>` は版に安定だが Homebrew 固有で、tarball 配置には無い |
| 「upgrade 後は入れ直せ」と運用ドキュメントに書く | 上記 3 の「覚えている以外の担保が無い」がそのまま残る。忘れたときに何も言わないのが問題の本体である |
| 版が違えば無条件に拒否する | `--from-source` の開発ループが動かなくなる |
| 同梱ツリーの探索を core に移す | 実行ファイル自身のレイアウトを推論する話なので CLI の領分。core はパスを**受け取る**だけにした（`PluginStore::with_bundled_root`） |

# Consequences

- **`--bundled` で入れたプラグインは、CLI の upgrade に自動で追従する。** 入れ直しのコマンドは無い（要らない）。
- **`plugin install --bundled` の出力が変わる。** 「Installed ... to <path>」から「Linked ... to the bundled tree」になる。
- **`plugin list` に `ORIGIN` 列が増えた**（`bundled` / `copied`）。`--json` にも `origin` フィールドが入る。
- **同梱由来の記録は、そのプラグインを同梱しているビルドからしか解決できない。** Homebrew 版で入れた記録を `cargo install` 版から読むと `NoBundledTree` になる。1 台に複数のビルドを置く構成では意識が要る。
- **既存のコピーはそのまま動く。** マーカーが無いものは `Copied` と読まれるので、この変更は後方互換である。移行手順は無い。
- **`--bundled` はもうディスクを消費しない**（マーカー 1 ファイルのみ）。
- 陳腐化の検出（`doctor`）と `plugin upgrade` は**作らなかった**。複製が無いので検出すべき差が無い。コピー由来のものが古いかどうかは、依然として運用者の判断である。

# 関連

- [設定リファレンス](/development/config-reference.md)
- [orchestrator-cli](/components/orchestrator-cli.md)
- [ADR-0027 プラグイン成果物の命名](/decisions/adr-0027-plugin-artifact-naming.md) —— バイナリ名 = manifest の `name` という不変条件（解決先の組み立てが依存している）
- [リリース運用](/operations/release-runbook.md) —— 同梱プラグインをリリースに載せる仕組み
