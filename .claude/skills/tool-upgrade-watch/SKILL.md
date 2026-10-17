---
name: tool-upgrade-watch
description: "Claude Code / Codex CLI / opencode の新しい版で追加された機能を調べ、totsuka がそのツールの機能不足を肩代わりしているロジック（hooks の合成・グローバル登録・ステータスマーカー・--standalone 等）を置き換えられるようになったかを判定し、置き換え候補を GitHub issue に起票する。トリガー: 「ツールの新機能で置き換えられる所をチェック」「Claude Code / codex / opencode のアップデートを確認して」「ワークアラウンドを消せるようになったか見て」「tool-upgrade-watch」、および claude / codex / opencode の版を上げたとき・リリースノートを見て totsuka への影響を知りたいとき。単に changelog を要約してほしい依頼でも、totsuka への影響を問うならこのスキルを使う。"
metadata:
  internal: true
---

# ツール更新による置き換え候補チェック

totsuka は Claude Code / Codex / opencode の機能不足を自前のロジックで埋めている（例: Codex にはプロセス単位の
hooks 指定が無いので `~/.codex/hooks.json` に差し込む、opencode の共有サーバが pane の env を見られないので
`--standalone` を強制する）。ツール側に機能が入れば、そのロジックは消せる。このスキルは 1 回の実行で
「今の最新版で消せるようになったものはあるか」を判定し、あれば issue にする。

- 台帳（何を・どの機能で置き換えられるか）: [references/replaceable-logic.md](references/replaceable-logic.md)
- 取得元とその癖: [references/sources.md](references/sources.md)

**結果の扱い**: 置き換え候補があれば会話にレポートを出し、`gh` で issue を起票する。候補が無ければ
短く「候補なし」と伝えて終わる — ファイルも issue も作らない。このスキルはコードも台帳も**書き換えない**
（台帳の不整合は報告だけする）。

## 1. 取得する

```bash
out="$(mktemp -d "${TMPDIR:-/tmp}/tool-upgrade-watch.XXXXXX")"   # セッションに scratchpad があればその下に作る
bash .claude/skills/tool-upgrade-watch/scripts/fetch-sources.sh "$out"
bash .claude/skills/tool-upgrade-watch/scripts/check-anchors.sh
```

- 作業ディレクトリは**実行ごとに新しく作る**。使い回すと、前回取った別系列の docs（opencode の v1 / v2 など）が
  残って判定に混ざる。
- `fetch-sources.sh` は各ツールの版（`versions.txt`）・changelog・現行 docs を `$out/<tool>/` に落とす。
  台帳の「読む docs」に無いページが要りそうなら、各ツールの `llms.txt` からページを探して追加で取ってよい。
  取れなかった URL は `$out/FAILED.txt` に出る。欠けたソースに依存する項目は「判定不能」とし、推測で埋めない。
- `check-anchors.sh` は台帳の各項目がまだコードに存在するかを確かめる。`STALE` の項目は既にコードが変わって
  いるので判定から外し、レポートの「台帳の保守」に載せる。

## 2. 項目ごとに判定する

台帳の各項目（`### <id>`）について、その「置き換え条件」が**現行 docs で満たされているか**を見る。

判定の根拠を docs に置くのは、changelog の箇条書きが自由文で機能名も docs と一致しないうえ、
「fix」と「新機能」の区別も曖昧だから。changelog は「どの版から使えるか」を引く索引として使う。

1. 項目の検索語で `$out/<tool>/docs-*` を `grep -n -i -E` し、当たった行の前後を読む。ファイルは大きいので
   丸ごと読まない（Claude の `docs-hooks.md` は約 270 KB）。
2. 機能が見つかったら、置き換え条件の**要件ひとつひとつ**を docs の記述と突き合わせる。名前が似ているだけで
   スコープや入力が違うことが多い — 例: Codex の `<repo>/.codex/hooks.json` はプロジェクト層であって
   起動単位ではないし、Claude の `PermissionDenied` は auto mode の拒否だけで承認は含まない。
3. 同じ検索語で changelog（Claude: `changelog.md`、Codex: `changelog.md` + `unreleased.md`、
   opencode: `changelog-v1.md` / `changelog.html`）を引き、導入された版を特定する。読む範囲は台帳の
   「基準版」から最新まで。changelog で特定できなければ、ツールの公開リポジトリのソース（例: Codex の
   `codex-rs/`）を基準版と最新版で見比べてよい。それでも不明なら「導入版は不明（現行 docs には記載あり）」と書く。
   基準版の時点で既にあった機能なら、新機能ではなく「台帳の前提が古かった」と書く。
4. 現状のコードをアンカーの行から読み、置き換えたときに何が消えるかを台帳の「置き換え後」と照らして確かめる。

判定は次の 4 つのどれか:

