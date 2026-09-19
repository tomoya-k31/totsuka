# 症状から引く表（orca 版）

GitHub / Slack 側の症状は herdr 版の [troubleshooting.md](../../live-e2e-herdr/references/troubleshooting.md) を見る。
ここは orca プラグインと orca 本体に固有のもの。事実の出典は
[orca CLI 制御サーフェス](../../../../ai-docs/references/orca-cli-control.md) の実測節（orca 1.4.205）。

**orca の CLI はエラーも stdout に出す。** `--json` の拒否は `{"ok": false, "error": {"code": …}}` が
stdout に出て終了コード 1。**stderr だけ・終了コードだけを見て判断しない**（`orca.sh` は envelope を読む）。

| 症状 | 原因 | 読み方・直し方 |
|---|---|---|
| dispatch が `orca does not know the worktree … register its repository` で失敗 | サンドボックス repo が orca に未登録（`selector_not_found`） | `orca.sh preflight` → 承認を取って `orca repo add --path …` |
| dispatch は成功したのに、画面の `❯` が空のまま（タスク本文が無い） | orca がエージェントを認識する前にプロンプトを送った。`provider: "unsupported"` の生キー入力は Claude に届かない | プラグインは `agentIdentity` を最大 30 秒待つ。ログに `orca did not recognise an agent … sending the prompt anyway` が出ていたらこれ。orca の版が変わって認識の仕方が変わった可能性が高い — 実測節を取り直す |
| `session/list` が空・`tt doctor` の panes が orca の端末を数えない | タスクの worktree に所有マーカー（orca の comment `totsuka <id>`）が付いていない | `orca worktree show --worktree path:<worktree> --json` の `comment`。ログ `could not mark the task's worktree` が出ていたら `worktree set` が拒否された。**タブタイトルは見ない** — 作業中のエージェントが OSC で書き換え続けるので所有の判定には使えない（`rename` も 5 秒保たなかった） |
| サイドバーに worktree が出ない | repo 設定の `externalWorktreeVisibility` が `show` でない | `orca repo show --repo path:<repo> --json` で確認。GUI の repo 設定で表示にする |
| `orca worktree list` / `worktree ps` にタスクの worktree が無い | **正常。** totsuka の worktree は orca から見ると external で、`--repo` 無しの一覧と `ps` には出ない | `orca worktree list --repo path:<repo> --json` には出る |
| `session/attach` の state が常に `running` | **正常。** attach は `worktree ps` の status を読むが、external worktree は `ps` に出ないので既定の `running` になる | 端末の生死（`connected`）は別に正しく判定している |
| エージェントを落としたのに `failed` にならない | 起動に `exec` が付いていない（シェルが残り、端末が終了しない） | `orca.sh snapshot` でシェルのプロンプトが見えたらこれ。プラグインの `launch.rs` を確認 |
| `failed` の `log_chunk` に `exit code 0` | **正常。** orca の `exitCode` は実際の値を反映しない（`exit 3` が 0 と報告された） | 終了した事実だけを見る |
| 起動時に orca が `CONFIG_INVALID` | `[orca]` に廃止キー（`agent` / `setup` / `repo_selector` / `plan_prompt_prefix` / `poll_interval_ms`） | メッセージにキー名と代替が出る。消す |
| 30 秒ほどで dispatch が `timed out` | `request_timeout_secs` より長くかかった呼び出しがある | Orca アプリが固まっていないか（`orca status`）。`terminal wait` / `terminal send --wait-submit` は自分の待ち時間＋10 秒まで延ばしてある |
| 直したはずの挙動が変わらない | インストール済みのコピーが古い | `orca.sh preflight` のプラグイン行。`tt plugin install --from-source --yes orca` |
| プラグインが自分自身を呼んで固まる | `target/{profile}` を PATH に入れている（バイナリ名が外部の `orca` と同じ） | PATH から外すか、`[orca] orca_bin` に絶対パスを書く |
| deadman の試験（`exit-agent`）でタスクが `failed` ではなく `done` になった | 以前の `exit-agent` は `/exit` を文字で送っていて、完了確認の質問（Approve completion）が出ていると回答として扱われた | 今の `exit-agent` は worktree を cwd に持つプロセスへ SIGTERM を送る。質問が出ていても関係なく落ちる |
| implement のタスクが最後に `waiting_input` で止まる | **正常。** profile が人間の完了確認を求めるとき、Claude は `AskUserQuestion` で「Approve completion」を聞く（ADR-0050）。herdr でも同じ | 人間が Orca のタブで答える。無人で回したいなら profile の confirm 方針を見直す |
