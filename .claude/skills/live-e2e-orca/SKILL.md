---
name: live-e2e-orca
description: totsuka を実機（実 Slack / 実 GitHub / 実 orca + 実 Claude Code）で通しで動かし、結果を検証する手順（orca 版）。トリガー: 「orca で実機検証」「orca で e2e を回して」「orca で動かして」、および agent-ide-orca に手を入れた PR の検証、orca の版を上げたとき。CI は fake orca CLI までしか検証しないので、orca プラグインの実接続に関わる変更を出したら必ずこのスキルで確かめること。GitHub / Slack の駆動と $E2E_HOME は live-e2e-herdr と共用する。
---

# 実機 E2E 検証（orca 版）

herdr 版 [live-e2e-herdr](../live-e2e-herdr/SKILL.md) の**エージェントだけを orca に差し替えた**もの。
task source 側（GitHub / Slack）の経路はエージェントに依存しないので、その駆動・観測スクリプトと
`$E2E_HOME`（`~/.totsuka-e2e`）、`.env`、サンドボックス repo、ProjectsV2 は**すべて herdr 版のものを使う**。
このスキルが持つのは orca 固有の部分 —— 準備・シナリオ・観測（`scripts/orca.sh`）・症状表 —— だけである。

プラグインの契約は [ADR-0082](../../../ai-docs/decisions/adr-0082-orca-herdr-parity.md)、orca 側の実測事実は
[orca CLI 制御サーフェス](../../../ai-docs/references/orca-cli-control.md) の実測節にある。**症状の読み方は
そこから来ている**ので、詰まったら先に [references/troubleshooting.md](references/troubleshooting.md) を見る。

自動／手動／目視の区分は herdr 版と同じ:

| 区分 | 誰がやるか |
|---|---|
| **自動** | エージェント（`scripts/`）が API と CLI で実行・観測する |
| **手動** | 人間にしかできない操作。**依頼して待つ** |
| **目視** | API から読めないので人間が画面で確認する。**判定を人間に返す** |

## 0. herdr 版の準備が済んでいること

`$E2E_HOME` が無ければ herdr 版の [初回セットアップ](../live-e2e-herdr/references/bootstrap.md) から。
orca 版はその上に積む。

## 1. orca 固有の準備

まず現状を読むだけの検査を回す:

```bash
source .env && bash .claude/skills/live-e2e-orca/scripts/orca.sh preflight
```

`FAIL` の行ごとに直し方が出る。検査は 5 種類ある:

| 検査 | 直し方 | 誰が |
|---|---|---|
| orca ランタイムに届く | Orca アプリを起動（`orca open`） | 手動 |
| サンドボックス repo 2 つが orca に登録済み | `orca repo add --path "$E2E_HOME/repo/totsuka-sandbox-web"`（cli も） | **承認を取ってから自動** — Orca のサイドバーにプロジェクトが 2 つ増える |
| そのリポジトリ設定で external worktree が表示 | Orca の repo 設定で表示にする（`externalWorktreeVisibility = show`） | 手動（GUI） |
| orca プラグインがソースより新しくインストール済み | `tt plugin install --from-source --yes orca` | 自動 |
| E2E 設定のワークフローが `agent = "orca"` | `bash .claude/skills/live-e2e-orca/scripts/orca.sh use orca` | 自動（反映は下の再起動） |

**repo 登録が要る理由。** プラグインは worktree を作らず、Orchestrator が切った worktree に
`orca terminal create --worktree path:…` で端末を開く。orca が `path:` を解決できるのは
**登録済みリポジトリの git worktree だけ**で、未登録だと dispatch は
`orca does not know the worktree … → register its repository` で失敗する。

**`use orca` は隔離環境の設定だけを書き換える**（バックアップ `config.toml.bak.<時刻>` を残す）。
herdr に戻すときは `use herdr`。`[orca]` テーブルは書かなくてよい（既定値で動く）。

## 1.5. 【必須】プラグインを入れ直す

herdr 版の 0.5 節と同じ理由で、**`cargo build` は効かない**。`tt run` が起動するのは
`$E2E_HOME/data/totsuka/plugins/orca/orca` のコピーである。`preflight` がソースの mtime と比べて
古ければ `FAIL` にするので、直したら必ず入れ直して `preflight` を通し直す。

> `target/{profile}/orca` は外部の `orca` CLI と同名。**`target/` を PATH に入れない**こと —
> プラグインが自分自身を `orca` として起動する。

## 2. 【手動】常駐プロセスを（再）起動してもらう

