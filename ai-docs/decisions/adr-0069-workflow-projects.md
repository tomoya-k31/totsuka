---
type: Decision
title: ADR-0069 workflow は source ではなく projects で domain を名指す
description: "同一 source の複数ボードで Status の option 集合が違う構成が動かない問題への決定。[[workflows]].source を廃止し projects（[[projects]].name の配列・必須）へ置き換え、source は [[projects]].source から導出する。[[projects]] の意味を「起票先トラッカー」から「ソースが持つ domain」へ広げ、slack / discord もキーなしのエントリを 1 本持つ。閉路検査のグラフを (domain, 列名) でキーし、protocol 0.7.0 で WorkflowInfo.projects と status_writebacks を追加する。走査範囲を絞るだけでは綴り違いが無言のままなので、status option の実在検査を config validate のオンライン部と doctor に error として入れる。改名・source の任意併記・スキーマ移動の同梱・移行案内の実装は不採用。"
resource: https://github.com/tomoya-k31/totsuka/issues/626
tags: [decision, config, workflow, projects, protocol, breaking, adr]
generated: { by: claude-code/opus-5, at: 2026-09-07T12:00:00+09:00 }
status: draft
owner: tomoya-k31
---

# Status

draft。実装済み・テスト green（1,617 件）だが、実機検収（`live-e2e`）は未了。

実装は 2 本の PR に分かれている: `source` → `projects` の本体（#627）と、status option の実在検査（§7）。

[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md) の「`[[projects]]` はリポジトリの起票先トラッカーである」を**この 1 点について改訂する**。ADR-0058 は全体としては有効で、`deprecated` にはしない（[ADR-0062](/decisions/adr-0062-status-vocabulary.md) が `trigger.status` について同じ形の 1 点改訂をしている）。

# Context

`[[workflows]]` は `source` でプラグインを名指すだけで、そのプラグインが持つ**どのボードを見るか**を言えなかった。github プラグインの fetch は workflow の trigger ごとに `[[projects]]` の全ボードを走査する（#542）ので、ボードごとに Status の option 集合（スイムレーン）が違う構成では、方向によって別々に壊れた。

| 向き | 挙動 |
|---|---|
| 取り込み（`trigger.status`） | **黙って 0 件**。`fieldValueByName` が `null` を返し、突き合わせが `false` になるだけ。エラーも警告もログも無い |
| 書き戻し（`on_*`） | item が居るボードに option が無ければ `NotFound` のハードエラー。探索中に通過するだけのボードは意図的に許容 |
| 閉路検査（`column_cycles`） | `(source, 列名)` でグラフを組み**ボードを混同**する。ボード A の `on_success = "Done"` とボード B の `trigger = "Done"` を繋いで、実在しない閉路を報告しうる |

回避策は「全ボードで Status のフィールド名と option 名を揃える」しかなく、**ボードごとに別のプロセスを回したい**という要求そのものを否定していた。

## 既存ドキュメントは、実装に無い性質を主張していた

設定リファレンスの閉路検査の節はこう書いていた —— 「検査は同一 `source` 内・字面の一致のみ。列名がたまたま同じだけの別のボードは閉路ではなく、`source` がそれを分けている」。

`source` が分けるのは**別ソースのボード**だけである。同一 source の 2 枚は 1 つの `source` を共有するので、`Done` と `Done` は同じ節点になっていた。主張は正しく、実装がそれに追いついていなかった。

## `source` は導出できる

`[[projects]]` の各エントリは `source` で所有プラグインを名指している（#554）。workflow が `[[projects]].name` を名指せば、`projects → source` は 1 ホップで解ける。**同じ事実の 2 つ目の綴りは、食い違うことしかできない。**

# Decision

## 1. `[[workflows]].source` を廃止し、`projects` を必須にする

```toml
[[projects]]
name = "board-a"
source = "github"
owner = "owner-x"
project_number = 1

[[projects]]
name = "board-b"
source = "github"
owner = "owner-x"
project_number = 2

[[projects]]
name = "slack"          # domain を持たないソースもキーなしのエントリを 1 本持つ
source = "slack"

[[workflows]]
name     = "implement"
projects = ["board-a", "board-b"]   # この 2 枚は同じレーン語彙を共有するという主張
trigger  = { status = "Todo" }
agent    = "herdr"

[[workflows]]
name     = "design"
projects = ["board-a"]              # このレーンは board-a だけ
trigger  = { status = "Design" }
agent    = "herdr"
```

