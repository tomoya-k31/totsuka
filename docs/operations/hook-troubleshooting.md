---
type: Playbook
title: フック完了判定のトラブルシューティング
description: Claude Code フック方式の運用手引き。スプールバックログ（doctor hook-spool チェックでの検出・drain/確認・corrupt 隔離ファイル）、Escalated タスクの対応手順（pane スナップショット確認・herdr pane での解消・次 Stop での自然復帰・fail アウト）、human 検収での totsuka task verify --pass/--fail 操作を、doctor のフックプローブ参照つきで整理する。
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/orchestrator-cli
tags: [operations, playbook, hook, claude-code, spool, escalation, verify, doctor, epic-131]
generated: { by: human:tomoya-k31, at: 2026-07-23T13:00:00Z }
status: stable
owner: tomoya-k31
---

# 対象

Claude Code のフック完了判定（[F-100〜F-107](/product/orchestrator-spec.ja.md)、フロー: [hook-signal-flow](/architecture/hook-signal-flow.md)）を使う運用でのトラブル対応。前提知識は [ADR-0004](/decisions/adr-0004-hook-completion-signal.md) と、セキュリティ面は [hook-security](/security/hook-security.md)。日常運用の総合手引きは [運用ガイド](/operations/operations-guide.md)。

まず疑ったら `totsuka doctor`（`--json` 可）。フック系プローブが一次切り分けになる（[orchestrator-cli](/components/orchestrator-cli.md)）。プローブ名は `doctor --json` の `.name` に出るチェック名で表記する:

| プローブ（チェック名） | 見るもの | 失敗が示すこと |
|---|---|---|
| `hook-socket` | UDS への自己 POST が 200 か | 受信サーバ不達・Bearer/権限不整合 |
| `hooks` | スクリプト + `orchestrator-*.json` の存在・0700/0600・内容ハッシュ | アセット未生成・パーミッションドリフト・改ざん |
| `hook-token` | `[hooks].auth_token_ref` が解決するか | keychain/env 参照切れ |
| `hook-deps` | `curl` / `jq` の存在（H-14） | Stop 等の送信系フックは生 JSON を spool へ退避。`on-user-prompt-submit.sh` は無出力で縮退（そのターンの不可視コンテキスト注入が失われ、マーカー欠落は `on-stop.sh` の block が是正） |
| `hook-spool` | `spool_dir` の書込可否とバックログ件数（>0 は warning） | POST 失敗が継続・回収が回っていない |

`hooks` / `hook-token` の失敗は多くが `totsuka run` または `totsuka doctor` の再実行で自己修復する（アセットは内容ハッシュ冪等で正本へ収束、N-02）。

# 1. スプールバックログ

## 何が起きているか

`on-stop.sh` は UDS への POST が失敗すると（2 回リトライ後）、送信予定の JSON を NDJSON 1 行として `[hooks].spool_dir`（既定 `${XDG_STATE_HOME}/totsuka/hooks/spool`）へ退避する（E-07）。Engine の `replay_spool()` が recover 直後と各サイクルで drain・再投入し、成功したファイルを削除する。**バックログが減らない = POST が継続失敗しているか、run が回っていない**。

## 検出

```bash
totsuka doctor --json | jq '.[] | select(.name=="hook-spool")'
# もしくは直接
ls -l "${XDG_STATE_HOME:-$HOME/.local/state}/totsuka/hooks/spool"
```

`hook-spool` のバックログ > 0 は warning（致命ではない）。冪等 UNIQUE 制約（D-05）があるため、滞留していても再投入で重複は無害に落ちる — 問題は「なぜ POST が失敗したか」。

## 切り分けと対処

1. **run が動いているか**: `totsuka status` で orchestrator 生存を確認。停止中なら `totsuka run`（または `--watch`）で回収が走る。
2. **依存欠落**: `hook-deps` が赤なら `curl` / `jq` を入れる。無いと送信系フック（Stop 等）は生 JSON を spool へ退避し、`on-user-prompt-submit.sh` は無出力で縮退する（不可視コンテキスト注入がそのターンだけ失われる）。
3. **受信不達**: `hook-socket` が赤なら socket パス・Bearer・0600 権限を確認（[hook-security](/security/hook-security.md)）。`hook-token` も併せて見る。
4. **中身を見たいとき**（1 行 1 JSON）:

   ```bash
   tail -n 5 "${XDG_STATE_HOME:-$HOME/.local/state}/totsuka/hooks/spool"/*.jsonl | jq .
   ```

   `last_assistant_message` を含み得るため機微扱い（N-05）。

## corrupt 隔離ファイル

parse 不能行を含むスプールファイルは**削除されず** `<name>.corrupt` へ隔離リネームされる（部分書き込み・壊れた 1 行で全体を失わないため）。`.corrupt` は自動回収・自動削除の対象外。

- 中身を確認して、失われて困る完了シグナルが無いか見る（多くは途中書き込みの残骸）。
- 有効な行があれば手で該当 job の状況を確認（`totsuka task show <id>`）。冪等なので、必要なら該当行だけ正しい `.jsonl` として置き直せば次サイクルで再投入される。
- 機微が残るため、確認後は手動削除する（放置しない、N-05）。

# 2. Escalated タスクの対応

## いつ Escalated になるか

タスクは次のいずれかで `Escalated`（**非終端**）へ遷移し、notifier 通知（🚨 エスカレーション）と `diagnostics/snapshot` を伴う（[notifier-macos](/components/notifier-macos.md)）:

- **UNKNOWN 連続 ≥ `block_retry_limit`（既定 3）**: マーカー無し完了が続いた（DB から再計算・フック自己申告は不使用, D-02）。
- **タイムアウト**: 最後のシグナルから `workflow.timeout_secs`（既定 1800 秒）無音（`sweep_signal_timeouts`, D-03）。
- **相関の異常**。

