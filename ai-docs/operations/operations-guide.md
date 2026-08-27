---
type: Runbook
title: 運用ガイド（doctor / worktree 掃除 / FAQ）
description: totsuka 日常運用の手引き。doctor の読み方、ランタイム health（縮退）の読み方と doctor との守備範囲の違い、worktree 掃除ポリシーと孤児掃除、run 停止・回復、メニューバー表示（SwiftBar）の導入と読み方、よくある問題の切り分け。
resource: https://github.com/tomoya-k31/totsuka
tags: [operations, doctor, health, worktree, menu, swiftbar, faq, troubleshooting]
generated: { by: claude-code/opus-5, at: 2026-08-28T08:45:00+09:00 }
status: stable
owner: tomoya-k31
---

> **このファイルは人間向け `docs/operations-guide.md` / `.ja.md` の生成元である。** 変更したら `human-docs` スキルで生成物も作り直すこと（`scripts/docs-freshness.sh` が CI で検査する）。
<!-- generates: docs/operations-guide.md docs/operations-guide.ja.md -->

# doctor の読み方

`totsuka doctor`（`--json` で機械可読）は次を診断する。各失敗は「原因 + 次のアクション」を表示する（§7）。

| チェック | ok の意味 | FAIL 時の代表対応 |
|---|---|---|
| `git` | git が PATH 上にある | git を導入 |
| `config` | config.toml が検証を通る | `totsuka config validate` で全エラー確認 |
| `state-db` | 状態 DB が開け、**スキーマ版数と適用したアプリ版数**を表示（`… opens — schema v7 (applied by 0.1.4)`。`applied by unknown` は `applied_by` 列を持たない旧版が適用したもので異常ではない） | まだ無いなら一度 `totsuka run`。**DB が新しすぎる**（ダウングレード）ならメッセージが名指す版以降へ totsuka を更新。**DB が古い**（アップグレード直後で未適用）なら一度 `totsuka run` — 適用するのは `run` だけで、`status` / `task` / `focus` / `doctor` は適用しない（→ [アップグレードとロールバック](/releases/upgrade-and-rollback.md)、[ADR-0017](/decisions/adr-0017-state-db-compatibility-policy.md)） |
| `worktree-location` | 明示した `[worktree].location` / `[[repositories]].worktree_location` が展開できる | `${ENV}` の未設定変数を export、またはキーを削って既定値（`$XDG_STATE_HOME/totsuka` 配下、未設定なら `$HOME/.local/state/totsuka`）に戻す。**worktree 作成はディスパッチ時**なので、これを放置すると run は正常起動したまま全タスクが失敗する |
| `plugin:{name}` | 起動 + `config/validate` 疎通 | install 済みか / `[<name>]` を修正 |
| `llm` | `api_key_ref` が**解決する**（鍵が有効かは見ない） | 1Password に item を作る / 環境変数 export / Keychain 登録 |
| `llm-online` | プロバイダが API キーを**受理した**（`--online` 時のみ） | 401/403 = 鍵をプロバイダで再発行し `[llm].api_key_ref` を更新。到達不能・5xx は warning 止まり（鍵が悪いとは限らない） |
| `worktrees` | 孤児 worktree なし | 対話的に掃除を提案（TTY） |
| `panes` | 孤児 agent pane なし（#211） | 対話的に解放を提案（TTY）。`pane_control` 宣言 agent が無い構成では出ない |
| `projects` | 起票先を claim しているリポジトリの数を報告する（#542、#554 で `trackers` から改名・縮小） | **検出ではなく報告**。重複はもう書けない — 1 リポジトリは `[[repositories]].project` で 1 つの `[[projects]]` を、その要素は `source` で 1 つのプラグインを指す。probe できなかったソースがあると skip（`cmd:` トークン等で doctor は起動しない）、claim が 1 件も無い構成では出ない |

`--json` 出力は不具合報告に添付する（Issue テンプレートが要求、§10.3）。

## `--online`（鍵の有効性検査、#267）

