---
type: Decision
title: ADR-0092 git の呼び出しに上限時間を設け、超えたらプロセスグループごと止める
description: Engine のループ上で同期に走る git にタイムアウトが無く、スリープ復帰後の git fetch（ssh が死んだ接続を待ち続けた）で Engine が 6 時間止まった。SystemGitRunner の全呼び出しに上限（既定 300 秒、[worktree].git_timeout_secs で上書き、0 で無効）を設け、超えたら git をプロセスグループごと SIGKILL して TimedOut で失敗させ、既存の dispatch 失敗 → 自動再キューに乗せる決定。spawn_blocking による非同期化は、dispatch 自体がループ上で逐次 await されているため効果が無く採らない。
tags: [decision, git, worktree, timeout, engine, adr]
generated: { by: claude-code/opus-5, at: 2026-09-22T12:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#764）。

# Context

`GitRunner::run` は同期の trait で、本番実装の `SystemGitRunner` は `Command::output()` を呼ぶ。`Engine` のループは 1 つの async タスクで、worktree の作成（`git fetch origin` を含む）もそのループ上で直接走る。git にタイムアウトは無かった。

2026-09-21T20:18Z、ホストのスリープ復帰の直後に dispatch の `git fetch origin` が ssh ごと返らなくなった。ssh は死んだ TCP 接続を待ち続け、Engine は約 6 時間、dispatch・完了シグナルの適用・タイムアウト監視・health の公開をすべて止めていた。`task/submit` に ack が返らなくなり、ソース側のプラグインが ERROR を出し続けたことで初めて気付かれた（#764 のコメント）。

# Decision

1. **`SystemGitRunner` のすべての git 呼び出しに上限時間を設ける。** 既定は `DEFAULT_GIT_TIMEOUT` = 300 秒。ネットワークに出るコマンドだけを選り分けることはしない。ローカルの git は数ミリ秒で終わるので、一律の上限でも誤って打ち切ることはない
2. **上限を超えたら git のプロセスグループ全体に SIGKILL を送る。** git は `process_group(0)` で自分のグループの先頭として起動する。git が起動した `ssh` は git の stderr を握っているので、git だけを殺すと、stderr の読み取りが ssh が生きている間ずっと終わらない（テストで実測。git だけを殺した場合は 30 秒待った）
3. **打ち切った呼び出しは `io::ErrorKind::TimedOut` の `Err` を返す。** 呼び出し元は `?` でそのまま伝播させるので、dispatch では `fail_dispatch` → 既存の自動再キュー（上限 `DISPATCH_RETRY_LIMIT`）に乗る。メッセージには `~/.ssh/config` の `ServerAliveInterval` を案内する
4. **`GIT_TERMINAL_PROMPT=0` と stdin の `/dev/null` を付ける。** 自分のグループに入った git は端末から読めない（読もうとすると SIGTTIN で停止し、上限まで居座る）。認証情報のプロンプトは待たずに即座に失敗させる
5. **上限は `[worktree].git_timeout_secs` で上書きできる。** 省略時は 300 秒、`0` で上限なし（`timeout_secs` と同じ「`0` で無効」）。値は `totsuka run` が `SystemGitRunner::with_timeout` で渡し、`SystemGitRunner::default()` は既定値を使う（`doctor` などローカルの git しか実行しない呼び出し元とテスト）。これは「固まった git を止める」ための上限であって、遅いが生きている fetch を取り締まるものではないので、巨大なリポジトリで正当に 300 秒を超える場合にだけ上げる
6. **死んだ ssh 接続は、ssh 自身の `ServerAliveInterval` / `ServerAliveCountMax` で先に切るのを推奨する**（[運用ガイド](/operations/operations-guide.md)）。そちらは応答の有無で判定するので、遅いだけの転送は切らず、より早く、分かりやすいエラーで落ちる。`git_timeout_secs` はその積（推奨値で 120 秒）より長く保つ

# 代替案と不採用理由

- **`spawn_blocking` で git をランタイムの外へ出す（#764 の原案）** — dispatch は `Engine` のループ上で逐次 `await` されている（プラグイン RPC も同じ）ので、git を別スレッドへ出してもループはその完了を待つ。ランタイムのワーカースレッドは空くが、hook の受信やプラグインの読み取りはもともと別のワーカーで動いている。ループを止めないには dispatch を「worktree 準備中」の保留状態に切り出す設計変更が要り、この障害を塞ぐのには過大
- **`fetch` と `ls-remote` だけに上限を付ける** — 呼び出し側で種類を選り分けるコードが要り、ローカルの git が固まる場合（壊れた fsmonitor デーモン、ロック待ちのフック等）を取りこぼす
- **上限を定数にして設定キーを設けない（最初の実装）** — 巨大なリポジトリの初回 fetch や遅い回線では 300 秒を正当に超えうる。そのとき利用者に逃げ道が無い
- **ssh 側の設定（`ServerAliveInterval`）だけに頼る** — 利用者の `~/.ssh/config` 次第で、https の remote や認証情報のヘルパーが固まる場合には効かない
- **`GIT_SSH_COMMAND` で `-o ServerAliveInterval=…` を強制する** — 利用者の `core.sshCommand` や ssh の設定を上書きしてしまう
- **`try_wait` のポーリングで待つ** — すべての git 呼び出しにポーリング間隔ぶんの遅延が乗る。パイプの EOF を待つ今の実装は、`output()` と同じ時点で戻る

# Consequences

- git が固まっても、Engine の停止は上限（既定 300 秒）で終わる。その間はループが止まる点は変わらない（プラグイン RPC の `request_timeout_secs` と同じ性質の上限になる）
- dispatch の fetch が 3 回続けて打ち切られると、タスクは failed になる
- パスフレーズ付きの鍵を ssh-agent なしで使っていた場合、これまで端末に出ていたプロンプトは出なくなり、ssh は SIGTTIN で止まって上限で打ち切られる。無人で動かす run ではもともと答えられないプロンプトだった
- `totsuka run` を Ctrl-C で止めると、実行中の git は自分のグループにいるので SIGINT を受け取らず、終わるまで（または上限まで）残る
- 検証は `adapters::git` の単体テストで行う。`core.sshCommand=sleep 30 #` で git の子として固まる ssh を再現し、上限 1 秒で 10 秒以内に `TimedOut` が返ることを確かめる。グループではなく git だけを殺すように変えると、このテストは 30 秒かかって落ちる