`Escalated` は人間対応待ちで**スロットを解放**する（F-45）。pane は診断のため保持される（F-107）。

## 手順

1. **状況把握**: `totsuka task show <id>` でイベント履歴を見る。Escalate 時に記録された `diagnostics/snapshot`（herdr `pane.read` の画面テキスト、R-10）が `events.detail` に入っているので、pane で何が起きていたか（承認待ち・ループ・クラッシュ手前など）を確認する。
2. **pane で直接解消**: エージェントは herdr の pane に生きている（保持される）。pane に attach し、詰まりを人手で解く（質問に答える・指示を出し直す・許可を与える等）。
3. **自然復帰**: pane 側で作業が進み次の `Stop` フックが正常なマーカー付きで発火すれば、Engine は次シグナルで `Escalated` から `Verifying`/`Publishing`/`WaitingInput`/`Running` へ**自然復帰**する（Escalated は全非終端から到達し、そこから復帰できる設計）。特別なコマンドは不要。
4. **回復しない/見切る場合**: これ以上進めないなら `totsuka task cancel <id>`（→ 次 run でセッション/スロット解放）。原因が明確な失敗なら、pane を潰さず調査してから cancel する（Failed/Escalated pane は保持されるので後追い調査可）。
5. **タイムアウトの頻発**: 正常でも時間のかかる workflow なら、その `[[workflows]]` の `timeout_secs` を延ばす（既定 1800）。UNKNOWN 連発なら `block_retry_limit` ではなく**マーカー未出力の根本**（rubric・指示文・`orchestrator-<workflow>.json`）を疑う。

# 3. human 検収（totsuka task verify）

`verification = "human"` の workflow は、完了自己申告（COMPLETED）で publish せず `Verifying` に留まり、notifier 通知（🔍 検収待ち）を出してスロットを保持する（出力未確定のため, F-45/D-01）。成果物は再起動後の検収に備えて永続化される。

- 成果物を確認して問題なければ:

  ```bash
  totsuka task verify <id> --pass
  ```

  `ApproveVerification`（`verifying` 状態のみ受付）→ 次 `run` の recover で `finalize_success`（出力ポリシー実行）。

- やり直させる場合:

  ```bash
  totsuka task verify <id> --fail --reason "<修正指示>"
  ```

  `VerificationFailed` → `Running`（D-07）。

`verifying` 以外の状態に対する `verify` はエラー（原因 + 次のアクションで表示）。どの workflow が human 検収かは設定次第（例: 詳細設計 = human、Slack メンション/実装 = llm）。llm 検収はセッション内 prompt 型 Stop フックが自動判定するため `task verify` 操作は不要。

# 4. plan 系 profile のタスクで編集やコマンドが拒否される

`profile = "answer"` / `"triage"` / `"design"` の claude タスクで、エージェントが `Edit` / `Write` を使おうとして拒否される、あるいは `git switch -c` / `gh pr create` が通らない。

**これは正常動作である。** これらの profile には Rust 固定の `permissions.deny` が入っており（#395、[ADR-0033](/decisions/adr-0033-workflow-profile.md) D4）、対象リポジトリの `.claude/settings.json` の allow より強い（deny はスコープ横断でマージされる）。

**ただし deny は read-only の保証ではない。** [#410](https://github.com/tomoya-k31/totsuka/issues/410) の実機検証で、ルールが全部発火した状態のまま `answer` タスクがブランチ・commit・push・PR まで到達した — `Bash` 経由のファイル書き込み（`cat >` 等）と、`&&` / パイプによる前方一致の回避が塞げていない。**「deny があるから read-only」と考えないこと。**

| 症状 | 判断 |
|---|---|
| `answer` タスクが `Edit` を拒否される | 正常。回答は返信文であって編集ではない |
| `answer` タスクが `gh issue comment` を拒否される | 正常。返信はプラグインの承認ゲートを通って本人名義で出る |
| `design` タスクが `gh issue comment` を拒否される | **異常**。design は issue コメントで成果物を書く profile なので、deny セットの不具合か profile の指定違い |
| `design` タスクが `gh pr create` を拒否される | 正常。PR を出すのは `implement` |
| **実装させたいのに拒否される** | profile の選択ミス。`profile = "implement"` にする。Slack 起点なら、本人のリアクションで別タスクとして起こす（#393 D6） |

確認するには rendered settings を見る:

```bash
jq '.permissions.deny' "${XDG_DATA_HOME:-$HOME/.local/share}/totsuka/hooks/orchestrator-<workflow>.json"
```

キー自体が無ければ `implement` profile か、profile を使わない明示記法（`mode` / `output` を直接書く形）である。**明示記法には deny が付かない** — `mode` は元々何も強制しておらず、そこから権限境界を推測すると既存の構成がアップグレードで黙って厳しくなるため。強制が欲しければ profile 記法へ移行する。

デプロイ中に deny セットが変わった場合、`run` / `doctor` の起動時に内容ハッシュ差分で自動的に書き換わる。**走行中のセッションにも反映される**（Claude Code は settings ファイルの変更を実行中セッションに取り込む）が、変化は常に「制限が強まる」方向なので安全側に倒れる。

# 関連

- [フックシグナルフロー](/architecture/hook-signal-flow.md)
- [ADR-0004 フック完了シグナルの受信配置](/decisions/adr-0004-hook-completion-signal.md)
- [Claude Code フック機構のセキュリティポリシー](/security/hook-security.md)
- [orchestrator-cli](/components/orchestrator-cli.md) / [notifier-macos](/components/notifier-macos.md)
- [運用ガイド（doctor / worktree 掃除 / FAQ）](/operations/operations-guide.md)