`llm` チェックが見るのは**参照が解決できるか**だけで、**その鍵が API に受理されるか**は見ない。両者は無関係で、実機では `op://` 参照が正しく解決する一方でプロバイダが全リクエストに 401 を返し続けている状態を `doctor` が `ok` と報告していた（[ADR-0016](/decisions/adr-0016-doctor-online-probe.md)）。

```bash
totsuka doctor --online
```

を付けると `[llm]` へ 1 回だけ最小リクエスト（`max_tokens: 1`・リトライなし・本文は破棄）を投げ、`llm-online` チェックとして結果を出す。**既定では実行しない**（`doctor` はオフライン・非対話が原則、[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md)）。`--online` が明示的に買うコスト:

- ネットワークに出る（わずかに課金される）— `doctor` でネットワークに触れるのはこのチェックだけ
- `op://` 参照を**実際に解決する** → 1Password の生体認証プロンプトが出うる

したがって **CI や cron からは使わない**。

> **注**: 生体認証プロンプトは `--online` 固有ではない。プラグインが 1 つでも enabled なら `plugin:{name}` チェックがプラグインを起動するために `plugin_spec` 経由で `[llm].api_key_ref` と `[<name>]` のシークレットを `op://` 含めて実解決するため、**フラグ無しの `doctor` でもプロンプトは出うる**。[ADR-0006](/decisions/adr-0006-onepassword-secret-backend.md) の「doctor は非対話」は `llm` チェック単体の話で、doctor 全体では既に成立していない（#267 以前からの既存挙動）。手元で「鍵を差し替えた直後」「リポジトリ選択 UI が毎回出る」ときの切り分けに使う。

**鍵が失効すると何が起きるか**: 候補リポジトリが 2 件以上ある構成では分類に LLM が要るため、鍵が無効だと [task-source-slack](/components/task-source-slack.md) の解決が毎回 picker へ縮退する。縮退自体は設計どおり安全なので、**設定不備が「少し不便な正常動作」に見える**のが厄介な点。run のログに次の `warn` が出ていたらこれ:

```text
WARN the LLM provider rejected the API key; repository selection falls back to
     the operator picker for every new conversation until it is fixed
```

（[task/lookup](/components/orchestrator-core.md) により 2 通目以降は LLM を呼ばないため、影響は新規会話に限られる。）

## `--no-repair`（検査だけする、#351）

```bash
totsuka doctor --no-repair
```

**`doctor` は既定では read-only ではない。** 検査のついでに、`run` と同じ書き出しを行う:

| 書き込み先 | 何を |
|---|---|
| `$XDG_DATA_HOME/totsuka/hooks` | フックスクリプトと workflow ごとの settings |
| `$CODEX_HOME/hooks.json` | totsuka の管理エントリ |
| opencode の config ディレクトリ | プラグイン + plan agent アセット |
| `$XDG_STATE_HOME/totsuka/hooks/spool` | ディレクトリ作成と書き込みプローブ |

これは「フル run なしでセットアップを完了させる」ための意図的な設計で、既定のまま変えない。ただし**そのままでは「純粋な監査」を表現できない** — 他人のマシンを点検する、CI で読み取り専用に走らせる、といった用途で `$CODEX_HOME` に書き込んでしまう。`--no-repair` はその 4 つを抑止する。

- **verify 側は全て走る。** 修復してから見た状態ではなく「見つけたままの状態」が報告される。`codex-hooks` が不一致を報告したときのアクションだけが変わる（「改竄を疑え」ではなく「`--no-repair` 無しで同期しろ」）
- **孤児 worktree / pane の掃除提案も出ない。** 読み取り専用の監査が削除を持ちかけるのは筋が通らない
- **代償**: `hook-spool` が書き込み可否を検証できない。ディレクトリが未作成なら warning（`ok: true`）に留め、失敗にはしない
- **チェックの集合と終了コードは変わらない。** `--no-repair` は書き込みを止めるだけで、検査を減らさない（テストで固定）

`doctor --fix`（残った fail を機械的に直す）は入れない。理由は [ADR-0028](/decisions/adr-0028-setup-wizard.md) の却下案に記録がある。

# ランタイム health（縮退の読み方）

