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
| dispatch（F-31） | `inspect`: タブタイトル `totsuka <source_task_id>`、端末の worktree = タスクの worktree |
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
bash .claude/skills/live-e2e-orca/scripts/orca.sh sessions
```

| 検証点 | 期待 |
|---|---|
| タブが閉じる | `sessions` にそのタスクの行が無い |
| **worktree は orca から消されない** | `$E2E_HOME/wt/…` が残る（消すのは Orchestrator の cleanup 方針）。`orca worktree rm` は呼ばれない |
| 冪等 | もう一度 cancel してもエラーにならない |

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

> Orca のサイドバーで、`totsuka-sandbox-web` プロジェクトの下に `totsuka-sandbox-web: <issue のタイトル>` という worktree が出ていますか？ 選ぶと `totsuka <source_task_id>` というタブに Claude の画面が出ていますか？

| 検証点 | 区分 |
|---|---|
| プロジェクト配下に worktree が出る | 👀（`externalWorktreeVisibility = show` が前提） |
| 表示名が `{repo}: {title}` | 👀 |
| タブ名が `totsuka <source_task_id>` のまま（Claude の `✳ Claude Code` に上書きされていない） | 👀 |
| Claude の対話画面が見え、操作できる | 👀 |
| `[orca.layout] shell = true` のときの分割 | 👀 設定したときだけ |

---

## まだ実機で通していないもの

**このスキルの作成時点（2026-09-19）で、Orchestrator（`tt run`）を通した回は O1〜O6 のどれも未実施。**
実機で確かめてあるのは次の 2 つだけで、どちらも Orchestrator を介していない:

- プラグインのバイナリを stdio で直接駆動した dispatch → プロンプト到達 → list → snapshot → release →
  deadman → attach（ADR-0081 の検証）
- 同じ手順で開いた端末が GUI のサイドバー（プロジェクト配下）に出て、Claude の画面が見えること（O6 相当）

通したシナリオはここから消し、[agent-ide-orca](../../../../ai-docs/components/agent-ide-orca.md) の
`verified` を更新する。
