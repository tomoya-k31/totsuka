---
type: Migration
title: アップグレードとロールバック（state.db）
description: totsuka のバージョンアップ時に state.db のマイグレーションを適用する手順と、バックアップから戻すロールバック手順。schema v7 時点。バージョン不整合エラー（SchemaTooNew / SchemaOutdated）の読み方も含む。
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-core/src/adapters/state_db.rs
tags: [migration, state-db, upgrade, rollback, operations]
timestamp: 2026-07-26T15:00:00+09:00
status: active
owner: tomoya-k31
---

# 前提

- 対象は `$XDG_STATE_HOME/totsuka/state.db`（既定 `~/.local/state/totsuka/state.db`）。
- 互換判定の権威は**スキーマ版数**であってアプリ版数ではない（[ADR-0017](/decisions/adr-0017-state-db-compatibility-policy.md)）。スキーマを変えないパッチリリース間の行き来は自由。
- **マイグレーションを適用するのは `totsuka run` だけ**。`status` / `task` / `focus` / `doctor` は適用しない（スキーマ変更を `run.lock` 下に限定するため）。

# アップグレード手順

1. `totsuka run` が動いていれば止める。
2. totsuka を新しい版に入れ替える。
3. **`totsuka run` を一度実行する。** これがマイグレーションの適用契機。適用前に `state.db.v{適用前バージョン}.bak` が自動で作られ、適用時に INFO ログが残る。

   ```
   INFO applying state.db migrations backup=…/state.db.v7.bak from=7 to=8
   ```

4. `totsuka doctor` で結果を確認する。

   ```
   $ totsuka doctor
   ok:   state-db — ~/.local/state/totsuka/state.db opens — schema v8 (applied by 0.2.0)
   ```

`applied by unknown` と出るのは、その版数を適用したのが `applied_by` 列を持たない古いバイナリ（0.1.4 以前）だった場合。異常ではない。

# バージョン不整合エラーの読み方

## `state.db のスキーマバージョン v8 は、この totsuka 0.1.4（対応 v7）では扱えません`

**DB のほうが新しい**（＝ totsuka をダウングレードした、または新しい版で一度起動した state_dir を古い版で開いた）。`run` でも読み取り系でも止まる。

メッセージが名指す版（`v8 を導入したのは 0.2.0 です`）以降へ totsuka を更新すれば解消する。古い版のまま使い続けたい場合は下のロールバック手順でバックアップへ戻す。

## `state.db のスキーマは v7、この totsuka は v8 を必要とします`

**DB のほうが古い**（＝ アップグレード後まだ `totsuka run` を一度も走らせていない）。`totsuka run` を一度実行すれば適用される。

これは読み取り系コマンドでのみ出る。黙って古いスキーマを読むより、適用契機を明示するほうを選んでいる。

# ロールバック手順

新しい版で `run` を走らせてスキーマが上がったあと、古い版へ戻したい場合。

1. `totsuka run` を止める。

2. 戻したいスキーマ版数のバックアップを確認する。ファイル名の `vN` は**そのファイルが保持しているスキーマ版数**（＝ 適用直前の版数）。

   ```
   $ ls ~/.local/state/totsuka/
   state.db          # 現行 v8
   state.db.v7.bak   # 0.1.4 時代のスナップショット
   state.db.bak      # 旧命名の残骸（どの版か不明。下記参照）
   ```

3. 差し替える。**WAL / SHM を必ず消す** — 残っていると新しいスキーマのページが古い DB に適用され、壊れた状態になる。

   ```bash
   cd ~/.local/state/totsuka
   cp state.db.v7.bak state.db
   rm -f state.db-wal state.db-shm
   ```

4. 古い版の totsuka に戻し、`totsuka doctor` で `schema v7` を確認する。

バックアップを取った時点以降のタスク・会話・イベントは失われる。戻す前に `state.db` を別名で退避しておくと、あとから `sqlite3` で参照できる。

# 注意点

## ガード導入版より前へは戻せない

`SchemaTooNew` のガードは **#275 の導入版（0.1.5 以降）にしか存在しない**。0.1.4 以前のバイナリはガードのコードを持たないので、新しい DB を渡しても止まらず、**知らないスキーマのまま動いてしまう**。0.1.4 以前へ戻す場合は、必ず上のロールバック手順でその版が理解できるスキーマの DB を用意すること。

## 旧命名 `state.db.bak` の残骸

バックアップ名にスキーマ版数を入れるようになったのは #275 から。それ以前に作られた `state.db.bak` は**どのスキーマ版か外から判別できない**（固定名で毎回上書きされていたため）。削除はしていないが、ロールバック先としては当てにできない。中身を確認するには:

```bash
sqlite3 state.db.bak 'SELECT MAX(version) FROM schema_migrations;'
```

## 2 世代分を一気に上げた場合

`vN.bak` は適用の走行ごとに 1 つ作られる。v6 → v8 を一度の `run` で上げた場合に残るのは `state.db.v6.bak` だけで、**中間の v7 のスナップショットは存在しない**。v7 へ戻したいときは v6 のバックアップから復元し、v7 までしか知らないバイナリで `run` を一度走らせる。

# 関連

- [ADR-0017 state.db の互換性ポリシー](/decisions/adr-0017-state-db-compatibility-policy.md) — なぜこの方針なのか、不採用案
- [状態DB（SQLite state.db）スキーマ](/data/state-db.md) — `schema_migrations` と各バージョンの内容
- [運用ガイド](/operations/operations-guide.md) — `doctor` のチェック一覧