**常に配列で、キー名は複数形。** `[[repositories]].project` は意図的にスカラーで、そのスカラー性が「2 枚のボードが 1 つのリポジトリを主張する状態」を表現不能にしている（#554 の主要な戦果）。同じ `project` というキー名で表によって arity が違うのは読む人間にとって罠なので、arity をキー名に出す。

配列は「**これらの domain は同じレーン語彙を共有する**」という明示的な主張である。`source = "github"`（= 全ボード）が持っていた暗黙の仮定が、列挙に変わる。

## 2. 表現不能にする状態

- **空配列はエラー**。source を導出できないので、その workflow はどのプラグインにも配られない
- **配列内の `source` 不一致はエラー**。未知キーの引き取り（#554）は「workflow が名指す source と agent の両方に聞き、ちょうど 1 つが引き取る」規則なので、source が 2 つあると claimant が一意に決まらない
- 参照先が実在しないのもエラー（`[[repositories]].project` と同じ検査の形）

3 つとも `config validate --offline` で落ちる。#554 が守った「参照連鎖をプラグインを起動せずに辿れる」性質を保つ。

## 3. `[[projects]]` の意味を「ソースが持つ domain」へ広げる

github はボード、notion はデータベース、slack はワークスペース、discord はギルド。**名前は `[[projects]]` のまま維持する。**

改名しないのは、破壊的変更を 1 つに抑えるためである。ADR-0062 が `project` を「GitHub Projects の語」と認定しているので原則としては改名が筋だが、`[[repositories]].project` まで含めると diff がほぼ倍になり、得るものは key 名の語源だけである。代わりに [glossary/project](/glossary/project.md) で「ソースが持つ、名前で指せる管轄単位」と定義し、GitHub の Project から切り離す。

**domain を持たないソースも明示エントリを 1 本書く。** core が暗黙に生成する案（プラグイン名から domain 名を作る）を採らないのは、(1) 参照解決が全ソースで一様になり `--offline` に分岐が入らない、(2) github のボードに同名を付けたときの衝突規則が要らない、(3) #554 / ADR-0058 が捨てた「見えない所有」に戻らない、の 3 点。代価は非トラッカーのソースごとに 2 行増えることだけで、将来 slack が複数ワークスペースを持つときの拡張点にもなる。

## 4. 閉路検査を `(domain, 列名)` でキーする

ボード跨ぎのカード移動は実在するが（`update_status` は「item は後からボード間を移動しうる」を前提にしている）、動かすのは人間なので 1 周ごとに人手が要り、この検査が捕まえたい**自動で回り続けるループ**にはならない。

配列 workflow は名指した各 domain にエッジを張る。ただし**報告は 1 グループ 1 件**で、domain ごとには出さない —— 2 枚のボードで回るループは 1 つの構造であり、直し方も 1 つである。メッセージは walk が到達したボードを名指す。

この変更で、Context に書いた既存ドキュメントの主張が初めて真になる。

## 5. protocol 0.7.0（`WorkflowInfo.projects`）

`WorkflowInfo` に `projects: Vec<String>` を追加する。core は配列を展開せず、そのまま渡す —— `WorkflowInfo` と `[[workflows]]` の 1:1 と、定義順 first-match（F-81）の契約を保つため。domain 単位に展開する案は、同名エントリが並ぶリストへの first-match になり、順序の意味が説明しづらくなる。

**`source` の廃止自体はワイヤに出ない。** `WorkflowInfo` にも `ProjectInfo` にも `source` は元々載っていない（どのプラグインに配るかは core が決めてから宛先を選ぶ）。バージョンが動く理由は追加側にある。

`projects` と対で **`WorkflowInfo.status_writebacks`** も 0.7.0 で入る（下記「status option の実在検査」）。`on_start` / `on_success` / `on_failure` から core が導出した列名の重複なしリストで、**`on_*` のテーブル自体はプラグインへ渡さない** —— プラグインがそれを解釈して動くことは無く（何をいつ書くかは `task/update_status` が伝える）、渡すのは「ボードと突き合わせられるのはプラグインだけ」という 1 点のためである。`status` が core 所有でありながらプラグインに見えるキーであること（ADR-0062 §3）と同じ配置で、向きが逆になっただけ。

