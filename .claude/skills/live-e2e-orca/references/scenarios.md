# テストパターン一覧（orca 版）

各シナリオの「起動 → 観測 → 判定」と、自動／手動／目視の内訳。**順序は上から**。
GitHub / Slack の駆動は herdr 版のスクリプト（`.claude/skills/live-e2e-herdr/scripts/`）を
そのまま使う。herdr 版の [scenarios.md](../../live-e2e-herdr/references/scenarios.md) の S1〜S4 の
**手順の細部（issue を毎回作る理由・基準時刻・レート）はそちらが正**で、ここでは orca で見る点だけを足す。

凡例: 🤖 自動 / 🙋 手動（人間に依頼） / 👀 目視（人間が画面で判定）

前提: `orca.sh preflight` が `preflight: OK`、`tt run --watch` が `agent = "orca"` の設定で動いている。

---

## O1. GitHub / implement 通し 🤖

**人間の関与ゼロ。** 最初にこれを通す。herdr 版 S1 と同じ手順でタスクを起こす:

```bash
url=$(gh issue create --repo "$E2E_GH_OWNER/$E2E_GH_REPO_WEB" \
        --title "feat: … 関数を追加する（orca 検収）" --body "<仕様と完了条件。push と PR 作成を含める>")
n="${url##*/}"
mkdir -p "$E2E_HOME/state/live-e2e"
date -u +%Y-%m-%dT%H:%M:%SZ > "$E2E_HOME/state/live-e2e/seed-$E2E_GH_REPO_WEB-$n"
iid=$(gh project item-add "$E2E_GH_PROJECT" --owner "$E2E_GH_OWNER" --url "$url" --format json --jq .id)
bash .claude/skills/live-e2e-herdr/scripts/github.sh prime-item web "$n" "$iid"
bash .claude/skills/live-e2e-herdr/scripts/github.sh seed web "$n"
```

**dispatch された直後に**（`tt task list` で `running` になったら）端末を判定する。完了後だと
worktree 掃除（`cleanup = "immediate"`）でタブも worktree も消え、見るものが無くなる:

```bash
id=$(tt task list --json | python3 -c 'import json,sys; print(max(t["id"] for t in json.load(sys.stdin)))')
bash .claude/skills/live-e2e-orca/scripts/orca.sh inspect "$id"
bash .claude/skills/live-e2e-orca/scripts/orca.sh snapshot "$id"   # プロンプトが 1 ターンとして届いているか
```

それから完了まで待つ:

```bash
bash .claude/skills/live-e2e-herdr/scripts/github.sh wait   web "$n"
bash .claude/skills/live-e2e-herdr/scripts/github.sh verify web "$n"
```

| 検証点 | 見るもの |
|---|---|
| dispatch（F-31） | `inspect`: 端末の worktree = タスクの worktree、その worktree の orca comment が `totsuka <source_task_id>`（所有マーカー） |
| **worktree が 1 本であること** | `inspect`: orca 自身が作った `totsuka-*` worktree が無い（旧実装の `worktree create` の再発検知） |
| サイドバーの表示名 | `inspect`: worktree の `displayName` が `{repo}: {title}` |
| **プロンプトの到達** | `snapshot`: 画面の `❯` の後にタスク本文が**丸ごと 1 ターン**で出ている。空の `❯` のままなら届いていない（troubleshooting 参照） |
| **hook による完了（F-100〜）** | `tt task show <id>` のイベントに Stop hook 由来の遷移があり、`running → publishing → done` |
| F-86 / F-84 | `verify`（herdr 版と同じ） |
| 掃除でタブが閉じる（`session/release`） | 完了後 `orca.sh sessions` にそのタブが無い |

## O2. pane control 🤖👀

O1 と同じ要領で**長めのタスク**（完了条件を増やす）を 1 本起こし、`running` の間に:

```bash
source .env && tt focus "$id"          # 👀 Orca がそのタスクのタブへ切り替わるか（人間に見てもらう）
source .env && tt doctor               # panes チェックが orca の端末を所有 pane として数え、孤児を出さない
```

| 検証点 | 区分 |
|---|---|
| `tt focus`（F-94）→ `terminal switch` | 👀 Orca の表示がそのタブに移る |
| `tt doctor` の `panes` | 🤖 fail しない（稼働中タスクの端末は孤児ではない） |
| 孤児の検出 | 🤖 タスクを `tt task cancel` せずに state DB 側だけ終わらせる手段は無いので、**手で開いた** `orca terminal create --worktree path:<task の worktree> --title "totsuka X-1"` を置いて `tt doctor` が孤児として挙げること（確認後 `--tab` で閉じる） |

## O3. deadman（エージェントの異常終了） 🤖

```bash
bash .claude/skills/live-e2e-orca/scripts/orca.sh exit-agent "$id"
source .env && tt task show "$id"
```

