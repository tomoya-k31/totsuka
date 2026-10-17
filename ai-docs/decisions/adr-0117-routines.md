---
type: Decision
title: ADR-0117 routines — host ごとの定期実行を task-source-routine プラグインで入れる
description: リポジトリ + cron スケジュール + プロンプトを routines/<host>/<name>.md に置き、task-source-routine プラグインが時刻ごとにタスクを submit する決定。逃した実行は catchup_window 内の直近 1 回にまとめ、冪等性は既存の (source, id) 重複検査に任せ、実行中の重複は Task.series と submit の busy で本体が止め、手動実行は task/trigger でプラグインが応答として返したタスクを本体が ingest する。
resource: https://github.com/tomoya-k31/totsuka/issues/918
tags: [decision, protocol, routine, schedule, task-source, adr]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T12:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-918
    resource: https://github.com/tomoya-k31/totsuka/issues/918
    title: "Issue #918 — routines の設計（不採用案・落とし穴を含む）"
---

# Status

stable（[#918](https://github.com/tomoya-k31/totsuka/issues/918)）。stacked PR の 4 層（protocol 0.7.8 → core → プラグイン → CLI）で入れる。本 ADR はプロトコル層と同時に入り、残りの 3 層が同じ決定を実装する。

# Context

Claude のルーティンのように「決まった時刻に、決まったリポジトリで、決まったプロンプトを走らせる」仕組みが totsuka に無かった。タスクの入口はすべてソースプラグインの `task/submit`（[ADR-0008](/decisions/adr-0008-task-submit-push-ingestion.md)）で、どのソースも外部システムの変化を拾うものだけだった。

要求は次のとおり。

- host ごとに定義できる（`hosts/<host>.toml` と同じ考え方、[ADR-0106](/decisions/adr-0106-per-host-config-file.md)）
- 実行は workflow と同じく worktree を作ってエージェントに渡す
- PC や daemon が止まっている間は動かなくてよいが、逃した分は設定した範囲で取り戻す

# Decision

1. **定義は `routines/<host>/<name>.md`**（設定ディレクトリの `hosts/` と同じ階層）。frontmatter に `repo`（必須）・`schedule`（必須、cron 5 フィールド、ローカルタイムゾーン、分単位）・`workflow`・`catchup_window`・`enabled` を書き、本文がプロンプトになる。ファイル名が routine 名。自分の host のディレクトリが無ければ何も実行しない。
2. **実装は `task-source-routine` プラグイン**。本体に予約ソースを作らない —— 「ソースはすべてプラグイン」を守れば、publish・ステータス書き戻し・`wf.source` 検査に例外の分岐が要らない。routine は workflow を名前で参照し、profile・agent・出力先は workflow が決める（該当する workflow が 1 つなら省略可）。
3. **逃した実行は直近 1 回にまとめる**。`catchup_window`（プラグイン設定に既定値、routine ごとに上書き、`0` で catch-up しない）以内の予定時刻のうち最新の 1 つだけを submit する。定期実行のプロンプトは「今の状態を見て何かする」ものが多く、同じ処理を N 回続けて流しても意味がないから。
4. **冪等性は既存の重複検査に任せ、プラグインは状態を持たない**。task id を `routine:<name>@<予定時刻>` にすると、同じ予定時刻の再送は `(source, id)` の UNIQUE で `duplicate` になる。daemon の再起動やスリープからの復帰をまたいでも二重に走らない。
5. **実行中の重複は本体が止める**: プロトコル 0.7.8 で `Task.series` と `TaskSubmitStatus::Busy` を足す。同じ source・series のタスクが終わっていなければ（入力待ちも含む）submit は `busy` になり、その回は飛ばす（キューに積まない）。プラグインからはタスクの状態が見えないので、判定は状態を持つ本体に置く。どのソースでも使える汎用の仕組みにする。
6. **手動実行 `totsuka routine run <name>` は `task/trigger`**: control UDS（[ADR-0094](/decisions/adr-0094-task-control-endpoints.md)）に `POST /routine/run` を足し、本体がプラグインに `task/trigger {key}` を送る。プラグインは **submit せず、応答として `TaskSubmitParams` を返し**、本体がそれを `task/submit` と同じ経路で ingest して、結果（accepted / busy＋実行中のタスク / 知らない key）を CLI に返す。プラグインが応答の途中で `task/submit` を呼び返す形にしなかったのは、本体が trigger の応答を待つ間にそのプラグインの submit を待たせる再入を作らないため。
7. **プラグインへの受け渡し**: `InitializeParams` に `config_dir`（`--config` があればその親、無ければ `$XDG_CONFIG_HOME/totsuka`）と `host`（ADR-0106 の host key）を足す。プラグインは protocol / sdk 以外に依存できない（arch-lint）ので、host key の求め方を複製させない。

# Consequences

- routine 定義は `config.toml` の外に置かれる。設定を 1 本に集める [ADR-0058](/decisions/adr-0058-config-ownership-boundary.md) からは外れるが、routine の本体は長いプロンプトという資産であり、TOML の文字列に入れると書けたものではない。設定値（`catchup_window` の既定）は `config.toml` のプラグイン設定に残る。
- 結果は投稿しない（routine 用 workflow は `output = "none"`）。成果物はエージェントがプロンプトに従って PR やスキルで外に出す。経過は `totsuka task show` とメニューバーで見る。
- routine は `branch_hint` を使わない。`branch_hint` は origin に既にあるブランチを指すもので、見つからないとタスクが失敗する。新しいブランチの名前はエージェントが決める。
- `duplicate` による冪等性は tasks の行が残っていることが前提になる。`catchup_window` より新しい行を消す掃除は入れないこと。
- cron は依存を足さずに自前で解析する（照合は分単位で、範囲を 1 分ずつ遡って走査する）。
- cron はローカル時刻で照合し、時差はプラグインの起動時に tokio のランタイムを作る前に 1 回だけ取る（`time` はマルチスレッドでの取得を拒む。[ADR-0097](/decisions/adr-0097-log-timestamps-local-display.md) と同じ方式）。**夏時間の切り替えは次の起動まで反映されない**。
- routine は無人で走るので、`design` / `implement` profile（完了時に pane 上で人間の承認を待つ）は向かず、`answer` / `triage` は既定の出力が `source` で使えない。workflow は profile を書かず `mode` と `output = "none"` を明示する。
