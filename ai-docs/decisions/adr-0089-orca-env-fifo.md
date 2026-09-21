---
type: Decision
title: ADR-0089 orca の起動時の環境変数は端末に打ち込まず、FIFO で渡す
description: orca プラグインが tool_launch.env を exec env K=V … として端末に打ち込んでいたため、フックのトークンと指示文の全文が画面とスクロールバックに平文で残っていた。env は 0700 のディレクトリに作った 0600 の FIFO で渡し、端末には FIFO を読んで消してからエージェントに置き換わるコマンドだけを打ち込む決定。読み手が現れなければディスパッチを失敗させ、FIFO を消す。
tags: [decision, orca, plugin, secrets, launch, adr]
generated: { by: claude-code/opus-5, at: 2026-09-21T18:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable（#744 の前半）。[ADR-0082](/decisions/adr-0082-orca-herdr-parity.md) の「env は `exec env K=V …` に載せる」を置き換える。起動のほかの部分（全語の単一引用符、`exec` で端末の寿命をエージェントに合わせる）は ADR-0082 のまま。

# Context

orca の `terminal create --command` は、渡した文字列をログインシェルに**打ち込む**（orca 1.4.205 で実測）。`terminal create` は env を受け取らないので、ADR-0082 は env を `exec env 'K=V' … 'program' …` の形で先頭に付けた。その結果、`tool_launch.env` の値がすべて端末の画面とスクロールバックに平文で残っていた:

- `TOTSUKA_HOOK_TOKEN` — フック受信口の Bearer トークン
- `TOTSUKA_PROMPT_CONTEXT` — 指示文の全文
- この後の #744 で `[tools].env_file` が足す値 — 1Password などから取った API キー

herdr は `workspace.create` の API パラメータで env を渡しているので、画面には出ない。

# Decision

1. **`tool_launch.env` はまるごと FIFO で渡す。** どれが秘密かをプラグインに見分けさせない（プラグインから見て env は opaque）。env が空なら FIFO は作らず、今までどおり `exec 'program' …` を打ち込む
2. FIFO は `${XDG_RUNTIME_DIR:-<state_dir>}/totsuka/orca-env/<pid>/` に置く。Orchestrator の `runtime_dir` と同じ規則で、hook ソケットの隣になる。ディレクトリは親もプロセスごとのものも `0700`、FIFO は `mkfifo -m 600` で作る。中身は 1 変数 1 行の `export K='v'` で、値は起動の各語と同じ単一引用符でクォートする。**キーがシェルの識別子でなければディスパッチを拒否する**（`export` 行にそのまま載るので、識別子でなければコードになる）
3. 端末には次の形だけを打ち込む:

   ```text
   exec sh -c 'e=$(cat "$1") || exit 1; rm -f "$1"; eval "$e" || exit 1; shift; exec "$@"' sh '<fifo>' '<program>' '<args>'…
   ```

   - **`. "$1"` ではなく `cat` + `eval` にする。** macOS の `/bin/sh` は bash 3.2 で、その `.` は `stat` で得たサイズだけ読むため、FIFO からは**何も読まない**。実測では変数が空のままエージェントが起動した。issue の原案はこの形だった
   - `|| exit 1` は、書き手が諦めて FIFO を消した後にシェルが動いた場合に、env の無いままエージェントを起動させないため
4. **書き手は `terminal create` の前に起動する。** シェル側の読み込みは、書き手が開くまで待つため
5. **書き手はブロッキングの `open` で待たない。** `O_NONBLOCK` の open を ENXIO の間ポーリングし、パイプが詰まったら（指示文はパイプのバッファを超えうる）書き込みもポーリングする。どちらも実時間の締め切りとキャンセルのフラグで打ち切る。書き込み用のブロッキング `open` は読み手が現れるまで戻らず、待っている間に FIFO が unlink されると永遠に戻れない。実際に、テストでスレッドが残ってランタイムの終了が止まった
6. **締め切りは起動の待ち時間（`STARTUP_WAIT` = 60 秒）を流用する。** 過ぎたら FIFO を消し、端末を閉じてディスパッチを失敗させる。後は既存の再キューに任せる。新しい設定キーは足さない
7. **FIFO のディレクトリはプラグインのプロセスごとに分ける**（`orca-env/<pid>/`）。開くときに同じ pid の残骸を消し、終了時にディレクトリごと消す。他のプロセスのディレクトリには触らない。`run.lock` があっても orca プラグインは 1 つとは限らない — `totsuka doctor` と `config validate` も自分の orca プラグインを起動・初期化するので、共有ディレクトリを掃除すると、実行中の run の FIFO を消してディスパッチを落とせてしまう（Copilot のレビューで指摘）。クラッシュしたプロセスのディレクトリは残るが、FIFO はディスクにデータを持たないので、秘密は残らない

# 代替案と不採用理由

- **今のまま打ち込む** — 秘密が画面に残る。この ADR の動機そのもの
- **env をファイルに書いて、読んだら消す** — ディスクに平文が残る時間がある。FIFO ならカーネルのパイプバッファを通るだけで済む
- **`. "$1"` で読む（issue の原案）** — macOS の `/bin/sh`（bash 3.2）では FIFO から何も読めない（Decision 3）
- **`libc::mkfifo` を呼ぶ** — `unsafe` と `CString` が要る。`mkfifo(1)` は POSIX にあり、`-m` で umask に関係なくモードを決められる。`libc` クレートから使うのは `O_NONBLOCK` / `ENXIO` の定数だけ
- **全プロセスで 1 つのディレクトリを共有し、起動時と終了時に掃除する（最初の実装）** — `doctor` / `config validate` のプローブが、実行中の run の FIFO を消せてしまう（Decision 7）
- **書き手のブロッキング `open` を、自分で O_RDWR を開いて解放する** — macOS では動いた。しかし「unlink 済みの FIFO の `open` に入った後」のレースを塞げない（Decision 5）

# Consequences

- HOOK_TOKEN と PROMPT_CONTEXT は、この変更だけで画面に出なくなる
- 秘密はディスクに書かれず、エージェントが起動した時点で FIFO は残っていない
- 打ち込むコマンドは `sh` を 1 段挟む。`exec` の連鎖なので、端末の寿命がエージェントの寿命と一致する性質は変わらない
- プラグインは起動時に `runtime_dir` を自分で求める（Slack の persist と同じやり方）。XDG の基底も `HOME` も絶対パスで無ければ `initialize` を失敗させる
- 検証は 3 段で行う。`handoff` の単体テスト（本物の FIFO と `/bin/sh` で往復する。`. "$1"` に戻すと落ちるのは `/bin/sh` が bash 3.2 の macOS だけで、Linux の CI では通ってしまう）、fake orca CLI の結合テスト（fake が読み手を演じ、読まれた中身と打ち込まれたコマンドを検査する）、実機（[live-e2e-orca](/components/live-e2e-orca.md)）