**`projects` は agent には送らない。** `trigger` と同じ理由で、どの domain 由来かはソースの領分である。実装では 1 度これを取り違えて無条件に送っていた（doc は「agent には空」と書いていたのに）—— 宣言した契約が実装に無い状態で、`status_writebacks` のテストが捕まえた。

**minor にして下限を上げるのは github / notion だけ。** フィールドの追加は形式上は互換だが、`WorkflowInfo` は `deny_unknown_fields` ではないので、0.6 世代のビルドは新しいフィールドを無視して**自分の全ボードを走査し続ける** —— 運用者が絞ったつもりの範囲が黙って効かない。0.6.0 の `triggers` → `workflows` 改名と同じクラスの失敗なので、同じ手当て（F-54 のゲートで起動拒否）を採る。

残る 5 本（slack / discord / herdr / orca / macos）は**下限を据え置き、上限だけ `<0.8` へ広げる**。domain が 1 つのソースでは絞り込みが恒等であり、agent と notifier はこのフィールドを読まない。下限は依存を表すもので世代を表すものではない（#411 で orca の下限を herdr と違えたのと同じ判断）。

## 6. 旧 config は素の serde エラーで落とす

移行コードもメッセージ内の名指しも書かない（#554 / ADR-0062 と同じ線）。実際の失敗はこうなる:

```console
$ totsuka config validate --offline
error: failed to parse TOML config: TOML parse error at line 5, column 1
  |
5 | [[workflows]]
  | ^^^^^^^^^^^^^
missing field `projects`
```

`missing field` 側で落ちるので、書くべきキー名はメッセージに出る。残った `source` は名指されない —— `WorkflowConfig` は意図的に `deny_unknown_fields` ではない（プラグインが workflow にキーを定義できる）ので、案内は手書きになる。**書き換え手順はこの ADR とリリースノートが唯一の案内である。**

## 7. status option の実在検査を入れる（`validate` / `doctor` で error）

**`projects` で走査範囲を絞っても、綴り間違いは「無言で 0 件」のまま残る。** 走査先を絞ることは検査ではない —— 存在しない列名は「一致しない」だけで、エラーも警告もログも出ない。動機になった症状の半分はここにある。

検査するのは、列の値を名指すキーのうち **domain 単位で意味が確定するもの**:

| キー | 所有 | 壊れ方 |
|---|---|---|
| `trigger.status` | workflow | **無言で 0 件**。動機そのもの |
| `on_start` / `on_success` / `on_failure` | workflow（core） | 実行時に `NotFound` で**大声で**失敗する。検査するのは「エージェントが働いた後」ではなく「働く前」に落とすため |
| `[[projects]].triage_status` | domain | 無言で Status なしのまま起票される |

**`in_progress_statuses` は対象外。** `[github]` / `[notion]` の全 domain 共通なので、あるボードに無い値が正しく存在しうる（2 枚のボードの実行中列名を union で列挙する運用が成立する）。per-domain 化を決めたら対象に入る。

**置き場所は各プラグインの `config/validate`。** ボードの option 一覧を取れるのはプラグインだけである。`doctor` は同じ RPC を叩くので追加実装なしで乗る。`initialize` では落とさない —— ボードから列を 1 つ消しただけでオーケストレータ全体が起動しなくなり、無関係なワークフローまで止まる。ネットワークが要るので `--offline` では走らない（参照の実在は従来どおり offline で検査する）。

コストは「workflow が名指した domain の数」× 1 クエリ。名指されていないボードは 1 度も叩かない。**検査できなかったこと（transport 失敗）はエラーとして報告する** —— 「検査できていない」が「検査して問題なし」と読まれないため。

## 8. スキーマ `version` は上げない

ADR-0062 と同じ理由。上げると「移行方式」と「`version` 省略時の既定」の 2 決定を先に片づける義務が付き、それに見合う対価がない。

# 移行手順

1. 各 `[[workflows]]` の `source = "<plugin>"` を消し、`projects = ["<domain 名>"]` を書く
2. domain を持たないソース（slack / discord）に `[[projects]]` を 1 本足す（`name` は任意の安定 ID、`source` はプラグイン名、他のキーは無し）
3. `totsuka config validate --offline` を通す

`totsuka setup` が生成する config はこの形に追随している（1 ボード前提は維持。2 枚目は手編集）。

