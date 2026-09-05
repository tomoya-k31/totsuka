---
type: Term
title: 起動時バックフィル（startup backfill）
description: "チャンネル監視ソースが起動時に、監視チャンネルの直近 N 件かつ年齢上限以内を無条件に再送してプラグイン停止中の取りこぼしを回収する仕組み。台帳が重複を Duplicate として無害化するため永続カーソルを持たず、取りすぎ側に倒してある。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/plugin-sdk/src/watch.rs
tags: [glossary, channel-watch, backfill, idempotency, slack, discord]
generated: { by: claude-code/opus-5, at: 2026-09-06T04:41:00+09:00 }
status: stable
owner: tomoya-k31
---

# 定義

[チャンネル監視トリガ](/glossary/channel-watch.md) を持つソースが**起動のたびに**行う 1 パスの回収処理。監視チャンネルごとに「直近 `watch_backfill_limit` 件（既定 100）かつ `watch_backfill_max_age_hours`（既定 24h）以内」の投稿を REST で読み直し、ライブ経路と**同じフィルタ表**を通して、通ったものを submit する。

# なぜ必要か

Slack Socket Mode も Discord Gateway も、**切断中のイベントは失われる**（Slack 公式が "you may lose events" と明記、Discord は RESUME 窓を過ぎると再生不可）。どちらにもイベント再生 API は無く、回復手段は履歴の読み直しだけ。

メンションなら「もう一度メンションすればいい」で済むが、監視チャンネルに貼った本人は**残ったつもりでいる**ので、取りこぼしに誰も気づかない。

# なぜカーソルを持たないか

Orchestrator の ingest は `(source, id, message_key)` で冪等で、既知の配送は `IngestOutcome::Duplicate` として**何も起こさない**。しかもそのコメントは「プラグイン側の dedup はメモリにあってプロセスと共に死ぬ」ことを明示的にこの用途の理由に挙げている。

したがって:

- **取りすぎ = 完全に無害**（Duplicate で止まる）
- **取り足りない = 投稿が黙って消える**

損失が非対称なので過剰側に倒し、その結果**永続カーソルという状態ファイルを 1 つも増やさずに済む**。壊れ方のパターンが増えないことがこの選択の実利。

# 年齢上限が要る理由

件数上限だけだと、履歴のある既存チャンネルを初めて監視対象に指定した瞬間に、**過去の投稿が最大 N 件そのままタスクになる**。年齢上限はその洪水を「最大 1 日ぶん」に有界化する。通常の再起動（数分〜数時間）の取りこぼしは全件拾えるので、回復力は落ちない。

# 実装上の性質

- ライブ経路と backfill は**同じ関数**（`WatchTriggers::admit`）でフィルタする。[ADR-0068](/decisions/adr-0068-channel-watch-trigger.md) が要求する起動者ゲートを回復経路で飛ばすと、再起動のたびに境界が開く
- 重複排除はプラグイン内の processed セットも共有するので、backfill が拾った投稿を直後に Socket Mode が再配送しても二重にならない
- チャンネル単位の失敗は warn してスキップする。backfill は回復であって、これで起動を落とすとライブイベントまで失う
- 送信はソース自身の submit 経路を通る（SDK は「パスの方針」だけを持つ）。ソースは submit と同時に結果投稿先の座標を記録しており、そこを迂回すると **backfill 由来のタスクだけ結果を返せない**という非対称が生まれる

# 関連

- [チャンネル監視トリガ](/glossary/channel-watch.md)
- [ADR-0068](/decisions/adr-0068-channel-watch-trigger.md) — 決定 4
- [plugin-sdk](/components/plugin-sdk.md) — `BackfillLimits` / `backfill_pass`