| 判定 | 意味 | 起票 |
|---|---|---|
| **置き換え可能** | 置き換え条件の要件がすべて docs で確認できた | する |
| **部分的** | 一部の要件だけ満たす、または docs に名前はあるが挙動（発火範囲・スコープ）が書かれていない | する（足りない要件と実機で確かめる点を明記） |
| **未対応** | 該当する機能が無い | しない |
| **判定不能** | ソースの取得に失敗した、または STALE | しない |

例外:

- `種別: policy` の項目は判定しても起票しない。前提が変わっていればレポートに 1 行書くだけ（ADR で
  意図して選んだ構成なので、変えるかは人間が決める）。
- `marker-protocol` は 3 ツールすべてで揃ったときだけ起票する（ADR-0020）。揃っていない間は「n/3」と書く。
- `no-prompt-hook` / `no-heartbeat` のように複数ツールにまたがる項目は、ツールごとに判定し、起票もツールごとに分ける。

## 3. 重複を確かめて起票する

候補（置き換え可能・部分的）ごとに、既存の issue を確認する。重複判定のキーは本文先頭の機械可読な印で、
タイトルではない（人がタイトルを直しても同一性が壊れないように）。複数ツールにまたがる項目はキーにツールを含める。

```bash
gh label create tool-upgrade --color 1d76db --force \
  --description "AI ツールの新機能で totsuka のワークアラウンドを置き換える候補（tool-upgrade-watch が起票）"
marker='<!-- tool-upgrade-watch: item=<id> tool=<claude|codex|opencode> -->'
existing="$(set -o pipefail
  gh issue list --state all --label tool-upgrade --limit 200 --json number,state,body \
    | jq -r --arg m "$marker" 'map(select(.body // "" | contains($m))) | .[0] // empty | "\(.number) \(.state)"')" \
  || { echo "issue 一覧の取得に失敗 — 起票を中止する"; exit 1; }
```

**一覧の取得が失敗したら起票しない。** `pipefail` が無いと、`gh` が落ちても空入力の `jq` が終了コード 0 で
何も出さず、「既存なし」と区別できずに重複起票する（`herdr-schema-watch.yml` が同じ罠を書き残している）。

`gh issue list --search` は使わない。タイトルの `refactor(hooks):` の括弧を GitHub 検索がグルーピング演算子と
解釈して、エラーにならずに 0 件を返す（`herdr-schema-watch.yml` で実際に重複起票した）。

- **既存が無い** → 起票する。
- **既存が open** → 起票しない。判定が「部分的」から「置き換え可能」に上がったときだけ、その issue に
  根拠を `gh issue comment` で足す。
- **既存が closed** → 起票しない（人が見送りか対応済みと判断した）。レポートに番号だけ書く。

起票は次の形で行う。**本文は日本語**。

```bash
gh issue create --label enhancement --label tool-upgrade \
  --title "refactor(<scope>): <ツール> <版> の <機能> で <今のワークアラウンド> を置き換える" \
  --body-file "$out/issue-<id>-<tool>.md"
```

- `<scope>` は主に変わる場所（`hooks` / `core` / `herdr` / `orca` など。既存の issue タイトルに合わせる）。
- `<版>` は導入版。不明なら「最新版」と書く。

本文のテンプレート:

```markdown
<!-- tool-upgrade-watch: item=<id> tool=<tool> -->

**判定: 置き換え可能 | 部分的**（tool-upgrade-watch、<YYYY-MM-DD> 時点の <ツール> <最新版>）

## 何が使えるようになったか

<機能の説明。docs からの短い引用と URL。導入版（changelog の該当行）>

## 置き換えられる totsuka のロジック

- `<path>:<line>` — <今のワークアラウンド>（ADR-xxxx）
- 置き換え後: <消える / 縮むもの>

## 足りない点・実機で確かめること

<部分的なら満たしていない要件。可能でも docs に書かれていない挙動。実機検証が要るなら live-e2e-herdr / live-e2e-orca>

## 進めるときの注意

- 置き換えると、そのツールの最低対応版が <版> に上がる（totsuka はツールの版を検査していない）。
- 設計判断を変えるので ADR の更新が要る（ai-docs/CLAUDE.md の更新義務）。
```

## 4. レポート

起票の有無にかかわらず、最後に会話へ次の形で出す。候補が 1 つも無いときは、表を省いて
「候補なし（確認した版: …）」の 1 行と、policy 項目の前提変化・`marker-protocol` の「n/3」・STALE・
取得失敗のうち該当するものだけにする。

```markdown
## ツール更新チェック（YYYY-MM-DD）

| ツール | ローカル | 最新 |
|---|---|---|

### 置き換え候補
| id | ツール | 判定 | 根拠（docs / 導入版） | issue |
|---|---|---|---|---|

### policy 項目で前提が変わったもの
### 判定不能（取得失敗 / STALE）
### 台帳の保守（STALE アンカー、台帳に無い新機能で気になったもの）
```

未対応の項目は表に並べない（毎回同じ行が並ぶだけで、読み手が変化を見落とす）。
