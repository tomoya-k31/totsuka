> 🌐 [English](operations-guide.md) · **日本語**
> _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_

<!-- generated-from: ai-docs/operations/operations-guide.md sha256:24f725bfab72270f89fc6f89e1aed76efe2a1cc3e719132806504dfd41132450 -->

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
| `plugin:{name}` | プラグインが起動し、設定の検証に応答する | インストール済みか確認し、`plugins/{name}.toml` を修正する |
| `llm` | `api_key_ref` が解決する（鍵が有効かは見ない） | 環境変数を export するか Keychain に登録する |
| `llm-online` | プロバイダが API キーを受理した（`--online` 時のみ） | 401 / 403 ならプロバイダで鍵を再発行し `[llm].api_key_ref` を更新する。到達不能や 5xx は警告止まり |
| `worktrees` | 孤児 worktree が無い | 対話的に掃除を提案する |
| `panes` | 孤児 pane が無い | 対話的に解放を提案する |

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