# Consequences

- **既存 config は起動しない。** `projects` が missing field で落ちる
- **`(domain, 列名)` のキーで偽陽性が消えた。** ボードごとに違うレーン語彙を敷く構成が、閉路検査の温床にならない
- **poll の API 呼び出しが減る。** 「ボード数 × workflow 数」から「各 workflow が名指した domain 数の合計」へ
- **`[[projects]]` エントリが増える。** 非トラッカーのソースごとに 2 行
- **workflow が増えうる。** ボードごとに別のレーンを敷くなら (ボード × レーン) 本になる。同じレーンを複数ボードに敷くなら配列 1 本で済む
- **`source` を読んでいた 13 箇所は無変更で済んだ。** `Workflow::from_config` が profile を解決する「唯一の場所」であるという既存の設計に、source の導出を相乗りさせたため（`Workflow.source` は解決済みフィールドとして残る）
- **綴り違いが起動時に大声になった**（§7）。`projects` の絞り込みだけでは「無言で 0 件」は直らないので、これが対になっている
- **`[[projects]]` の意味が 2 つの関係を持つ。** `[[repositories]].project` は起票先、`[[workflows]].projects` は取り込み元。github / notion では一致するが、slack の domain を `[[repositories]].project` に書ける状態が生まれた（下記）

## 意図的に残した穴

**`[[repositories]].project = "slack"` と書けてしまう。** slack は起票先になれないが、そう書いた repo は「トラッカーを設定していない repo」と同じ状態になるだけで、何も壊れない（`project` は元々省略可で、無いのが正常な状態）。github の静的検査「ボードに repo が 1 つも紐づいていない」は従来どおり効く。

塞ぐには `plugin.toml` の capability（「この source の domain は起票先になれる」）を足して `[[repositories]].project` を検査すればよく、`common.rs` のオフライン capability 読み取り経路にそのまま乗る。実害が出ていないので入れない。必要になったら `config validate` の warning から始める。

# Alternatives considered

| 案 | 却下理由 |
|---|---|
| `source` を残し `projects` を任意の絞り込みにする | 動機は同じく解決し、非破壊で済む。ただし `projects` を書いたとき `source` が冗長で、「`source = "github"` かつ `projects` 未指定かつ github の `[[projects]]` が 0 件」のような組み合わせが検査対象として残る。導出できるキーは持たない |
| `[[projects]]` を `[[domains]]` / `[[spaces]]` へ改名 | ADR-0062 の「GitHub の語を core に持ち込まない」原則には忠実だが、`[[repositories]].project` まで含めて diff がほぼ倍になる（設定リファレンス約 30 箇所、コード側の型とエラーメッセージ）。glossary で意味を明記すれば実害はない |
| slack / discord の domain を core が暗黙に作る | ロスター名から domain 名を生成すれば config は増えないが、`[[projects]]` に同名のボードを書いたときの衝突規則が要る。明示エントリ 2 行のほうが安い |
| core が `projects` を domain 単位に展開して配る | プラグイン実装は最も単純になるが、同名 `WorkflowInfo` が並ぶリストへの first-match になり、定義順の意味が説明できない。プラグイン側の `filter` 1 本で足りる |
| 0.6.x のマイナー追加（`#[serde(default)]` のまま据え置き） | 旧プラグインが `projects` を無視して全 domain を走査する。無言の誤スコープで、F-54 が存在する理由そのもの |
| 全 `plugin.toml` の下限を一律 `>=0.7.0` にする | 読まないプラグインまで動作するオーケストレータを拒否することになる。下限は依存を表す（#411 の orca / herdr の判断） |
| `status_field` / `in_progress_statuses` / `property_map` の per-domain 化を同梱する | 本件の動機（option 集合の不一致）とは独立で、しかも共通値で運用可能（`in_progress_statuses` は membership 検査なので両ボードの列名を union で列挙すれば動く）。PR を分ける |
| ボードの列名を揃える運用で解決する | ボードごとに別プロセスという要求そのものを否定する |
| エラーメッセージで旧キーの移行先を名指す（`removed_keys_in` 相当） | ADR-0034 に先例はあるが、#554 / ADR-0062 の「移行案内は実装しない」に揃える。`missing field` が書くべきキー名を出すので、案内は ADR とリリースノートに置く |
