> 🌐 [English](operations-guide.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/operations-guide.md sha256:cdefbc5bdaad5a7eecd1649bcb62ff6ca82dded352b647c5128f09cdbe2ef296 -->

# 運用ガイド

日常運用の手引き。`doctor` の読み方、worktree と pane の掃除、停止と回復、タスク操作、よくある問題の切り分けを扱う。

## doctor の読み方

`totsuka doctor` は環境を診断する。`--json` で機械可読な出力になる。失敗したチェックには原因と次のアクションが表示される。

| チェック | ok の意味 | FAIL したら |
|---|---|---|
| `git` | git が PATH 上にある | git を導入する |
| `config` | `config.toml` が検証を通る | `totsuka config validate` で全エラーを確認する |
| `state-db` | 状態 DB が開ける。スキーマ版数と、それを適用した totsuka の版数を表示する | まだ無いなら一度 `totsuka run`。DB が新しすぎる（ダウングレードした）ならメッセージが名指す版以降へ totsuka を更新する。DB が古い（アップグレード直後）なら一度 `totsuka run` — スキーマを適用するのは `run` だけで、`status` / `task` / `focus` / `doctor` は適用しない |
| `worktree-location` | 明示した worktree の配置先が展開できる | `${ENV}` の未設定変数を export するか、キーを削って既定値に戻す |
| `plugin:{name}` | プラグインが起動し、設定の検証に応答する | インストール済みか確認し、`config.toml` の `[<name>]` テーブルを修正する |
| `llm` | `api_key_ref` が解決する（鍵が有効かは見ない） | 1Password に item を作る、環境変数を export する、Keychain に登録する、のいずれか |
| `llm-online` | プロバイダが API キーを受理した（`--online` 時のみ） | 401 / 403 ならプロバイダで鍵を再発行し `[llm].api_key_ref` を更新する。到達不能や 5xx は警告止まり |
| `worktrees` | 孤児 worktree が無い | 対話的に掃除を提案する |
| `panes` | 孤児 pane が無い | 対話的に解放を提案する |
| `projects` | 起票先を claim しているリポジトリの数を報告する | **検出ではなく報告** — 重複はもう書けない。各リポジトリは `project` で 1 つの `[[projects]]` を、その要素は `source` で 1 つのプラグインを指す。probe できなかったソースがあると skip（`cmd:` トークンのプラグインは doctor から起動されない）、どのソースも何も claim していない構成では出ない |

`worktree-location` の失敗は放置すると厄介で、**worktree を作るのはタスクを割り当てる瞬間**なので、`run` は正常に起動したまま全タスクが失敗する。

不具合を報告するときは `--json` の出力を添付する。

### `--online` — 鍵が実際に使えるかを確かめる

`llm` チェックが見るのは**参照が解決できるか**だけで、その鍵をプロバイダが受理するかは見ない。この 2 つは無関係で、参照が正しく解決しているのにプロバイダが全リクエストを 401 で拒否し続ける、という状態があり得る。

```bash
totsuka doctor --online
```

を付けると `[llm]` へ最小のリクエストを 1 回だけ投げ（リトライなし、応答本文は破棄）、`llm-online` チェックとして結果を出す。既定では実行しない。明示したときのコストは 2 つ:

- ネットワークに出る（わずかに課金される）。`doctor` がネットワークに触れるのはこのチェックだけ
- シークレット参照を実際に解決するため、1Password の生体認証プロンプトが出ることがある

このため **CI や cron からは使わない**。

生体認証プロンプト自体は `--online` に固有ではない。プラグインが 1 つでも有効なら、`plugin:{name}` チェックはプラグインを起動するためにシークレットを実解決するので、フラグ無しの `doctor` でもプロンプトは出うる。

**鍵が失効すると何が起きるか。** 候補リポジトリが 2 つ以上ある構成では、どのリポジトリのタスクかを判定するのに LLM を使う。鍵が無効だと判定できず、毎回あなたに選ばせる画面へ縮退する。縮退そのものは安全側の動作なので、**設定不備が「少し不便なだけの正常動作」に見えてしまう**のが厄介な点。`run` のログに次の警告が出ていたらこれが起きている。

```text
WARN the LLM provider rejected the API key; repository selection falls back to
     the operator picker for every new conversation until it is fixed
```

影響は新規の会話に限られる。同じ会話の 2 通目以降は判定をやり直さない。

### `--no-repair` — 検査だけする

**`doctor` は既定では読み取り専用ではない。** 検査のついでに `run` と同じセットアップを書き出す。

| 書き込み先 | 何を |
|---|---|
| `$XDG_DATA_HOME/totsuka/hooks` | フックスクリプトとワークフローごとの設定 |
| `$CODEX_HOME/hooks.json` | totsuka の管理エントリ |
| opencode の設定ディレクトリ | プラグインと plan エージェントのアセット |
| `$XDG_STATE_HOME/totsuka/hooks/spool` | ディレクトリ作成と書き込みの確認 |

これは「フルの `run` をしなくてもセットアップが完了する」ための意図的な設計である。ただしそのままでは純粋な監査ができない — 他人のマシンを点検する、CI で読み取り専用に走らせる、といった用途で `$CODEX_HOME` に書き込んでしまう。

```bash
totsuka doctor --no-repair
```

はこの 4 つの書き込みを抑止する。

- **検査は全て走る。** 修復後の状態ではなく、見つけたままの状態が報告される
- **孤児 worktree / pane の掃除提案は出ない。** 読み取り専用の監査が削除を持ちかけるのは筋が通らない
- **代償**: 書き込み可否を検証できないため、spool ディレクトリが未作成なら警告に留め、失敗にはしない
- **チェックの集合と終了コードは変わらない。** 書き込みを止めるだけで、検査を減らさない

## うまく動いていないとき

`totsuka doctor` が答えるのは「設定と環境が正しいか」である。これは繰り返し叩けるものではない —— プラグインを実際に起動して秘密を本当に解決するので、Mac では生体認証を求められることがある。

**動作中の totsuka が今できていること・できていないことは別の問いで**、こちらは自動で答えが出る。`run` は毎サイクル、今縮退している内容を書き出しており、`totsuka status` がそれを読む。

```bash
totsuka status              # 1 行目の下に `degraded:` ブロックが出る
totsuka status --json | jq '.health // "not running"'
```

出るのは 4 種類で、いずれも直せば自動的に消える。

| 出るもの | 意味 |
|---|---|
| hook 受信が bind できなかった | **その run では、どのタスクも完了を報告できない。** プロセスは完全に健全に見えるので、見落とすと一番たちが悪い。ソケットのパスが空いてから `totsuka run` を起動し直す |
| プラグインが落ちている | それを必要とするタスクは待機のまま。「再起動しない」と出ていれば待っても無駄なので、直して起動し直す |
| hook のシグナルが spool に滞留している | 配送が失敗し続けている。ソケットのパスとトークンを `doctor` で確認する |
| LLM ゲートウェイが API キーを拒否した | リポジトリの選択が、新しい会話のたびに人間へ聞く形に縮退する。キーを再発行して `[llm].api_key_ref` を更新する |

**停止している totsuka に health は無く、あるのはロックだけである。** `run` が強制終了された場合、最後の報告はディスクに残るが読まれない —— 出るのは必ず「動いていない」で、「縮退している」にはならない。

**黙ってしまった run は別扱いである。** プロセスは生きているのに 2 分間報告が無い場合、`⚠` と「ハングしている可能性がある」旨の 1 行が出る。報告を捨てないのは、捨てると「健全」と読めてしまうためである。`totsuka status --json` は `health.recorded_at` と `health.stale` を持つので、自分で判断できる。

## worktree の掃除

「1 タスク = 1 worktree」の後始末は掃除ポリシーで決まる。

- `[worktree].cleanup`（implement の既定は `manual`）と `plan_cleanup`（plan の既定は `immediate`）に、`immediate` / `manual` / `{ retention_days = N }` を指定する
- **未コミットの変更がある worktree は決して自動削除しない。** データを失わないための安全弁
- `retention_days` は完了から N 日後に削除する。`run` の各サイクルで再評価される
- どのタスクにも属さない**孤児 worktree** は `doctor` が検出し、`git worktree remove` を対話的に提案する。未コミット変更があるものは飛ばす

手動で消すなら `git worktree remove <path>`。未 push のコミットがあるときの `--force` は慎重に。**手動削除では pane の解放が連動しない**ので、残った pane は次の `doctor` で回収する。

### ブランチの後始末

worktree を削除するとき、その `agent/*` ブランチも一緒に消す。判定は**「origin に無いコミットを持っているか」の一点**。

- 全てのコミットが origin のどこかから辿れる → 削除する（失うものが無い）
- 1 つでも origin に無い → **ブランチを残す**。未 push の成果物がそこにしかないため。`run` のログに `branch kept: it has commits that are not on origin` が出る

squash merge されたブランチは、元のコミットハッシュが origin に存在しない。`origin/{branch}` が削除されると「未 push」と数えられ、以後は残り続ける。失敗する方向が「残す」なので失うものは無い。溜まったものは同じ基準で手動掃除できる。

```bash
# 削除して安全なもの（origin に無いコミットがゼロ）を一覧する
for b in $(git branch --format='%(refname:short)' --list 'agent/*'); do
  [ "$(git rev-list --count "$b" --not --remotes=origin)" = 0 ] && echo "$b"
done

# 確認したうえで削除する
for b in $(git branch --format='%(refname:short)' --list 'agent/*'); do
  [ "$(git rev-list --count "$b" --not --remotes=origin)" = 0 ] && git branch -D "$b"
done
```

## 孤児 pane の掃除

worktree と pane の連動が破れると、pane だけが残る（手動での worktree 削除、解放の拒否、クラッシュなど）。`doctor` が受け皿になる。

- pane を操作できるエージェントプラグインに自分の pane を列挙させ、状態 DB と突き合わせる。候補は「対応するタスクが DB に無い」か「タスクが終了済みで worktree も既に無い」もの。実行中のタスクや、保持ポリシーで worktree が残っているタスクの pane は候補にならない
- 端末があれば 1 件ずつ確認しながら解放する。`--json` や非対話環境では検出のみ
- **無人での自動解放はしない。** 孤児 worktree と同じ方針
- エージェント側が落ちていて列挙できないときは警告に留め、他のチェックは続行する

## 停止と回復

- `run --watch` は Ctrl-C で穏やかに停止する。実行中のタスクは状態 DB に残り、ロックは解放される
- 異常終了した後の再起動では、状態 DB からセッションを復元して再接続を試みる。再接続できなかったタスクは**自動で失敗にはせず**「継続確認待ち」として残るので、`totsuka task retry <id>` か `totsuka task cancel <id>` を選ぶ
- `run` の多重起動はロックファイルと PID で防いでいる。`totsuka status` は `run` が止まっている間、情報が古いことを明示する

## タスク操作

| コマンド | 何をするか |
|---|---|
| `totsuka status [--json]` | 実行中・待機中のタスクと worktree の一覧。動き出さないタスクに理由が付いていればそれも表示する |
| `totsuka task show <id>` | 状態、セッション履歴、worktree、イベントの全履歴 |
| `totsuka task cancel <id>` | タスクを中止する |
| `totsuka task retry <id>` | 失敗・中止したタスクを、worktree とセッションを再利用して再開する |
| `totsuka logs [-f] [--task <id>]` | ログの整形表示。機密は無条件にマスクされる |

`retry` が受け付けるのは失敗・中止したタスクだけで、完了したタスクは再実行できない。

## メニューバーで見張る

`run --watch` を回している間、「自分待ちで止まっているタスク」を知らせてくれるのは通知だけで、通知は流れて消える。しかも配送に失敗してもタスクを止めない設計なので、**通知が来ないことは異常が無いことを意味しない**。`totsuka menu` は、その件数を残り続ける場所に出す。

### 読み方

チャネルは 2 つで、独立に読む。

| チャネル | 意味 |
|---|---|
| 形 | `○` 動いていて健全 · `⚠` 動いているが縮退している（上記） · `✕` 止まっている、または stale lock が残っている |
| 数 | 自分の対応を待っているタスクの件数。**0 件なら数字そのものが出ない** |

数に入るのは `pending` / `waiting_input` / `verifying` / `escalated` / 理由が記録された `queued` の 5 状態。**終わったタスクは決して数えない** —— データベースに残り続けるので、数えると数字が増える一方で 0 に戻らなくなる。失敗の確認は `totsuka status` で行う。

ドロップダウンは「Needs you」と「Working」の 2 節。タスク行をクリックすると `totsuka focus <id>` が走り、その pane が前面に来る。**状態を変えるボタンは無い** —— 検収を通すとプルリクエストの作成や本人名義の返信まで走って取り消せないので、うっかり押せる場所には置かない。

### 導入

**SwiftBar は別途インストールが要る**。また totsuka はこのファイルを書き込まない —— SwiftBar のプラグインフォルダは初回起動時に自分で選ぶもので、書き込み先が一意に決まらないためである。

```bash
brew install --cask swiftbar   # 初回起動でプラグインフォルダを選ぶ

# 選んだフォルダは SwiftBar 自身が覚えている。`~/SwiftBar` とは限らない
# （検証した機体では `~/.config/swiftbar` だった）。
dir=$(defaults read com.ameba.SwiftBar PluginDirectory)

# `$(command -v totsuka)` はここで展開され、絶対パスがファイルに焼き込まれる。
# ヒアドキュメントを引用符で囲まないのがその要点。
mkdir -p "${dir}"
cat > "${dir}/totsuka.5s.sh" <<EOF
#!/bin/sh
exec $(command -v totsuka) menu
EOF
chmod +x "${dir}/totsuka.5s.sh"

cat "${dir}/totsuka.5s.sh"   # 焼き込まれたパスを読み返す
```

- ファイル名の `5s` が更新間隔で、これは SwiftBar の規約。`totsuka menu` は状態データベースを読むだけで、**実測 7ms/回**（実データベースに対する 100 回連続実行の平均、プロセス起動込み）。この間隔でも負荷にならない。
- **絶対パスを焼き込む。** GUI から起動されたプロセスは `/usr/local/bin` も mise も含まない最小の `PATH` を継承するので、`totsuka` を名前で呼ぶとターミナルからは動いて SwiftBar 経由では失敗する。スクリプトを `env -i` で実行して確認済み。
- **パスをベタ書きしない。** インストール方法で変わる —— tarball なら `/usr/local/bin/totsuka`、Homebrew は Apple Silicon で `/opt/homebrew/bin/totsuka`、Intel で `/usr/local/bin/totsuka`。上の `$(command -v totsuka)` はそのどれでも解決する。
- メニュー項目のクリック先（`totsuka focus <id>` 等）は totsuka 自身が `current_exe()` から出すので、設定は要らない。

### 出ないとき

| 症状 | 見るところ |
|---|---|
| メニューバーに何も出ない | SwiftBar のプラグインフォルダにファイルがあるか、実行ビットが立っているか。`~/SwiftBar/totsuka.5s.sh` を直接実行して出力を見る |
| 項目が壊れて見える・空になる | `totsuka` が絶対パスか。`env -i /usr/local/bin/totsuka menu` で最小環境でも動くか確かめる |
| `✕` のまま | `run` が動いていない。`totsuka status` の 1 行目と一致するはず |
| `⚠` のまま | 何かが縮退している。ドロップダウンに理由が出る（`totsuka status` の `degraded:` と同じ内容） |
| タスクをクリックしても何も起きない | 設計どおりである。pane の前面化は totsuka が停止中や pane 消失のとき静かに縮退する。クリックで実際に何が走るかは `Open logs` を押すと分かる —— SwiftBar が login shell を開き、組み立てたコマンド行をそのまま表示する。**その shell はプラグインスクリプトの環境を引き継がない**ので、そこで設定したもの（`XDG_STATE_HOME` 等）は渡らない |
| 件数が `totsuka status` と合わない | 終わったタスクを数えないのは仕様。`totsuka menu --json` の `attention` 配列と突き合わせる |

**`totsuka menu` は失敗しても exit 0 で終わり**、原因をメニューの 1 行として出す（状態データベースが無い、マイグレーションが未適用、パスすら解決できない最小環境で動かした等）。メニューバーのプラグインが非ゼロ終了すると項目ごと壊れるためで、「エラーが出ない」ことを健全さの証拠にせず、メニューの本文を読むこと。なお `config.toml` は読まないので、設定が壊れていても表示は変わらない（それを見るのは `totsuka config validate`）。

## よくある問題

| 症状 | 対処 |
|---|---|
| `config not found` | `totsuka init` で雛形を作って編集する |
| `state database not found` | 一度 `totsuka run` すると作成される |
| プラグインが `enabled but not installed` | `totsuka plugin install <dir>` |
| タスクが取り込まれない | `totsuka run --dry-run` でトリガーの一致・リポジトリ選択・エージェント割当を副作用ゼロで確認する。ワークフローの `source` はプラグインのインスタンス名と一致させる |
| リポジトリ選択が `pending` のまま | `[llm]` が未設定か、判定の確信度が低い。リポジトリが 1 つなら自動選択される。複数なら `[llm]` を設定するか、依頼に `repo_hint` を付ける |
| `task show` にブランチが出ない | エージェントがブランチを切っていない（worktree は detached HEAD で渡される）。コミットがあれば worktree は残るので、そこから作業を拾える。plan モードでは常にこの状態が正常 |
| 通知が来ない | 通知プラグインが有効かと疎通を `doctor` で確認する。配送に失敗してもタスクの実行は止まらない |

---

このページは内部ドキュメント `ai-docs/operations/operations-guide.md` から生成されている。設計上の判断や実測の経緯はそちらにある。
