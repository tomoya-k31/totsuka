---
type: Decision
title: ADR-0071 タスク識別子の命名 — 内部 task 番号を読める半分にし、手順を plugin-protocol に一本化する
description: "herdr の agent name・orca の worktree 名・Orchestrator の worktree ディレクトリ名を 1 つの規則に揃える決定。名前は <prefix><task 番号><sep><sha256(source ∥ source id) 先頭 8 hex> とし、制約（prefix・長さ上限・大小・許可文字）だけを各ツールが IdentifierPolicy で宣言して sanitize・切り詰め・ハッシュ付与の手順は plugin-protocol が持つ。読める半分を source id から内部 task 番号へ移すため protocol 0.7.1 で TaskDispatchParams.task_number を足し、job_id は使わない。session row を含めない理由、ハッシュを常に付ける理由、worktree の葉とプレースホルダの扱いを含む。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/crates/plugin-protocol/src/identifier.rs
tags: [decision, adr, naming, identifier, plugin-protocol, herdr, orca, worktree]
generated: { by: claude-code/opus-5, at: 2026-09-13T01:10:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

**採択（stable）。** [ADR-0032](/decisions/adr-0032-herdr-protocol-17.md) の D-2（`agent_name` の生成規則）を置き換える。D-3（`agent_name_taken` を孤児の徴候として扱い、別名で逃げない）は**前提ごと維持**する — D-1 はその前提を壊さないことを要件として選んである。

実装は 2 段階に分かれる（#645）。protocol 0.7.1・`identifier` モジュール・herdr の移行が 1 本目、orca と Orchestrator の worktree 名が 2 本目。本 ADR は両方の決定を記す。

# Context

## 何が起きたか

実機で Slack 発のタスクが dispatch に失敗し、自動リトライ 3 回を使い切って `failed` で確定した。

```text
ERROR orchestrator_core::run::dispatch: dispatch failed: plugin `herdr` method `task/dispatch`
  failed (-32603): herdr error (invalid_agent_name): agent name must start with a lowercase
  letter and contain only lowercase letters, digits, '-' or '_' (1-32 characters) task_id=3
WARN  dispatch failed; requeued automatically attempt=1 limit=3 task_id=3
```

原因は `agent_name` の予算計算である。`t-` (2) + 可読プレフィクス (≤21) + `-` (1) + ハッシュ 8 = 32 のつもりだったが、**区切りの `-` を長さチェックの後に足していた**。

```rust
if prefix.len() >= NAME_PREFIX_CHARS { break; }                   // len == 20 は通る
if c.is_ascii_alphanumeric() {
    if pending_dash && !prefix.is_empty() { prefix.push('-'); }  // 21
    prefix.push(c.to_ascii_lowercase());                          // 22 → 全体 33
```

英数字の連なりが**ちょうど 20 文字目で終わり、区切りを挟んで次の英数字が来る** id だけが 33 文字になる。ソース別に当たり方が違う:

| ソース | `source_task_id` | 33 になる条件 |
|---|---|---|
| Slack 無印（answer / mention） | `{channel}:{ts}` | **channel id が 9 文字（レガシー `C`+8）なら毎回**。実機で踏んだのはこれ |
| Slack `impl:` / `books:` | `{prefix}:{channel}:{ts}` | 無し |
| Discord | `{channel_id}:{message_id}` | 無し（snowflake の桁数では当たらない） |
| GitHub Projects | GraphQL node id（base64url なので `_` `-` が任意位置） | **確率的に有り**。特定 issue だけ何度でも落ちる |
| Notion | ハイフン付き UUID | 無し（区切りが 8/13/18/23 文字目） |

既存の単体テストはサンプル 3 つが偶然 32 以内に収まるものだったため素通りした。

## なぜ実装だけ直して終わりにしないのか

**同じ「名前を作る」処理が 3 つある。** 制約（何文字・どの文字）と手順（sanitize・
切り詰め・衝突回避）が混ざったまま、それぞれの場所に別々に書かれている。

| 場所 | 実装 | 制約 |
|---|---|---|
| herdr `agent_name` | `t-` + `sanitize(source id)[..21]` + hash8 | 32 文字・小文字・`[a-z][a-z0-9_-]` |
| orca `worktree_name` | `totsuka-` + sanitize(source id) | 長さ無制限・大文字と `_` は素通し・ハッシュ無し |
| core `render_worktree_name` | `{source}-{task_id}` を git ref 規則で正規化 | 長さ無制限 |

**残り 2 つは間違っていない。それがこの問題の本質である** — 3 つが互いを参照していないので、1 つの算術の誤りを他の 2 つは検出できない。4 つ目のツールを足す人は 4 つ目の sanitizer を書く。

**もう 1 つ、可読プレフィクスが機能していない。** ADR-0032 D-2 は「`herdr agent list` を人間が読んでどのタスクか当たりを付けられる」ことを可読半分の理由にしたが、実際の出力は Slack が `t-cxxxxxxxxxx-170000000-…`（ts が桁の途中で切れる）、GitHub が `t-i-kwdotrfap88aaaablko-…`（base64 の断片）、Notion が UUID、Discord が数字列である。**どのソースでも名前からタスクを特定できない。** 一方、ログ（`task_id=3`）・`totsuka status`・`totsuka task retry 3`・`TOTSUKA_JOB_ID=job-3-…` が使う内部の task 番号は名前のどこにも出ていない。

# Decision

## D-1: 名前は `<prefix><task 番号><sep><hash8>`、core を全ツールで一致させる

```text
core        = <task 番号>[-<handle>]-<hash8>   hash8 = sha256("<source>\0<source_task_id>") 先頭 8 hex
herdr       = t-3-web-42-9f3c2a1e              32 文字・[a-z][a-z0-9_-]
orca        = totsuka-3-web-42-9f3c2a1e
worktree 葉 = 3-web-42-9f3c2a1e                <state>/totsuka/worktrees/<repo_name>/3-web-42-9f3c2a1e
```

`handle` は任意で、D-7（0.7.2 / #646）で足した。初版は `<task 番号>-<hash8>` だけだった。

**読める半分は内部 task 番号**（`state.db` の `tasks.id`）。ログ・`status`・`retry <n>` と同じ番号なので、名前から**タスクへ戻れる**。source id を切り詰めたものには戻る先が無かった。

**prefix だけツール固有にする。** core が一致するので `3-9f3c2a1e` の 1 回の grep で herdr のエージェント・orca の worktree・ディスク上のディレクトリが同時に引ける。prefix はツールの制約（herdr は英字始まりを要求する）と既存の慣習（orca の `totsuka-`）を吸収する層として残す。

**core を一致させるには `case` を揃える必要がある**（#646 のレビューで判明）。初版の core は数字と小文字 hex だけだったので `Case::Lower` と `Case::Preserve` の区別が出力に現れず、worktree と orca は `Preserve`、herdr だけ `Lower` で問題無かった。D-7 の `handle` は**英字を持ち込む最初の部分**で、`Web-App-42` のような GitHub の handle はそこで割れる（herdr `t-3-web-app-42-…` / 葉 `3-Web-App-42-…`）。したがって **3 つとも `Case::Lower` に揃える** — 小文字は herdr の制約であって他 2 つの制約ではないが、「揃っていること」自体がここでの要件である。

**長さだけは揃わない。** herdr の 32 文字は他 2 つに無いので、長い handle は herdr でだけ切られる。完全な一致を求めて全ツールを最も狭い制約に合わせる案は採らない — 将来もっと狭いツールが 1 つ増えるだけで全員の名前が縮むことになる。したがって**あらゆる場合に byte 一致する部分は `<hash8>`** であり、確実に 3 つ引きたいときはダイジェストで検索する。

## D-2: 読める半分は `job_id` ではなく、新しい `task_number` フィールドで受け取る

`job_id`（`job-<task_id>-<session_row>`）には task 番号が入っているが、**主キーの供給元にはできない**。

- `job_id` が作られるのは、agent が `hook_completion` を宣言し**かつ** workflow にフック起動設定があるときだけである（`wire_hooks`）。**orca は宣言していないので恒久的に `None`** で、`job_id` から取る設計では orca を揃えられない
- `session_row` は `reserve_session` の INSERT が返す rowid で、**再 dispatch ごとに変わる**

そこで protocol 0.7.1 で [`TaskDispatchParams.task_number: Option<i64>`](https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/methods.rs) を足し、Orchestrator が**毎回**埋める。`job-<n>-<m>` をプラグイン側で parse する必要も無くなる。

**`None`（0.7.1 より古い Orchestrator）では source id を sanitize したものにフォールバックする。** 名前が変わるだけで壊れないので、**プラグインの manifest 下限は上げない** — 上げれば、問題なく動く Orchestrator を拒否することになる。これは 0.7.0 が逆向きに判断した点と対になっている: あちらは 0.6 世代の source が `projects` を無視して**運用者が絞ったはずのボードを黙って全部見る**ので F-54 で拒否する必要があった。こちらの縮退は目に見えて無害で、次のリリースで自然に直る。

## D-3: `session_row` は名前に含めない

**ADR-0032 D-3 の前提を壊さないため。** D-3 は「`agent_name_taken` が返るのは同じタスクのエージェントがまだ生きているとき = 掃除されていない異常」と読み、別名で自動回避せず dispatch を失敗させる。これは**同じタスクなら同じ名前**という決定論の上にしか成り立たない。dispatch ごとに変わる名前にすると `agent_name_taken` は二度と起きず、孤児 pane が積み上がったまま成功し続ける（#481 の状況が無言化する）。

物理的にも入れられない。**worktree は `reserve_session` より前に作られる** —
`dispatch_one` の順序は worktree 作成 → `wire_hooks`（session row 採番）→ `task/dispatch` なので、worktree 名に session row は存在しない。含める設計は worktree だけ揃わないことを意味する。

## D-4: ハッシュは切り詰めの有無に関わらず常に付け、`source` を入力に含める

task 番号が一意なのは**1 つの `state.db` の中だけ**である。2 つの totsuka インスタンスが 1 つの herdr / orca を共有すれば、両方の task 3 が同じ名前を要求する。長さに余裕がある orca や worktree でもハッシュを落とさないのはこのためで、「切り詰めるから付ける」ではなく「一意性はハッシュが担う」と読む。

入力は `source` と `source_task_id` を `\0` で連結したものにする。`\0` が無いと `("ab","c")` と `("a","bc")` が同じダイジェストになり、ソース名の末尾と id の先頭が噛み合う組み合わせで他タスクの identity を名乗れてしまう。`source` を含めるのは、GitHub の `42` と Notion の `42` を別物として扱うためである。

## D-5: 制約はツールが宣言し、手順は `plugin-protocol` が持つ

```rust
pub trait IdentifierPolicy {
    // required — 制約の宣言だけ
    fn prefix(&self) -> &str;
    fn max_len(&self) -> Option<usize>;
    fn case(&self) -> Case;
    fn extra_allowed(&self) -> &[char];

    // provided — sanitize → 区切りを予算に含めて切り詰め → hash8
    fn identifier(&self, core: &IdentifierCore<'_>) -> String { … }
}
```

**`plugin-protocol` に置く。** Orchestrator（worktree 名）・同梱プラグイン・外部のプラグイン作者の**全員が到達できる唯一の公開クレート**であり、`plugin-sdk` は task_source 向けのランタイムで core から使うと層が逆転する。代償として `sha2` が公開クレートの依存に加わるが、`version.rs` が semver 判定を持っているのと同程度の逸脱と見る。

**性質テストは 1 回だけ書く。** 「任意の入力 × 任意の `IdentifierPolicy` で、出力は prefix で始まり・許可文字のみ・`max_len` 以下」を決定論的な擬似乱数コーパスに対して検査する。3 つの sanitizer がそれぞれ自分について主張していたことを、trait 1 つについて主張する形に変わる。後から足すツールは検査を書き直さずに継承する。

**区切り文字の予算は切り詰めの前に引く。** これが今回のバグを表現不能にする 1 行で、`max_len - (prefix + sep + hash)` を可読半分の予算とする。結果として予算が区切りの位置でちょうど尽きると名前は上限より 1 文字短くなる（例の id で 31 文字）。誰も読まない id の 1 文字と引き換えに、はみ出しが起こり得なくなる。

## D-6: worktree の葉はテンプレートをやめて識別子にし、`location` にはプレースホルダを足す

葉（`{worktree_name}`）は `WorktreeLeaf`（`IdentifierPolicy`）の出力 `<task 番号>-<hash8>` にする。**テンプレート機構そのものを削除する** — `DEFAULT_WORKTREE_NAME_TEMPLATE`（`{source}-{task_id}`）・`render_worktree_name` / `render_legalized` / `sanitize_branch_for_path`・設定フィールド `worktree_name_template` を落とす。理由は 2 つある。

- **合法化に仕事が無くなった。** あの git ref 合法化（`:` 空白 `~^?*[\`、`..`・`.lock`・`@{`、先頭 `-`）は葉がブランチ名だった頃の遺産で、[ADR-0026](/decisions/adr-0026-agent-owned-branch-and-push.md) 以降そうではない。ポリシーが出すのは `[A-Za-z0-9_-]` だけで、git ref 規則にもパス要素規則にも**厳密に収まる**
- **1 つの値しか取らない設定は設定ではない。** `worktree_name_template` は `config.toml` から到達できず、既定値以外が入ることは無かった。残したまま葉をポリシーに変えると、「3 ツールが同じ core を持つ」性質が**テンプレート文字列の偶然の一致**として手で維持されることになる

`[worktrees].location` には `{task_number}` / `{hash}` を**追加**する（葉の 2 つの半分を別々に置き、運用者が区切りを変えたり片方を落としたりできる形にする）。**`{task_id}`（source id）と `{source}` の意味は変えない** — 意味の差し替えは既存の設定の出力を黙って変える。

既存の worktree に移行は要らない。掃除・孤児検出・`doctor`・再利用ガードはいずれも `tasks.worktree_path` に記録された**フルパス**を読み、**名前を parse して task を復元している箇所は 1 つも無い**。

## D-7: 読める半分の隣に、ソースが名付ける `handle` を置く（0.7.2 / #646）

D-1 の名前は「どのタスクか」に答えるが、「**何の**タスクか」には答えない。`t-3-9f3c2a1e` を見て `totsuka status` は引けても、それが web リポジトリの issue 42 なのか Slack の #dev-support なのかは分からない。herdr の 32 文字にはまだ 20 文字ほど余っている。

そこで [`Task.handle: Option<String>`](https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-protocol/src/task.rs) を protocol 0.7.2 で足し、**ソースが人間向けの短い名前を書く**。識別子では task 番号とダイジェストの間に入る。

**切られるのは handle だけ。** 番号は `totsuka status` へ戻る手段で、ダイジェストは一意性の担い手なので、予算が足りないときに落とせるのは可読性だけである。切り詰めは末尾から行うので、**ソースは識別性の高い順に並べる**（`web-app-42` であって `42-web-app` ではない）。

各ソースが何を入れるか、そして**何も入れないこと**も正しい答えである:

| ソース | handle | 理由 |
|---|---|---|
| GitHub Projects | `{repo}-{issue 番号}` | 人が呼ぶ名前そのもの。`Task.id` は base64 の node id で、これになれない。1 つのボードが複数 repo を追うので番号だけでは曖昧 |
| Slack | チャンネル**名**（`dev-support`） | 人は `#dev-support` を読み、`C0ABCDEF12` は読まない。ts は**入れない** — 予算で数字の途中で切られるうえ、それが区別するもの（同一チャンネルの 2 スレッド）は task 番号が既に区別している |
| Discord | チャンネル**名** | 同上。Discord の id は全て snowflake で、名前だけが人の読むもの |
| Notion | **無し** | page id は UUID、title は散文。短く・安定し・パス安全という条件を満たすものが無い。title から作ると**改名可能な文字列が識別子に入る** |

**一意性は要求しない。** それはダイジェストの仕事なので、重複する handle は何も壊さない。逆に言えば handle は**識別子ではない**ので、これで dedup する経路を作ってはならない。

# Consequences

## 良くなること

- **`invalid_agent_name` が構造的に起こらない。** 予算計算が 1 か所になり、性質テストが任意の入力について検査する
- **名前からタスクへ戻れる。** `herdr agent list` の `t-3-…` を見て `totsuka task retry 3` が打てる
- **1 回の grep で 3 つのツールを横断できる。** core が一致しているため
- **4 つ目の agent_ide が sanitizer を書かなくてよい。** 制約 4 つの宣言で済む

## 悪くなること・注意点

- **既存の名前がすべて変わる。** 名前を後から参照しているのは同一 dispatch 内の `agent.start` 再送（#387）だけで、`agent.prompt` は pane id 宛て、orca は `create` 以後 `id:<worktree_id>` 宛てなので影響は無い。ただし**運用中の worktree ディレクトリ名が新旧混在**する（古いものは記録されたパスで参照され続ける）
- **`sha2` が `plugin-protocol` の依存に入る。** 公開クレートの依存が 1 本増える
- **上限ぎりぎりの名前が 1 文字短くなることがある**（D-5 の末尾）
- **ハッシュは短い。** 4 バイトなので、生存中のエージェント数の規模では衝突しないが、無限に安全ではない。衝突したときは `agent_name_taken` で**止まる**（D-3 の経路）ので、静かに取り違えるのではなく失敗する
- **アップグレードをまたいだ 1 回だけ、孤児の検出が効かない窓がある。** 名前は保存されず毎回導出されるので、名前を決めるのは常に「今動いているビルド」である。旧ビルドが起動したエージェントが生きている最中に更新し、**その task を retry した場合に限り**、新しい名前は旧エージェントと一致せず `agent_name_taken`（D-3）は自分の孤児を認識できない。2 つ目のエージェントがその横に立つ。孤児自体は見失わない — `totsuka doctor` は workspace label（`totsuka <source_task_id>`、本 ADR は触っていない）経由で見つける（[ADR-0013](/decisions/adr-0013-orphan-pane-detection.md)）。**フォールバックだけ旧ハッシュを残しても窓は狭まらない**: 読める半分も同時に変わるうえ、変わらないのは `task_number` が来ない経路だけで、主経路はどのみち改名されるからである

## 実機で確かめること

- herdr で `t-<n>-<hash>` が `agent.start` に通ること、`herdr agent list` の見え方
- 2 本目の PR 後、orca の `--name` が新しい形を受け付けること（**orca の `--name` に文字種・長さの制約は公開情報が無く、重複時のエラー経路もプラグインに無い**ため、実機でしか確認できない）

# 不採用案

| 案 | 不採用の理由 |
|---|---|
| herdr の予算計算だけ直す | 3 つの実装が残り、4 つ目が書かれる。今回の 1 文字は「直せば済む」が、同じ種類の誤りを次に検出する手段が増えない |
| herdr の 32 文字上限を上げてもらう | herdr は外部ソフトで、schema 上 `name` は素の `string`・制約はランタイムにしか無い（暗黙契約 C-6）。運用者が入れる版を選べない以上、プラグインは保守的な値に合わせるしかない |
| `job_id` 文字列をそのまま読める半分にする | orca には永久に届かない（D-2）。`session_row` が入ることで D-3 の孤児検出も失われる |
| ハッシュを `max_len` のあるツールだけに付ける | 一意性の担い手が切り詰めの有無で変わることになる。別インスタンスの同番号が衝突する（D-4） |
| trait ではなく値（`IdentifierRule` 構造体）で宣言する | 実装としてはほぼ同じだが、ツール固有の事情（将来 `agent.start` が別の形を要求する等）を型で表現する余地が無くなる。required を制約に限れば手順の分岐は防げる |
| `plugin-sdk` に置く | core（worktree 名）から使えない。core が sdk に依存すると「sdk はプラグイン作者向け」という層が逆転する |
| worktree 名テンプレートを残し、`{task_number}` / `{hash}` プレースホルダで葉を組む | 葉がポリシーの出力ではなくなり、3 ツールの core 一致がテンプレート文字列の偶然の一致に落ちる。到達不能な設定を残す対価としては高い（D-6） |
| ~~task_source が読める handle を提供する~~ | **採択した**（D-7、#646）。初版では範囲外としていたが、余った予算の使い道として分離したまま放置する理由が無かった。Slack の handle だけは当初案（`{channel}-{ts 秒}`）を採らず、チャンネル**名**にしている |

# 関連

- [ADR-0032 herdr protocol 17 への追随](/decisions/adr-0032-herdr-protocol-17.md) — D-2 を本 ADR が置き換え、D-3 の前提を維持する
- [ADR-0026 ブランチはエージェントが決める](/decisions/adr-0026-agent-owned-branch-and-push.md) — worktree が detached HEAD で渡る理由。葉の名前がブランチ名と無関係なのはこのため
- [herdr の暗黙契約](/references/herdr-implicit-contracts.md) — C-6（`name` の規則は schema に無い）
- [plugin-protocol](/components/plugin-protocol.md) / [agent-ide-herdr](/components/agent-ide-herdr.md) / [agent-ide-orca](/components/agent-ide-orca.md) / [orchestrator-core](/components/orchestrator-core.md)
