---
type: Decision
title: ADR-0108 on_start / on_success / on_failure でラベルを付け外しする
description: on_* テーブルに `labels = ["+a", "-b"]` を足し、GitHub Project のカードに紐づく Issue / PR のラベルを書き戻せるようにした決定。新メソッド task/update_labels と capability label_writeback で送り、接頭辞なしは起動時エラー、存在しないラベルは validate / doctor で検出、失敗は status と同じく warn のみ、閉路検査は拡張しない。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/methods.rs
tags: [decision, protocol, config, github, workflow, adr]
generated: { by: claude-code/opus-5.5, at: 2026-09-30T18:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。プロトコル（0.7.6）→ core → github プラグインの 3 層で入れる。本 ADR はプロトコル層と同時に入り、残りの 2 層が同じ決定を実装する。

# Context

`on_start` / `on_success` / `on_failure` に書けるのは `status` だけだった。trigger 側では GitHub の `label` / `exclude.label` で絞り込めるのに、書き戻し側でラベルを動かせないため、「AI が触ったタスクに `ai:needs-human` を付けて次から拾わない」「実行中は `ai:running` を付けておく」といった運用はエージェントに `gh issue edit` をさせるしかなかった。これは検証の結果ではなくエージェント自身の判断で走るので、成否に紐づけられない。

`on_*` をスカラーでなくテーブルにしたのは、こういう拡張の余地を残すためだった（[ADR-0062](/decisions/adr-0062-status-vocabulary.md)）。

# Decision

1. **書式は `labels = ["+a", "-b"]`**。`+` が追加、`-` が削除。接頭辞の無い要素は起動時エラー（追加のつもりか削除のつもりかを必ず書かせる）。先頭が `+` / `-` のラベル名は `++x` / `-+x` と書けば曖昧さは残らない。1 つのテーブルで同じ名前を `+x` と `-x` の両方に書くのも起動時エラー —— どちらが勝つかを決めずに済むよう、`task/update_labels` の `add` と `remove` は常に交わらない。`status` と併記しても、`labels` 単独でもよい。
2. **新メソッド `task/update_labels { task_id, add, remove, projects }`** で送る。`task/update_status` を拡張しなかったのは、ラベルだけの書き戻しに送る status が無く、`TaskUpdateStatusParams::status` を Option にするとワイヤ互換が壊れるから。送る順は status → labels で、互いに独立。
3. **capability `label_writeback` で申告させる**（`task_claim` と同じ形）。宣言の無いソース（Notion / Slack / Discord）の workflow に `labels` があれば起動時エラー —— 黙って送らないより、書いた設定が効かないことを最初に知らせる。
4. **存在しないラベルは `config validate` のオンライン部と `doctor` がエラーにする**。status の列名検査（[ADR-0062](/decisions/adr-0062-status-vocabulary.md)）と同じ扱いで、`WorkflowInfo.label_writebacks` が名前を運ぶ。追加だけでなく削除側も検査する —— 綴り違いの `-label` は何にも一致しないまま黙って成功するので、`+label` と同じだけ危ない。`[[repositories]]` は owner を持たないので、github はリポジトリを**ボードの owner の下**で探し、見つからなければ検査できなかったとして**警告**にする（ボードと owner が違うリポジトリでは設定が正しくてもそうなるので、エラーにはしない）。
   **実行時に付けるラベルが無ければ作る**（github は `createLabel`、色は既定の `ededed`）。検査を通った後に消されたラベルのためで、ここで失敗させると検査と実行の間の削除ひとつで書き戻しが止まる。作成は冪等でないので再送しない。
5. **失敗は warn ログのみ**。status の書き戻しと同じで、タスクの成否を巻き込まない。削除対象が付いていないのは成功（書き戻しは「こうなっていてほしい」終状態を言っているので、既にそうなら何もしない）。
6. **対象は Issue / PR**。GitHub の `task_id` はコンテンツの node id なので、そのままラベル操作の対象にできる。Draft issue のカードはリポジトリを持たず、そもそも取り込まれないので対象外。

## 閉路検査は拡張しない

列グラフの閉路検査（[ADR-0059](/decisions/adr-0059-task-claim-exclusion.md)）は status しか見ていないが、ラベルを載せる必要は無い。GitHub の配送キー（lane identity）は **status セルの `updatedAt`** から作られ（`plugins/task-source-github/src/client.rs` の `message_key`）、ラベルは一切入らない。`status` を持つ trigger ではラベルを付け外ししても同じキーのまま重複として捨てられ、`label` だけの trigger はそもそもタスクごとに高々 1 回しか走らない。ラベルの書き戻しが再配送を生む経路が無いので、閉路になりようがない。

グラフに載せると、実際にはループしない「自分の trigger 条件を満たすラベルを足す」設定を誤検知する。配送キーにラベルが入る変更が将来あれば、この前提は崩れるので見直す。

# Consequences

- GitHub のトークンにラベルを書く権限（と、無いラベルを作る権限）が要る。**`repo` + `project` の OAuth トークンで足りることは実測した**（#840、`github-label-probe.sh` で Issue・PR とも 8 項目 PASS）。fine-grained PAT なら Issues: write と導出したが、**最小値と fine-grained PAT は未実測**で、PR のラベルに Pull requests: write も要るかは未確認。他の操作と同じく「十分条件は実測・最小値は未実測」で止めている。
- core が `on_*` から読むキーは `status` と `labels` の 2 つになる。どちらも core のキーで、プラグインには意味の確定した形（`task/update_labels` の `add` / `remove`）で渡るので、`on_*` テーブルそのものは引き続きプラグインに見えない。
