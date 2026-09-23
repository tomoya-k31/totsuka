---
type: Decision
title: ADR-0097 ログの時刻は保存を UTC、表示をローカル時刻にする
description: ログの時刻はファイルも端末も UTC で、日本の利用者は 9 時間ずらして読む必要があった。journald や Docker にならって保存（JSON Lines ファイル）は UTC のまま、人間が読む表示（run などの端末出力と totsuka logs）だけ OS のタイムゾーンで出す決定。時差は起動直後にスレッドを作る前に time で 1 回だけ取得し、logs には --utc を足す。
tags: [decision, logging, timezone, cli, adr]
generated: { by: claude-code/opus-5, at: 2026-09-24T10:30:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: journal-fields
    resource: https://freedesktop.org/software/systemd/man/latest/systemd.journal-fields.html
    title: systemd.journal-fields — __REALTIME_TIMESTAMP
  - id: moby-46910
    resource: https://github.com/moby/moby/issues/46910
    title: moby/moby#46910 — local log driver の時刻
  - id: otel-logs
    resource: https://opentelemetry.io/docs/specs/otel/logs/data-model/
    title: OpenTelemetry Logs Data Model
  - id: time-soundness
    resource: https://docs.rs/time/latest/time/util/local_offset/fn.set_soundness.html
    title: time — util::local_offset::set_soundness
---

# Status

stable。[ログ規約](/development/logging-conventions.md)の時刻の扱いを変える。

# Context

ログの時刻はファイル（JSON Lines）も端末の表示も UTC の RFC 3339（`…Z`）だった。日本の利用者は `totsuka run` の出力や `totsuka logs` を 9 時間ずらして読む必要がある。

2026 年時点の実装を調べると、ライブラリの既定値はばらばらだった（Python logging・Go の `slog`・Logback はローカル時刻、zap の本番設定・Docker の `json-file`・OTel は UTC か epoch）。ただし、保存と表示を分ける作りに収束している。

- journald は UTC の epoch で保存し、`journalctl` が既定でローカル時刻で表示する（`--utc` で切り替え） [^journal-fields]。
- Docker は「ログは常に UTC、別のタイムゾーンで見せるのは表示側の話」としている [^moby-46910]。
- OTel の Logs Data Model は時刻を epoch のナノ秒で持ち、タイムゾーンを保存形式に入れない [^otel-logs]。

ファイルにローカル時刻を書くと、時差の違う行が混ざったとき（夏時間の切り替えや、別の地域で動かしたとき）に文字列の並びと時系列がずれ、`jq` のソートや比較が壊れる。

Rust の `time` クレートは、Unix のマルチスレッドプロセスではローカル時差の取得をエラーにする（`localtime_r` が `setenv` と競合するため） [^time-soundness]。

# Decision

1. **ログファイルは UTC のまま**（`…Z`）。状態 DB も UTC なので揃ったままになる。
2. **人間が読む表示はローカル時刻にする**（例: `2026-09-24T09:16:23.349+09:00`）。
   - `RedactingLayer` の人間向け表示（`run` を含む各コマンドの端末出力）
   - `totsuka logs` の表示。`--utc` を付けると保存どおり UTC で出す
3. **時差は起動直後に 1 回だけ取る。** `logging::local_offset()` を `main` の先頭（スレッドを作る前）で呼び、以後はその値を使う。取れなければ UTC で表示する。`TZ` を設定すればそちらに従う。
4. **依存は増やさない。** `time` の `local-offset` feature を有効にするだけにした。

# 代替案と不採用理由

- **ファイルもローカル時刻にする。** 上の `jq` の問題があり、どの外部実装もとっていない。
- **`jiff`（または chrono 4.20 以降）で毎回取得する。** libc を使わずタイムゾーン情報を自分で読むのでスレッドがあっても安全で、夏時間の切り替えも正しく扱える。ただし、表示のためだけに依存が 1 つ増える。
- **何もしない（UTC 表示のまま）。** 利用者が毎回換算することになる。

# Consequences

- `run` を動かしたまま夏時間の切り替えをまたぐと、再起動するまで端末の表示が 1 時間ずれる。ファイルの時刻は正しいままで、夏時間の無い JST では起きない。困るようになったら `jiff` に切り替える。
- 端末表示と `totsuka logs` の時刻は時差付きなので、UTC との換算を読み手がする必要はない。

[^journal-fields]: systemd.journal-fields — __REALTIME_TIMESTAMP
[^moby-46910]: moby/moby#46910 — local log driver の時刻
[^otel-logs]: OpenTelemetry Logs Data Model
[^time-soundness]: time — util::local_offset::set_soundness