`doctor` が「設定と環境は正しいか」を答えるのに対し、**health は「今動いている `run` が仕事を全部できているか」**を答える。守備範囲が違うので、どちらかで代用できない。

| | doctor | health |
|---|---|---|
| いつ | 人間が叩いたとき | `run` が毎サイクル自分で書く |
| 何を見る | 設定・環境・プラグイン疎通・孤児 | 今この run の縮退 4 種 |
| コスト | プラグインを起動し `op://` を実解決する（**生体認証が出うる**） | ゼロ（`run` が既に知っていることを書くだけ） |
| 読み方 | `totsuka doctor [--json]` | `totsuka status` の `degraded:` / `--json` の `health` / `totsuka menu` の `⚠` |

**`doctor` は数秒おきに叩けない**（プラグイン起動と秘密の実解決を伴う）ので、常時監視は health 側が引き受ける。

## 入るもの・入らないもの

health に入るのは **「今もそうか」を毎サイクル問い直せる事実だけ**である。一過性の失敗を入れるとフラグが消えなくなり、警告が背景ノイズになるため。

| 縮退 | 何が起きているか |
|---|---|
| `hook_receiver_down` | UDS を bind できなかった。**その run は全タスクの完了検知が効かない** — プロセスは元気に見えるので、これが `⚠` の最重要ケース |
| `plugin_down` | プラグインが落ちている。`abandoned` なら supervisor が再起動を諦めた後で、待っても戻らない |
| `spool_backlog` | `replay_spool` が drain できなかった `*.jsonl` が残っている |
| `llm_key_rejected` | ゲートウェイが鍵を 401/403 で拒否した。**成功した呼び出しで自動的に解除される** |

入らないもの: `notify delivery failed` / `worktree cleanup failed` のような一過性の失敗（再評価できないので永久に残る）と、**隔離済みの `*.jsonl.corrupt`**（自動回収されないので数えると `⚠` が永久化する。あちらは `doctor` の `hook-spool` チェックの担当）。

## 停止中の扱い

**`run.lock` が health より優先する。** `run` が SIGKILL された場合 `health.json` は残るが、それは存在しないプロセスの話なので読まない（`pid` も突き合わせる）。したがって停止中は必ず `✕` であって `⚠` にはならず、`status --json` の `health` キーもごと消える。

## 黙った run（stale）

pid は生きているのに **120 秒 republish が無い** health は stale として扱い、`⚠` の理由に 1 行足す。**捨てない**のが要点で、捨てると「黙っている run について健全と報告する」ことになる —— プラグイン呼び出しでハングした run は pid を保ったままなので、これが唯一の手掛かりになる。

stale は `run` が publish するものではなく**読み手の判断**である（黙った run は「自分は黙っている」と書けない）。したがって `status --json` は判断材料そのものも出す:

```bash
totsuka status --json | jq '{recorded: .health.recorded_at, stale: .health.stale}'
```

```bash
totsuka status --json | jq '.health // "not running"'
ls -l "${XDG_STATE_HOME:-$HOME/.local/state}/totsuka/health.json"   # run 中だけ存在する
```

# worktree 掃除

「1 タスク = 1 worktree」の後始末は掃除ポリシーで決まる。

- `[worktree].cleanup`（implement 既定 `manual`）/ `plan_cleanup`（plan 既定 `immediate`）: `immediate` / `manual` / `{ retention_days = N }`
- **未コミット変更のある worktree は決して自動削除しない**（データ損失防止、F-23）
- `retention_days` は完了後 N 日で削除。`run` の各サイクルで再評価される
- どのタスクにも属さない **孤児 worktree** は `totsuka doctor` が検出し、TTY 上で対話的に `git worktree remove` を提案する（F-24）。dirty なものは skip

手動で消す場合は `git worktree remove <path>`（committed-but-unpushed があるなら `--force` は慎重に）。**手動削除では pane 解放の連動（#210）が働かない**ため、残った pane は次の `totsuka doctor` の孤児 pane チェックで回収する（下記）。

## ブランチの後始末（#266）