herdr 版の 1 節と同じ（`op read` のため本人のターミナルからしか起動できない）。
`use orca` で設定を変えた直後は**再起動が要る**:

> `tt run --watch` を動かしているターミナルで Ctrl-C し、`source .env && tt run --watch` を実行してください。起動したら教えてください。

ログに `hook receiver listening` が出ていること、別のターミナルで `source .env && tt doctor` の
`plugin:orca` が pass すること（orca プラグインの `config/validate` が orca ランタイムへの到達まで
確かめる）を見る。`panes` チェックが orca を対象に含むのは、orca が `pane_control` を宣言しているため。
**起動時に orca が `CONFIG_INVALID` で落ちたら**、`[orca]` に廃止キーが残っている（メッセージに
キー名と代替が出る）。

## 3. シナリオを回す

[references/scenarios.md](references/scenarios.md) に全パターンと自動／手動／目視の内訳がある。
順序は herdr 版と同じく「人間の関与が少ないものから」:

1. **O1 GitHub / implement** — 人間の関与ゼロ。herdr 版の `github.sh seed` → `wait` → `verify` に、
   `orca.sh inspect` を足す
2. **O2 pane control** — `tt focus` / `tt doctor` の pane チェック / worktree 掃除でのタブ解放
3. **O3 deadman** — `orca.sh exit-agent` でエージェントを終わらせ、`failed` になること
4. **O4 cancel** — `tt task cancel` でタブが閉じ、worktree は残る
5. **O5 Slack / メンション** — 人間が 1 回打ち、承認を 1 回押す。**会話の継続（resume）もここで見る**
6. **O6 GUI の見え方** — 目視。サイドバー・表示名・タブ・Claude の画面

各シナリオは「起動 → 観測 → 判定」。観測は `scripts/`（herdr 版の `github.sh` / `slack.sh` と
このスキルの `orca.sh`）に任せ、**手で `orca` や `gh` を組み立て直さない**。

## 4. 結果を報告する

```bash
bash .claude/skills/live-e2e-herdr/scripts/report.sh          # タスクの終端状態（エージェント非依存）
bash .claude/skills/live-e2e-orca/scripts/orca.sh sessions    # 残っている totsuka の orca 端末
```

報告は表で、**どの機能要件が通ったか**を書く（F-31 dispatch / F-37 attach / F-38 state stream /
F-94 focus / F-100〜 hook 完了 など）。目視項目は「未確認」で残し、何を見てほしいかを具体的に伝える
（どのプロジェクトの、どの worktree の、どのタブの、何を）。

## 手動が必要な操作

herdr 版の表（`tt run` の起動・Slack への投稿・承認ボタン・Slack アプリの操作）に加えて:

| 操作 | なぜ代行できないか |
|---|---|
| Orca アプリの起動 | GUI アプリ。CLI は起動済みのランタイムに話しかけるだけ |
| repo 設定の external worktree 表示 | 設定を変える CLI が無い（`orca repo show --json` で読めるだけ） |
| `orca repo add` | 代行はできるが、**人間の Orca に見えるプロジェクトが増える**ので必ず承認を取る |

## 目視でしか確認できないもの

| 項目 | なぜ API で読めないか |
|---|---|
| サイドバーのプロジェクト配下に worktree が出るか | CLI の一覧（`worktree list` の `--repo` 無し・`ps`）には出ない。**GUI とは見え方が違う** |
| タブを選んだときに Claude の対話画面が見えるか | 描画は GUI。`orca.sh snapshot` は同じ内容の文字列を返すが、見えることの証明にはならない |
| `tt focus` で Orca がそのタブへ切り替わるか | フォーカスは GUI の状態 |
| `[orca.layout] shell = true` の分割の見た目 | 同上 |

## 後始末

**既定は「残す」**（herdr 版と同じ）。片付けるときは:

```bash
bash .claude/skills/live-e2e-orca/scripts/orca.sh cleanup-hints
bash .claude/skills/live-e2e-herdr/scripts/report.sh --cleanup-hints
```

orca の端末は `orca terminal close --terminal <handle> --tab`。worktree は Orchestrator の cleanup
（`[worktree] cleanup`）か `tt doctor` に任せ、**`orca worktree rm` は使わない** —— worktree は
Orchestrator のもので、orca が消すと state DB と食い違う。

herdr に戻す: `bash .claude/skills/live-e2e-orca/scripts/orca.sh use herdr` → `tt run` 再起動。
orca の repo 登録は残しておいてよい（herdr の実行には影響しない）。