| 検証点 | 期待 |
|---|---|
| state stream の deadman（F-38） | `running → failed`。`log_chunk` に `the agent's terminal exited (orca reports exit code …)` |
| 終了コード | **見ない。** orca の `exitCode` は実際の値を反映しない（ADR-0081 D-5）。0 でも `failed` が正しい |
| worktree | 残る（`task retry` 用、F-44） |

**完了済みのタスクには意味が無い** — Orchestrator は終わったタスクへの `failed` を無視する。
`running` のうちに打つこと。

## O4. cancel 🤖

```bash
source .env && tt task cancel "$id"
sleep 90   # 次の sweep（既定 60s 周期）を待つ
bash .claude/skills/live-e2e-orca/scripts/orca.sh sessions
source .env && tt logs --task "$id" | tail -5
```

| 検証点 | 期待 |
|---|---|
| CLI の cancel | タスクが `cancelled` になる。**端末はその場では閉じない**（`the pane is not closed here — totsuka doctor lists it`。herdr でも同じ） |
| sweep による解放 | 次の sweep でブランチを記録 → `pane released`（`session/release`）→ `worktree cleanup outcome="Removed"`。`sessions` からそのタスクが消える |
| **worktree は orca から消されない** | 消すのは Orchestrator の cleanup（`orca worktree rm` は呼ばれない） |
| 2 回目の cancel | `tt task cancel` は**エラーを返すのが正常**（`task N is already cancelled → nothing to cancel`） |
| `tt focus`（解放後） | `pane not focused — the pane is already closed`。解放済みなので正しい |

実測（2026-09-19, task 13）: cancel 02:57:21 → sweep の解放 02:58:06。

## O5. Slack / メンション経路と会話の継続 🙋👀

herdr 版 S3 をそのまま回す（文面・承認の依頼の仕方もそちら）。orca で足す検証点:

| 検証点 | 区分 |
|---|---|
| plan モードの起動 | 🤖 `snapshot`: plan フラグ付きで起動している（`tool_launch` の argv がそのまま効く） |
| 下書き → 承認 → 返信 | 🙋🤖 herdr 版 S3 と同じ |
| **追いメンションでの resume** | 🙋 同じスレッドに追いメンションしてもらう → 🤖 新しいタブが同じ worktree に開き、`snapshot` で前の会話を覚えている |
| resume できない場合の縮退 | 🤖 `SESSION_UNRESUMABLE` の後、resume 無しで 1 回だけ再 dispatch される（ログ） |

## O6. GUI の見え方 👀

人間に見てもらう。**CLI の一覧は GUI と見え方が違う**ので、これは CLI では代替できない:

> Orca のサイドバーで、`totsuka-sandbox-web` プロジェクトの下に `totsuka-sandbox-web: <issue のタイトル>` という worktree が出ていますか？ 選ぶと Claude のタブ（タイトルは Claude が付けたもの）に対話画面が出ていますか？

| 検証点 | 区分 |
|---|---|
| プロジェクト配下に worktree が出る | 👀（`externalWorktreeVisibility = show` が前提） |
| 表示名が `{repo}: {title}` | 👀 |
| worktree のメモ（comment）に `totsuka <source_task_id>` が出ている | 👀 GUI での見え方の確認。判定は `inspect` が CLI で行う |
| Claude の対話画面が見え、操作できる | 👀 |
| `[orca.layout] shell = true` のときの分割 | 👀 設定したときだけ |

---

## 実機での実施状況

2026-09-19、orca 1.4.205 + Claude Code 2.1.277、Orchestrator（`tt run`）を通して実施:

| シナリオ | 結果 | 見つかった不具合（修正済み） |
|---|---|---|
| O1 | 合格（task 8・10。PR・Status 書き戻し・hook 完了・タブ解放・worktree 削除） | deadman が `terminal wait` の誤報（`terminal_handle_stale`）で動いているタスクを `failed` にした → `terminal show` で裏を取るようにした |
| O2 `inspect` | 合格（task 12） | 所有マーカー（タブタイトル）が Claude に上書きされた → worktree の comment に移した |
| O2 `tt focus` | 解放済みの端末に「既に閉じている」と正しく答えることのみ確認 | — |
| O3 | 合格（task 12。SIGTERM から約 3 秒で `failed`、worktree は残る） | `/exit` の文字送信が完了確認の質問への回答になった → `exit-agent` を SIGTERM に変えた |
| O4 | 合格（task 13。cancel 後の sweep がタブを解放） | 期待値の誤り（CLI の cancel は端末を閉じない）→ シナリオを訂正 |
| O5 | 合格（task 14。メンション → 下書き → 承認 → 返信、追いメンション 2 回での resume） | 作り直した worktree を orca が約 10 秒認識せず dispatch が 2 回失敗 → 認識を待つようにした（再実施で 1 回目から通過）／解放済み端末の空パスを別端末と誤認 → 不明として扱うようにした |

**まだ通していないもの**: 生きている端末への `tt focus`、`tt doctor` の pane チェック（孤児の検出を含む）、O6（repo の
`externalWorktreeVisibility` が `hide` のままだったため）。通したらこの表へ移す。