worktree を削除するとき、その `agent/*` ブランチも一緒に消す。判定は **「origin に無いコミットを持っているか」** の一点:

- 全コミットが origin のどこかのリファレンスから辿れる → `git branch -D` で削除（失うものが無い）
- 1 つでも origin に無い → **ブランチを残す**（未 push の成果物がそこにしか無い）。`totsuka run` のログに `branch kept: it has commits that are not on origin` が出る

> squash merge されたブランチは、元のコミットハッシュが origin に存在しない。`origin/{branch}` が prune されると「未 push」と数えられ、以後は保持され続ける（totsuka 自身は prune しないが、グローバル設定 `fetch.prune = true` があると踏む）。失敗方向は保持なので失うものは無く、下記のワンライナーで手動削除できる。

**#266 より前は `git branch -d` を使っており、ほぼ常に失敗していた。** `-d` の「マージ済み」判定はローカルの `HEAD` が基準だが、worktree のブランチは `origin/{default}` から切られる。**ローカルの既定ブランチが origin より遅れているのは常態**なので、判定が通らず削除が拒否され、しかも結果が握り潰されていたためログにも出なかった。

すでに溜まってしまった `agent/*` ブランチは、同じ基準で手動掃除できる:

```bash
# 削除して安全なもの（origin に無いコミットがゼロ）を一覧
for b in $(git branch --format='%(refname:short)' --list 'agent/*'); do
  [ "$(git rev-list --count "$b" --not --remotes=origin)" = 0 ] && echo "$b"
done

# 確認したうえで削除
for b in $(git branch --format='%(refname:short)' --list 'agent/*'); do
  [ "$(git rev-list --count "$b" --not --remotes=origin)" = 0 ] && git branch -D "$b"
done
```

# 孤児 pane の掃除（#211）

worktree↔pane の連動（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）が破れると herdr に totsuka の pane だけが残る（手動 `git worktree remove`・解放拒否・クラッシュ・#210 以前の残骸）。`totsuka doctor` が worktree と対称の受け皿になる（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）:

- `pane_control` 宣言の agent プラグインに `session/list`（protocol 0.2.2）で自分の pane を列挙させ、DB と突き合わせる。候補 = 「対応するタスクが DB に無い」または「タスクが終端かつ worktree も既に無い」。実行中タスクや保持ポリシー（`keep_7d` 等）で worktree が残っているタスクの pane は候補にならない
- TTY では 1 件ずつ `[y/N]` で解放（`session/release`）を提案。`--json` / 非 TTY は検出のみ（`panes` チェックの FAIL）。**無人自動解放はしない**（孤児 worktree と同方針）
- herdr が落ちている等で列挙できないときは warning に留まり、他のチェックは続行する

# 停止・回復

- `run --watch` は SIGINT で graceful 停止。実行中タスクは状態 DB に残し、ロックを解放する（F-74）
- 異常終了（SIGKILL 含む）後の再起動は、状態 DB からセッション ID を復元し `session/attach` で再接続を試みる（§5.3）。再接続不能なタスクは **自動 failed にせず**「継続確認待ち」として残り、`totsuka task retry <id>` / `task cancel <id>` を人間が選ぶ
- `run` の多重起動は `$XDG_STATE_HOME/totsuka/run.lock` + PID で防止。`totsuka status` は run 停止中に stale を明示する

# タスク操作

- `totsuka status [--json]`: 実行中 / 待機（waiting_input・pending）タスクと worktree 一覧。**`Queued` のまま動かないタスクに理由が付いていればそれも出す**（`not starting yet:` ブロック / `--json` の `wait_reason`）。現状の唯一の理由は `blocked_agent_tools`（#399 の外部ツール未整備）で、対処は [config.toml リファレンス](/development/config-reference.md) 参照
- `totsuka task show <id>`: 状態・セッション履歴・worktree・イベント全履歴
- `totsuka task cancel <id>` / `retry <id>`: retry は failed/cancelled のみ。worktree/セッションを再利用して再開（F-44）
- `totsuka logs [-f] [--task <id>]`: JSON Lines ログの整形表示。機密は logging layer で無条件マスク（§5.2）

