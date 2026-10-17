---
type: Tool
title: tool-upgrade-watch スキル
description: Claude Code / Codex CLI / opencode の新しい版で入った機能を現行 docs と changelog から調べ、totsuka がツールの機能不足を肩代わりしているロジック（台帳 25 項目）を置き換えられるようになったかを判定して、候補があれば gh で issue を起票する手動実行のスキル。
resource: https://github.com/tomoya-k31/totsuka/tree/main/.claude/skills/tool-upgrade-watch
tags: [skill, tooling, claude-code, codex, opencode, hooks]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T00:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

totsuka は AI ツールの機能不足を自前のロジックで埋めている。たとえば Codex にはプロセス単位の hooks 指定が
無いので `~/.codex/hooks.json` に差し込み、opencode の共有サーバは pane の env を見られないので `--standalone`
を強制する（[ADR-0014](/decisions/adr-0014-tool-abstraction.md)）。ツール側に機能が入ればそのロジックは消せるが、
3 ツールともほぼ毎日リリースされるため、人が changelog を追い続けるのは現実的でない。このスキルは 1 回の実行で
「今の最新版で消せるようになったものはあるか」を判定する。

[herdr-schema-watch](/components/agent-ide-herdr.md)（[ADR-0055](/decisions/adr-0055-herdr-schema-typed-wire.md)）が
herdr の新しい API schema を CI で検出して起票するのと同じ発想だが、こちらは判定に docs を読む必要があるので
CI ではなくエージェントが手動で実行する。

# 構成

| パス | 内容 |
|---|---|
| `SKILL.md` | 流れ: 取得 → アンカー検査 → 項目ごとの判定（4 段階）→ 重複確認 → 起票 → レポート |
| `references/replaceable-logic.md` | 台帳。項目ごとに種別（`feature` / `policy`）・アンカー・現状・置き換え条件・検索語・置き換え後。ツールごとの**基準版**（totsuka が実機で確かめた版）もここに置く |
| `references/sources.md` | 取得元の URL とその癖（Codex docs の移転、opencode v1 / v2 の並走など） |
| `scripts/fetch-sources.sh` | 3 ツールの版・changelog・現行 docs を作業ディレクトリへ落とす。失敗した URL は `FAILED.txt` に残して続行する |
| `scripts/check-anchors.sh` | 台帳の各アンカーを `git grep -F` で引き、コードが既に変わった項目を `STALE` として出す |

# 判定と起票の規則

- **判定の根拠は現行 docs、changelog は導入版を引く索引。** changelog の箇条書きは自由文で、機能名が docs と
  一致しないうえ「fix」と新機能の区別も曖昧なため。changelog で導入版が分からなければ、ツールのソースを
  基準版と最新版で見比べてよい。
- 判定は「置き換え可能」「部分的」「未対応」「判定不能」の 4 つで、起票するのは前の 2 つだけ。
- `policy` 項目（ADR で意図して選んだ構成）は起票せず、前提が変わったかをレポートに書くだけにする。
- `marker-protocol` は 3 ツールすべてで揃ったときだけ起票する（[ADR-0020](/decisions/adr-0020-status-marker-stays.md)）。
- 重複判定は issue 本文の印 `<!-- tool-upgrade-watch: item=<id> tool=<tool> -->` で行い、`tool-upgrade` ラベルの
  issue を `gh issue list --state all` で取って突き合わせる。`--search` はタイトルの `refactor(hooks):` の括弧を
  演算子と解釈して 0 件を返すので使わない（herdr-schema-watch で重複起票した原因と同じ）。closed の issue は
  人が見送ったものとして再起票しない。
- 候補が無ければ何も書かずに終わる。スキルはコードも台帳も書き換えない。

# 検証状況

2026-10-17 に dry-run（起票せず本文をファイルに出す）で、スキル有り / 無しを 2 件ずつ比べた。有りの方は
重複判定の印・`file:line` の明示・`--search` の回避をすべて満たし、Codex の `-c hooks.<Event>` と
`--dangerously-bypass-hook-trust`、Claude Code の `type: "http"` hook などを候補に挙げた。一方で、Codex の
changelog を浅くしか取っていなかったために `request_user_input_async`（0.153.0）を見落とし、opencode v2 の
判定には古い `.mdx` を読んでいた。この 2 点と取得ページの不足は同じ PR で直した。実際の起票はまだ行っていない。

# 関連

- [ADR-0014 ツール抽象](/decisions/adr-0014-tool-abstraction.md)
- [ADR-0020 ステータスマーカーを残す](/decisions/adr-0020-status-marker-stays.md)
- [ADR-0050 質問ツール](/decisions/adr-0050-question-tool-asking.md)
- [orchestrator-core](/components/orchestrator-core.md)（台帳の項目の大半があるクレート）
