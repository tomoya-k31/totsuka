---
type: Decision
title: ADR-0017 state.db の互換性ポリシー（前方互換のみ・適用は run のみ）
description: state.db の互換判定をスキーマ版数で行い、対応範囲より新しい DB は起動拒否する決定。マイグレーションの適用は run.lock を持つ totsuka run だけに限定し、読み取り系は非適用オープンにする。適用したアプリ版数は schema_migrations.applied_by に残すが、互換判定の権威にはしない。
tags: [state-db, migration, compatibility, versioning, sqlite]
generated: { by: human:tomoya-k31, at: 2026-07-26T15:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: ref-1
    resource: /data/state-db.md
    title: "状態DB（SQLite state.db）スキーマ — `schema_migrations` と `MIGRATIONS` の詳細"
  - id: ref-2
    resource: /releases/upgrade-and-rollback.md
    title: "アップグレードとロールバック — 実際の手順"
  - id: ref-3
    resource: /decisions/adr-0012-cli-exit-codes-json-errors.md
    title: "ADR-0012 CLI の exit code 体系と JSON エラーエンベロープ — 「原因 → 次のアクション」のエラー文規約"
  - id: ref-4
    resource: https://github.com/tomoya-k31/totsuka/issues/275
    title: "#275 — 調査と設計"
---

# Status

Accepted — 2026-07-26（[#275](https://github.com/tomoya-k31/totsuka/issues/275)。PR ①=#285 / PR ② で実装）

# Context

「バージョンアップに合わせて DB のマイグレーションを実行したい」という要望から調査したところ、**マイグレーション機構そのものは既にあった**。`MIGRATIONS`（index+1 = version）を `StateDb::init` が `schema_migrations` の `MAX(version)` と比較し、未適用分を 1 バージョン = 1 トランザクションで適用する。適用前のバックアップも取っている。

空いていたのは機構ではなく、その周辺の 4 つの穴だった。

1. **ダウングレードが無警告**。`if (current as usize) < MIGRATIONS.len()` は `current > len` のとき単に false になり `init` は `Ok` を返す。**知らないスキーマのまま動き続ける。** 追記のみの差分（v5 の `task_messages` 追加のような）ならエラーすら出ず、静かに食い違う。config 側には `UnsupportedVersion` という同種のガードが既にあり、state.db にだけ対応物が無かった。
2. **アプリ版数がどこにも残らない**。state.db にも state_dir にも totsuka の版数の記録が無く、「この DB を上げたのはどの版か」が事後に一切追えない。
3. **ロック外でスキーマが変わりうる**。適用の発火点が `StateDb::open` なので、`run.lock` を取らない `status` / `task` / `focus` / `doctor` からも走る。`run.lock` を取るのは `totsuka run` だけで、`busy_timeout` はワークスペース全体で未設定。バージョンアップ直後に `run` と `status` を同時に叩くと、単一ロック下でないスキーマ変更が起きうる。
4. **バックアップがどの版か判別できない**。`{path}.bak` 固定名で毎回上書きしていた。

# Decision

## 1. 互換判定の権威はスキーマ版数（アプリ版数は案内専用）

`MAX(schema_migrations.version)` と `MIGRATIONS.len()` を比較する。`current > len` なら [`StateError::SchemaTooNew`] で**起動を拒否**する。

**不採用: アプリ版数を権威にする。** スキーマを一切変えないパッチリリース（0.1.5 → 0.1.4）でも弾いてしまい厳しすぎる。実際に非互換なのはスキーマだけなので、判定もスキーマで行う。

アプリ版数は「どの版に上げればよいか」を**案内**するためだけに使う。エラー文は対応範囲の 1 つ先（`supported + 1`）を導入したアプリ版数を名指す — それが「最低これに上げろ」の答えだから。

```text
error: state.db のスキーマバージョン v8 は、この totsuka 0.1.4（対応 v7）では
       扱えません。v8 を導入したのは 0.2.0 です → totsuka を更新してください
```

## 2. 適用は `totsuka run` のみ（`run.lock` 保持下）

`StateDb::open` は従来どおり適用する。**`run.lock` を持たないコマンドのための** `StateDb::open_no_migrate` を新設し、CLI の `Cx::open_state_db`（`status` / `task` / `focus` / `doctor` が通る唯一の入口）をそちらへ差し替えた。未適用のスキーマは `SchemaOutdated` として `totsuka run` を案内する。

非適用オープンは**スキーマ・台帳への書き込みを一切行わない**（`applied_by` のブートストラップ ALTER も含む）。SQLite の `CREATE` フラグも落としてあり、`state.db` が無いときに空 DB を作ってしまうこともない。ただし最終接続のクローズ時に SQLite が WAL をチェックポイントすることはある（どの接続でも起きる、コミット済みページの畳み込み）。

**不採用: 現状維持 + `busy_timeout`。** 二プロセスが同時にマイグレーションを**始める**窓が残る。`busy_timeout` はロックの待ち時間を伸ばすだけで、「誰が適用してよいか」を決めない。

**不採用: 読み取り系は read-only で開き、古いスキーマのまま読む。** 全クエリの下位互換を人手で保証し続ける契約になる。加えて `task cancel` / `task retry` は同じ入口を通る書き込みコマンドなので、read-only では成立しない。

## 3. 前方互換のみ。ダウングレードはガード導入版以降でしか救えない

新しい totsuka が古い DB を上げるのはサポートする。古い totsuka が新しい DB を読むのはサポートしない。

**原理的な制約**: ガードのコードを持たない既存版（本機能の導入版より前、0.1.4 以前）へ戻した場合、その版はガードを実行しないので**この決定では救えない**。ガードが効くのは本機能の導入版以降どうしの間だけ。だからバックアップからの復旧手順（[アップグレードとロールバック](/releases/upgrade-and-rollback.md)）が必要になる。

**不採用: 明示的な `totsuka db migrate` コマンド。** `totsuka run` を一度走らせれば適用されるため、覚えるコマンドを増やす価値がない。

## 4. `applied_by` は台帳に持ち、`MIGRATIONS` には載せない

各バージョンを**導入した**アプリ版数（`CARGO_PKG_VERSION`）を `schema_migrations.applied_by` に記録する。nullable で、列を持たなかった旧バイナリが書いた行は NULL = 「不明」のまま。バックフィルはしない（その版が実際に適用したわけではないため）。

**不採用: `meta(key, value)` KV に `last_app_version` を持つ。** 「最後に開いた版」しか分からず、「どの版に上げればよいか」を導けない。毎起動で書き込みも発生する。

**列の追加は `StateDb::init` のブートストラップ段階（適用ループの前）で条件付きに行う。** これを `MIGRATIONS` のエントリとして書くと順序が循環する:

`schema_migrations` は `MIGRATIONS` の各エントリを**採番している側**のテーブルである。ALTER を仮に v8 として書くと、v5 の DB を v8 まで一気に上げるとき **v6 の INSERT が v8 の ALTER より先に走り** `no such column: applied_by` で落ちる。台帳テーブル自身を、その台帳が管理するバージョン番号で管理することはできない。

ブートストラップに置くことで、適用ループ内の INSERT は常に `applied_by` を書ける。この罠は回帰テスト（`applies_two_versions_at_once_over_a_legacy_ledger`）で固定してある。

## 5. バックアップはスキーマ版数付き

`{path}.v{適用前バージョン}.bak`（例 `state.db.v7.bak`）。

**不採用: 固定名 `.bak`。** アップグレードのたびに上書きされるため、2 世代分を一気に上げると中間地点に戻れない。ディスク上の `.bak` がどのスキーマ版かも外から分からない。

**不採用: タイムスタンプ命名。** 削除ポリシーが要るうえ、ファイル名からスキーマ版数が読み取れない。

旧命名の `state.db.bak` は削除せず残置する。

**不採用: デフォルトを非適用に反転し、`open` を明示的な適用版にする。** テスト側のファイル DB `open` 約 90 箇所を全書き換えすることになり、レビュー不能な PR になる。既存の `open` は据え置き、非適用版を**足す**。

# Consequences

- 古いバイナリが新しい DB を開くと、`run` でも他のコマンドでも「どの版に上げればよいか」を含むエラーで止まる。
- スキーマ変更は `run.lock` を保持した `totsuka run` の中だけで起きる。**分岐の基準はロックであって読み書きではない** — `task cancel` / `retry` / `verify` は同じ非適用オープンを通って行を書き換えるが、スキーマは触らない。
- `totsuka doctor` がスキーマ版数と `applied_by` を表示する（`state-db — … opens — schema v7 (applied by 0.1.4)`）。不整合時は exit 3。
- アップグレード直後の初回は `totsuka run` を一度走らせる必要がある。それまで `status` 等は `SchemaOutdated` で止まる — 黙って古いスキーマを読むより、これを望ましい挙動とみなす。
- `--json` 出力にスキーマ版数のフィールドは**足していない**。現時点で消費者がいないため。必要になった時点で追加する。
