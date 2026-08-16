---
type: Decision
title: ADR-0047 AI 用 OKF バンドルを ai-docs/ へ分離し、人間用 docs/ を生成物にする
description: "OKF バンドルはエージェント向けの構造（frontmatter・元帳・内部 issue 参照）を持ち人間の読者には冗長なため、バンドルを ai-docs/ へ移し、README が指す人間向けページは ai-docs から生成する docs/ に置く決定。隠しディレクトリ .ai-docs/ は ripgrep と okf-search.sh の find から不可視になることを実測して不採用、複製 + 手動同期も検査が無いため不採用とする。"
resource: https://github.com/tomoya-k31/totsuka/issues/458
tags: [decision, docs, okf, tooling, adr]
generated: { by: claude-code/fable-5, at: 2026-08-16T16:50:47+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-458
    resource: https://github.com/tomoya-k31/totsuka/issues/458
    title: "docs 分離: AI 用 OKF バンドルを ai-docs/ へ移設し、人間用 /docs を AI 生成 + 鮮度検査で新設する"
  - id: adr-0031
    resource: https://github.com/tomoya-k31/totsuka/blob/main/ai-docs/decisions/adr-0031-docs-ledger-conflicts.md
    title: "ADR-0031 元帳ファイルは生成物にする"
---

# Status

stable（[#458](https://github.com/tomoya-k31/totsuka/issues/458)）。本 ADR は分離の決定そのものを記録する。移設（本 PR）と人間向けページの生成機構は 2 段階で入る。

# Context

`docs/` は [ADR-0001](/decisions/adr-0001-adopt-okf.md) 以来 OKF 準拠の Knowledge Bundle として運用してきた。読み手はエージェントである前提で、frontmatter・`index.md` / `log.md` の元帳・内部 issue 番号への参照・判断過程の記録を持つ。これはエージェントには有効に働いている。

一方 README は同じツリーの 5 本を**実利用ユーザ向けドキュメント**としてリンクしている（プラグイン開発ガイド、セットアップ Playbook、仕様書、設定リファレンス、運用ガイド）。ここに OKF の構造がそのまま出ることには具体的な不都合がある:

- frontmatter が本文の前に出る
- 「なぜそうしたか」の記録が「どう使うか」に混ざる
- issue 番号・ADR 番号・PR 番号が本文に埋まっており、リポジトリの事情を知らない読者には意味がない
- 1 ページが長い

つまり `docs/` は**二つの読者に同時に奉仕できていない**。

# Decision

読者ごとにツリーを分ける。

- **`ai-docs/`** — OKF v0.2 バンドル。**単一の正本**。これまでの `docs/` をそのまま移設する。
- **`docs/`** — 人間向け。README が指す 5 本 + 手書きの目次。**`ai-docs/` からの生成物**とする。

## `ai-docs/` は隠しディレクトリにしない

当初案は `.ai-docs/` だった（GitHub UI で先頭に沈み「人間は見なくてよい」意図が伝わる）。**実測で不採用**とする。ドット始まりディレクトリは、この決定が奉仕しようとしている当のエージェントのツールから消える:

| 検査 | `.ai-docs/` | `ai-docs/` |
|---|---|---|
| `rg -l <pattern>`（既定。Claude Code の Grep の実体） | ヒットしない | ヒットする |
| `okf-search.sh` の走査（`find … -name '.*' -prune …`） | 0 件 | 1 件 |

後者は偶発的ではない。`scripts/okf-search.sh` の走査は隠しファイルを除外するために `-name '.*' -prune` を持っており、バンドル root 自身の basename が `.ai-docs` だと**root ごと prune される**。`okf-search` スキルは無言で「該当なし」を返すようになる。

ドットを外しても「人間用 `docs/` と分離する」という目的は達成できるので、この 1 文字に払う代償が見合わない。

## 人間向けページは生成物にし、鮮度だけを機械検査する

`docs/` の 5 本は `ai-docs/` の対応ソースから**スキルが生成**する。変換は機械的ではなく編集的である — frontmatter を剥がすだけでは足りず、内部 issue 番号や判断過程の記録を落とし、簡潔に書き直す必要がある。

同期は次のように分担する:

- **内容の正しさ** — 生成スキル + PR レビュー
- **古さの検出** — CI。生成ページにソースの content hash を埋め、`ai-docs/` 側の現在値と照合する

このリポジトリでは**検査の無い手動同期が既に 2 度壊れている**（[ADR-0031](/decisions/adr-0031-docs-ledger-conflicts.md) の元帳、[ADR-0045](/decisions/adr-0045-read-only-is-not-guaranteed.md) の撤回漏れ）。元帳が機能しているのは `--check` を CI に置けたからで、同じ形を踏襲する。

ただし**この検査が保証するのは「古くない」ことだけ**で、生成物の内容が正しいことは保証しない。そこは人間のレビューが引き受ける。

# Alternatives considered

- **`.ai-docs/`（隠しディレクトリ）** — 上表のとおり `rg` と `okf-search.sh` の双方から不可視になる。不採用。
- **`docs/` を薄い入口ページに限定する**（詳細は ai-docs へリンク） — 重複は最小化できるが、読者は結局 OKF 文書を読むことになり、冗長さの問題が解けない。不採用。
- **人間向け 5 本を `docs/` へ移籍して単一正本にする** — 同期問題は消えるが、その 5 本は同時にエージェントの参照先でもあり、バンドルから外すと `index.md` とリンク整合が崩れる。不採用。
- **複製 + スキルに同期義務を書く** — `.ja.md` 規約と合わせて同一内容が 3 箇所になる。検査が無い義務は守られないことがこのリポジトリで実証済み。不採用。
- **決定的スクリプトで生成する**（セクションマーカー + frontmatter 除去） — CI で完全に検査できる利点があるが、「内部情報の除去と簡潔な書き直し」は編集作業であり、機械変換では目的の品質に届かない。不採用。
- **鮮度検査を置かず生成スキルだけに任せる** — スキルの実行を忘れた PR から黙ってズレる。却下した「手動同期」と強度が変わらない。不採用。

# Consequences

- エージェント向けの参照は全て `ai-docs/` を指すようになる。CI・PostToolUse フック・`.gitattributes`・`.rumdl.toml`・OKF スクリプト 5 本の既定 `bundleDir`・`CLAUDE.md`・rules・skills が対象。
- Rust ソース中の `docs/…` 参照も追随する。**GitHub blob URL 形式**（`github.com/…/blob/main/docs/…`）で書かれた rustdoc リンクが 11 箇所あり、これらは移設で 404 になるため同時に更新した。
- 人間向けの導線は、移設した段階では**まだ `ai-docs/` を指したままになる**。README / README.ja のドキュメント一覧、ユーザ向けの実行時メッセージ 2 箇所（`config/validate.rs` のスキーマ版数エラー、`task-source-slack` のプレースホルダ警告）、issue テンプレートの spec 参照がこれにあたる。つまり移設だけを入れた時点では「人間が OKF 構造の文書を読む」という本 ADR が解こうとしている状態が残っており、生成物が入って初めて解消する。これらは人間向けページが入った段階で `docs/` に切り替える。
- 過去の issue / PR 本文にある `docs/…` の深いリンクは main 上で 404 になる。リダイレクト用のスタブは置かない（当時のコミットから辿れる）。
- `docs/` は OKF バンドルではないので、`okf-lint` を掛けてはならない。PostToolUse フックのパス判定も `ai-docs/` のみに絞ってある。
