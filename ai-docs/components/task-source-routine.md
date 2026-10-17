---
type: Component
title: task-source-routine プラグイン
description: "host ごとの routine（routines/<host>/<name>.md。frontmatter の repo・cron 5 フィールドの schedule と、本文のプロンプト）を時刻どおりにタスクとして submit する公式 task_source プラグイン（stdio JSON-RPC 単体バイナリ）。逃した実行は catchup_window 内の直近 1 回にまとめ、状態は持たず、task/trigger で手動実行に応える。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/plugins/task-source-routine
tags: [rust, crate, plugin, task-source, routine, schedule, cron]
generated: { by: claude-code/opus-5.5, at: 2026-10-18T16:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# 責務

Claude のルーティンと同じ「決まった時刻に、決まったリポジトリで、決まったプロンプトを走らせる」をタスクソースとして提供する（[#918](https://github.com/tomoya-k31/totsuka/issues/918)、[ADR-0117](/decisions/adr-0117-routines.md)）。外部システムを見ない唯一のソースで、入力は設定ディレクトリのファイルだけである。

routine 1 つ = `<config dir>/routines/<host>/<name>.md` 1 ファイル。`<config dir>` と `<host>` は Orchestrator が `initialize` の `config_dir` / `host`（プロトコル 0.7.8）で渡す — `--config` があればその親、無ければ `$XDG_CONFIG_HOME/totsuka`（`hosts/<host>.toml` を選んだときも `hosts/` ではなくその親）。`<host>` は [ADR-0106](/decisions/adr-0106-per-host-config-file.md) の host key。自分の host のディレクトリが無ければ何も走らない。

# 状態を持たない設計

プラグインは「最後に実行した時刻」を保存しない。代わりに:

- **task id = `<name>@<予定時刻（UTC、分単位）>`**（例 `nightly@20261019T0000Z`）。同じ予定時刻を再送すると Orchestrator の `(source, id)` 重複検査で `duplicate` になる。daemon の再起動・スリープ復帰・プラグインのクラッシュ再起動をまたいでも二重に走らない
- **`Task.series` = routine 名**。前回の実行が終わっていなければ（入力待ちも含む）Orchestrator が `busy` を返し、その回は飛ばす
- メモリ上の `last`（routine ごとに最後に決着した id）は、毎 tick 同じ問い合わせを繰り返さないためだけのもの。消えても上の 2 つが正しさを守る

`gave up`（submit の再送が尽きた）だけは決着とみなさず、次の tick でもう一度送る。

# モジュール構成

| モジュール | 内容 |
|---|---|
| `cron` | 5 フィールド（分 時 日 月 曜日）の解析と照合。各フィールドは `*`・数値・`a-b`・`*/n`・`a-b/n`・カンマ区切り。曜日は `0`–`7`（0 と 7 が日曜）。**日と曜日の両方を制限したら OR**（Vixie cron と同じ）。名前（`MON`）と `@daily` は受け付けない。`latest_within(now, window)` が「`now` 以前で `window` より新しい最新の予定時刻」を**現在から 1 分ずつ遡って**探す（月末などの繰り上がり計算を持たない）。|
| `routine` | `.md` の解析。frontmatter は YAML ではなく平らな `key: value` 行（値の `"…"` / `'…'` は外す、空白の後の `#` はコメント）。**未知のキー・重複・欠落はエラー**で既定値に落とさない。本文が空でもエラー。`parse_duration`（`0` / `90m` / `24h` / `7d`）と、ディレクトリ内の `*.md` をファイル名順に読む `load_dir`（ディレクトリが無いのは「routine 0 件」でエラーではない） |
| `config` | `[routine]` の設定（`catchup_window` 既定 `24h`、`source_name` 既定 `routine`、`deny_unknown_fields`） |
| `server` | `TaskSourceHandler`。`initialize` で `Site`（ディレクトリ・既定の窓・このソースを名指す workflow 名・`[[repositories]]` 名・時差）を作り、30 秒ごとの tick を起動する。tick は毎回ディレクトリを読み直す（編集は再起動なしで効く）。壊れたファイル・存在しないリポジトリ・曖昧な workflow は**ファイルごとに 1 回だけ** warn し、他の routine は止めない。`task_trigger` が `totsuka routine run` に応える |

# 時刻

cron は**ローカル時刻**で照合する。時差は `main` が **tokio のランタイムを作る前に 1 回だけ**取る（`time` はスレッドが 2 本以上あると時差の取得を拒む。[ADR-0097](/decisions/adr-0097-log-timestamps-local-display.md) と同じ方式）。したがって**夏時間の切り替えは次の起動まで反映されない**。tick は 30 秒ごとなので、起きている間はどの分も取りこぼさない。

# 手動実行（`task/trigger`）

`task/trigger {key}` に、その routine のタスクを**応答として**返す（submit しない — Orchestrator が受け取って `task/submit` と同じ経路で取り込む）。id は `<name>@manual-<UTC 秒単位>` で、予定時刻の id とは重ならないので**定期の回を消費しない**。`enabled: false` の routine も手動では走る（`enabled` が止めるのはスケジュールだけ）。series は定期と共通なので、実行中なら `busy` が CLI まで返る。

# workflow と出力

routine は workflow を名前で参照する（`workflow:`。このソースを名指す workflow が 1 つなら省略可）。workflow の `projects` は `source = "routine"` の `[[projects]]` を指す。**このソースは何も publish しない**（`outputs` を宣言しない）ので、workflow は `output = "none"`。成果物はエージェントがプロンプトに従って PR やスキルで外へ出す。

無人で走るので、**`design` / `implement` profile は向かない**（完了時に pane 上で人間の承認を待つ）。`answer` / `triage` は既定の出力が `source` なので弾かれる。profile を書かず `mode` と `output = "none"` を明示するのが基本形（[設定リファレンス](/development/config-reference.md)）。

trigger のキーは 1 つも読まないので、workflow の `trigger` に書いたキーは起動時エラーになる（`trigger = {}`）。`on_*` の status 書き戻しは受け付けて何もしない。

# 依存

plugin-protocol（0.7.8 以上）/ plugin-sdk / serde / schemars / time（`local-offset`）/ tokio / tracing。ネットワーク依存は無い。