# メニューバー表示（SwiftBar）

`run --watch` を回している間、**人間が動かさない限り止まったままのタスク**に気づく手段は、放っておくと通知だけになる。通知は一過性で、しかも配送失敗は握り潰される（F-93）ので、「通知が来ない」は「異常なし」を意味しない。常時視界に入る面へ [要対応](/glossary/attention.md)件数を出すのが `totsuka menu`（F-109、[ADR-0065](/decisions/adr-0065-menubar-status.md)）。

## 読み方

2 チャネルで、独立に読む。

| チャネル | 意味 |
|---|---|
| 形 | `○` = 健全 / `⚠` = 生きているが縮退している（上記 health） / `✕` = 停止中・stale lock |
| 数 | 要対応の件数。**0 件なら数字そのものが出ない** |

数に入るのは `pending` / `waiting_input` / `verifying` / `escalated` / `queued`+`wait_reason` の 5 状態だけで、**終端状態（`done` / `failed` / `cancelled` / `skipped`）は数えない**。数えると `totsuka status` の表と同じく単調に増え続け、0 に戻らなくなるため。失敗の確認は `totsuka status` の担当。

ドロップダウンは「Needs you」（要対応）と「Working」（`dispatched` / `running` / `publishing`）の 2 節。タスク行をクリックすると `totsuka focus <id>` が走り、その pane が前面に来る。**状態を変えるボタンは無い** — 検収（`task verify --pass`）は押した瞬間に PR 作成や本人名義返信まで走って取り消せないので、メニューには置いていない。

## 導入

**SwiftBar は別途インストールが要る**（totsuka はこのファイルを書き込まない — SwiftBar のプラグインフォルダは初回起動時にユーザーが選ぶもので、固定パスが無いため）。

```bash
brew install --cask swiftbar   # 初回起動でプラグインフォルダを選ぶ

# **サブシェルで囲む。** 中の `exit` は失敗したときにセットアップを止めるための
# もので、囲まないと対話シェルに貼ったときターミナルのウィンドウごと閉じる。
(
  set -eu

  # 選んだフォルダは SwiftBar 自身が覚えている。`~/SwiftBar` とは限らない
  # （実機では `~/.config/swiftbar` だった）。まだ SwiftBar を起動していないと
  # このキーは存在せず空になる — 空のまま進むと `/totsuka.5s.sh` のような
  # 無関係な場所を作りにいくので、ここで止める。
  dir=$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null) || {
    echo "SwiftBar のプラグインフォルダが未設定。一度起動して選ぶこと" >&2; exit 1; }
  [ -n "${dir}" ] || { echo "PluginDirectory が空" >&2; exit 1; }

  # totsuka も同じく確認してから使う。PATH に無いと `$(command -v totsuka)` は
  # 空へ展開され、`exec  menu` という壊れた行が黙って焼き込まれる。
  bin=$(command -v totsuka) || { echo "totsuka が PATH に無い" >&2; exit 1; }

  # `${bin}` はここで展開され、絶対パスがファイルへ焼き込まれる。
  # ヒアドキュメントを引用符で囲まないのがその要点。
  mkdir -p "${dir}"
  cat > "${dir}/totsuka.5s.sh" <<EOF
#!/bin/sh
exec ${bin} menu
EOF
  chmod +x "${dir}/totsuka.5s.sh"

  cat "${dir}/totsuka.5s.sh"   # 焼き込まれたパスを目で確認する
)
```

