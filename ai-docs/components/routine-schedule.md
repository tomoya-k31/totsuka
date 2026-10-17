---
type: Library
title: routine-schedule
description: "routine ファイル（routines/<host>/<name>.md）の解析と cron 5 フィールドの照合を持つ leaf の共有クレート。task-source-routine（時刻が来たら submit する）と orchestrator-cli（totsuka routine list で次回を示す）の両方が使い、両者が同じファイルを違う意味に読まないようにする。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/routine-schedule
tags: [rust, crate, library, routine, schedule, cron, leaf]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

routine（[ADR-0117](/decisions/adr-0117-routines.md)）の**ファイル形式とスケジュール**だけを持つ。submit・ディレクトリの位置・workflow の解決・時差の取得は持たない（それぞれ [task-source-routine](/components/task-source-routine.md) と CLI の仕事）。

ワークスペース内のどのクレートにも依存しない leaf（`routine-leaf`、[依存境界ルール](/architecture/workspace-dependency-rules.md)）。外部依存は `time` だけ。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `cron` | `Schedule`（5 フィールドの解析と照合）。各フィールドは `*`・数値・`a-b`・`*/n`・`a-b/n`・カンマ区切り。曜日は `0`–`7`（0 と 7 が日曜）。**日と曜日の両方を制限したら OR**（Vixie cron と同じ）。名前（`MON`）と `@daily` は受け付けない。`latest_within(now, window)` が「`now` 以前で `window` より新しい最新の予定時刻」を**現在から 1 分ずつ遡って**探す（月末などの繰り上がり計算を持たない）。照合は渡された `OffsetDateTime` のオフセットで行う |
| `routine` | `.md` の解析（`parse`）。frontmatter は YAML ではなく平らな `key: value` 行（値の `"…"` / `'…'` は外す、空白の後の `#` はコメント）。**未知のキー・重複・欠落はエラー**で既定値に落とさない。本文が空でもエラー。`Routine::check(repos, workflows)` が「`repo` が `[[repositories]]` にあるか」と「どの workflow で走るか（名指したもの、無ければ唯一のもの）」を決める — プラグインの submit と CLI の一覧が同じ判定を使う。`parse_duration`（`0` / `90m` / `24h` / `7d`）と、ディレクトリ内の `*.md` をファイル名順に読む `load_dir`（ディレクトリが無いのは「routine 0 件」でエラーではない） |