- ファイル名の `5s` が更新間隔である（SwiftBar の規約）。`totsuka menu` は状態 DB を直読みするだけで、**実測 7ms/回**（100 回連続実行の平均、20 タスクの実 DB・プロセス起動込み）。この間隔でも負荷にならない
- **`totsuka` は絶対パスで焼き込む。** **プラグインが走る `PATH` は予測できない**ためで、名前で呼ぶとターミナルからだけ動いて SwiftBar 経由では「command not found」になる。実測では Homebrew と mise shims は入っていたが **`/usr/local/bin` は無かった**（shims は全ての zsh が読む `.zshenv` 由来、`/usr/local/bin` は login shell でしか走らない `/etc/zprofile` の `path_helper` 由来）。SwiftBar 自身の起動方法にも依存する（launchd 起動なら `/usr/bin:/bin:/usr/sbin:/sbin` だけ）。スクリプトが `env -i`（PATH ゼロ）でも動くことは実機で確認済み
- **パスをベタ書きしない。** インストール方法で変わる —— tarball 配置なら `/usr/local/bin/totsuka`、Homebrew なら Apple Silicon で `/opt/homebrew/bin/totsuka`、Intel で `/usr/local/bin/totsuka`。上の `$(command -v totsuka)` はそのどれでも正しく解決する
- メニュー項目のクリック先（`totsuka focus <id>` 等）は totsuka 自身が `current_exe()` から出すので、**そちらは設定不要**である

## 出ないとき

| 症状 | 見るところ |
|---|---|
| メニューバーに何も出ない | プラグインフォルダにファイルがあるか、実行ビットが立っているか。`"$(defaults read com.ameba.SwiftBar PluginDirectory)/totsuka.5s.sh"` を直接実行して出力を見る |
| 項目が壊れて見える / 空 | スクリプトの `totsuka` が絶対パスか。`env -i "$(command -v totsuka)" menu` で最小環境でも動くか確認する（`command -v` は `env -i` の**外**で展開される — 中では `PATH` が無く解決できない） |
| `✕` のまま | `run` が動いていない。`totsuka status` の 1 行目と一致するはず（一致しないなら不具合） |
| `⚠` のまま | 縮退している。ドロップダウンに理由が出る。`totsuka status` の `degraded:` と同じ内容 |
| `⚠` で「may be wedged」と出る | run が 120 秒以上 health を更新していない。`totsuka logs -f` で最後に何をしていたか見る |
| クリックしても何も起きない | タスク行の `totsuka focus` は `run` 停止中や pane 消失で**静かに縮退する**（設計どおり）。実際に何が走るかは `Open logs` を押すと分かる —— SwiftBar は `SWIFTBAR_*` を export した login shell を開き、組み立てたコマンド行をそのまま表示する。**その環境はプラグインスクリプトの env を引き継がない**（`XDG_STATE_HOME` 等は渡らず、既定の XDG に解決される） |
| 件数が `totsuka status` と合わない | 終端状態は数えない仕様。`totsuka menu --json` の `attention` 配列と突き合わせる |

**`totsuka menu` は失敗しても exit 0 で、原因をメニューの 1 行として出す**（状態 DB が無い、migration が未適用、`HOME` すら無い最小環境で XDG パスが解決できない等）。非ゼロ終了するとメニュー項目ごと壊れるための設計なので、「エラーが出ない」ことを健全さの証拠にしないこと — メニューの本文を読む。なお **`config.toml` は読まない**ので、config が壊れていても `menu` の表示は変わらない（切り分けには `totsuka config validate` を使う）。

# FAQ / 切り分け

- **`config not found`**: `totsuka init` で雛形生成 → 編集
- **`state database not found`**: 一度 `totsuka run` すると作成される
- **プラグインが `enabled but not installed`**: `totsuka plugin install <dir>`
- **タスクが取り込まれない**: `totsuka run --dry-run` でトリガーマッチ・リポジトリ選択・エージェント割当を副作用ゼロで確認。ワークフローの `source` は `[plugins.{name}]` のインスタンス名と一致させる
- **リポジトリ選択が `pending`**: `[llm]` 未設定 or 確信度が低い。単一リポジトリなら自動選択、複数なら `[llm]` を設定するか `repo_hint` を付与
- **`totsuka task show` にブランチが出ない**: エージェントがブランチを切っていない（worktree は detached HEAD で渡る）。コミットがあれば掃除は worktree を残すので、そこで作業を拾える。plan モードは常にこの状態が正常
- **通知が来ない**: `[plugins.{notifier}] enabled` と `notifier` プラグイン疎通を `doctor` で確認。配送失敗はタスク実行を止めない（F-93）

リリース前の実機確認は [リリース前手動チェックリスト](/quality/release-checklist.md) を参照。
