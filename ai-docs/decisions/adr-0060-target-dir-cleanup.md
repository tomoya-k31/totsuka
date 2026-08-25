---
type: Decision
title: ADR-0060 dev-test.sh が target/ を掃除する（.o は毎回、cargo clean は定期）
description: "ローカルのフル再ビルドが 15 分近くまで劣化した原因は target/debug/deps に溜まった 839,365 個の .o で、対照実験で 6.6 秒が 40〜126 秒になることを実測した。.o はビルドの入力ではないので毎回掃いてもキャッシュを失わない。incremental/ は入力なので消さず、cargo clean を既定 7 日間隔で自動実行する。"
resource: https://github.com/tomoya-k31/totsuka/blob/main/scripts/dev-test.sh
tags: [decision, testing, performance, developer-experience, tooling, adr]
generated: { by: claude-code/opus-5, at: 2026-08-26T00:30:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0049
    resource: /decisions/adr-0049-local-test-loop.md
    title: "ADR-0049 ローカルのテストランナーを nextest にし、変更影響範囲の絞り込みは採らない"
  - id: adr-0018
    resource: /decisions/adr-0018-ci-test-time.md
    title: "ADR-0018 CI テスト時間の削減（待ちの構造化・profile・計測の作法）"
  - id: adr-0029
    resource: /decisions/adr-0029-ci-cache-lifecycle.md
    title: "ADR-0029 CI キャッシュのライフサイクル"
---

# Status

Accepted — 2026-08-26

# Context

`bash scripts/dev-test.sh` が **14 分 59 秒**かかったという観測から始まった。
[ADR-0049](/decisions/adr-0049-local-test-loop.md) が実測した値は 10 秒である。

## 15 分の内訳はコンパイルで、テストではない

成果物の mtime から復元した内訳:

| 区間 | 所要 |
|---|---|
| `cargo build --workspace --all-targets` | **約 14 分 40 秒** |
| `cargo nextest run` + `cargo test --doc` | 約 20 秒 |

再ビルドされたサードパーティ rlib は 756 本中 16 本だけで、依存は温まっていた。
焼き直されたのはワークスペースのコードとテストターゲット全部である
（変更が `crates/plugin-protocol/` = 全 11 crate が依存する葉だったため）。

**したがって「変更ファイルを見てテストを絞る」はこの 15 分に 1 秒も効かない。**
テスト実行は 20 秒しかなく、先に走る `cargo build --workspace --all-targets` は
nextest の filterset では減らない。[ADR-0049](/decisions/adr-0049-local-test-loop.md) の
「rdeps 絞り込みは採らない」はそのまま維持する。

## 原因は `.o` の無制限な蓄積だった

`target/` は 34G / 985,340 ファイルまで肥大していた。その内訳:

```text
target/debug/deps         839,365 個の .o / 15G  ← 1 つのフラットなディレクトリ
target/debug/incremental       21G / 1,126 セッション
```

`.o` は**全件ワークスペース由来**（orchestrator_core 93k / integration 75k /
totsuka 74k …）で、**全件 1 か月以内**に書かれていた。macOS の cargo は dev
プロファイルで `split-debuginfo = "unpacked"` が既定なので `.o` を残し、cargo は
これを GC しない。実測でフル再ビルド 1 回につき約 8,900 個増え、削除は 0 だった。

## 対照実験で因果を確定させた

肥大した木と健全な木を比べるだけでは、ソースも incremental も違うので何も言えない。
**同一ワークスペース・同一の変更（葉クレートを `touch` した warm 再ビルド）で、
`target/debug/deps` のエントリ数だけを人為的に変えて**測った:

| `deps` のエントリ数 | warm 再ビルド |
|---|---|
| 53,733 | 6.6s / 7.2s |
| 89,029 | 11.5s / 12.4s |
| 250,029 | 20.7s / 23.7s |
| 852,733 | **39.9s / 126.3s** |

`sys` 時間が 20 秒から 88〜91 秒へ跳ねる。junk を消すと 11〜12 秒へ戻るので、
**可逆**であることも確認した。

エントリ数の計測は `stat -f '%z' <dir>` で行える。**APFS のディレクトリ `st_size`
は「エントリ数 × 32 バイト」ちょうど**で、36〜852,720 エントリの 7 ディレクトリで
誤差 0 だった。実行 0.005 秒で、実数を数える
`find … -name '*.o' | wc -l` の 8.2 秒とは桁が違う。

# Decision

## 1. `.o` は毎回掃く

`.o` は**ビルドの入力ではない**。`split-debuginfo = "unpacked"` の下では、リンク
済みバイナリのデバッグ情報がそこに置かれているだけである。実測で確認した:

- 250,001 → 664 エントリまで `.o` を全削除した直後の
  `cargo build --workspace --all-targets` が **0.33 秒の no-op**。
  つまり**ビルドキャッシュは 1 バイトも失われない**。
- フル再ビルド 1 回分の `.o` は約 8,900 個 / 1.2G で、掃除は **0.53 秒**。

失うものは 1 つだけで、これも実測で特定してある:

| | `.o` あり | 掃除後 |
|---|---|---|
| `panicked at crates/…/wire_contract.rs:434:5` | あり | **あり**（`file!()` 由来なので不変） |
| バックトレース frame の `at ./tests/wire_contract.rs:434:5` | あり | **落ちる**（関数名は残る） |

その crate を次にビルドし直した時点で戻る。失敗した回は `set -e` で掃除まで
到達しないので、**デバッグ中の回は debuginfo が残る**。

## 2. `incremental/` は消さず、`cargo clean` を定期実行する

`incremental/` は `.o` と違って**ビルドの入力**である。消した直後のフル再ビルドは
**5.57s → 18.14s**（12.6 秒の実損）なので、掃くとその場で損をする。

一方でセッションディレクトリは溜まり続け、実測 19〜30 MB/dir でディスクだけが
膨らむ（1,126 個 / 21G まで到達していた）。回収する手段は `cargo clean` しかない。

そこで `cargo clean` を**定期的に**、テストが通ったあとに自動実行する:

- 既定 **7 日**間隔（`DEV_TEST_CLEAN_MAX_AGE_DAYS` で変更可）
- または `incremental/` が **300 セッション**を超えたとき（実測 19〜30 MB/dir なので
  おおよそ 6〜9 GB にあたる）

最後に掃除した時刻は `target/.dev-test-last-clean` の mtime で持つ。`target/` の
中に置くので `cargo clean` で必ず消え、直後に置き直す。**スタンプが無い状態は
「掃除した直後」と区別できない**ので、そのときは掃除せず置くだけにする（初回の
実行が必ず `cargo clean` になるのを避けるため）。

タイミングをテストの後にしたのは、その回の結果はもう出ていて、待たされているものが
無いからである。代償は次回のフルビルドが incremental 無しの 27.8 秒になることだけ。

## 3. 2 つの機構は互いを安くする

`.o` を毎回掃いているので、定期の `cargo clean` が削除するファイル数は 1 万件台に
留まる。**実測で 0.88 秒 / 11,057 ファイル / 2.2GiB** だった。掃除を怠った木で
`cargo clean` に 222 秒、84 万件の `.o` の削除に 370 秒かかったのと比べると、
2 桁違う。片方だけでは成り立たない関係である。

## 4. `[profile.dev]` も `.cargo/config.toml` も CI も触らない

`split-debuginfo` を `packed` にすれば `.o` は残らなくなるが、`Cargo.toml` を
変えると CI の rust-cache が 1 回全無効化される
（[ADR-0029](/decisions/adr-0029-ci-cache-lifecycle.md) が明示的に警告している箇所）。
掃除で足りるならそこまで踏み込まない。`target/` 肥大はローカル固有の問題で、CI は
毎回 rust-cache から復元するため CI には存在しない。

# 不採用案

| 案 | 不採用の理由 |
|---|---|
| 変更ファイルからテストを絞る（当初の依頼） | 15 分のうちテスト実行は 20 秒。絞っても `cargo build --workspace --all-targets` は減らない。[ADR-0049](/decisions/adr-0049-local-test-loop.md) の判断が今回も成立している |
| 肥大を**警告するだけ**にする | 一度は実装して両方向の発火まで検証したが、「気づいた人が手で消す」に留まる。放置すると必ず再発する種類の蓄積で、しかも掃除は 0.53 秒でキャッシュを失わない。ナグより実行が正しい |
| `[profile.dev] split-debuginfo = "packed"` | 根本的だが `Cargo.toml` を変えるので CI キャッシュが全無効化される（上記 4）。加えて `dsymutil` を 45 本のバイナリに対して回すコストが未計測 |
| `scripts/dev-clean.sh` を別に立てる | 掃除は 0.53 秒で、判断の余地も引数も無い。`dev-test.sh` が毎回やれば済むものに別コマンドを増やす理由がない |
| `incremental/` も毎回消す | 次のフル再ビルドが 5.57s → 18.14s になる。12.6 秒を毎回払う（上記 2） |
| 掃除をビルドの**前**に置く | 掃除の対象は同じだが、その回のテストが落ちたときに debuginfo が既に無い。後ろに置けば、失敗した回は `set -e` で掃除まで到達しない |

# Consequences

- `bash scripts/dev-test.sh` のフルランは実測 **9.6〜11.8 秒 / 1,325 テスト全通過**で、
  [ADR-0049](/decisions/adr-0049-local-test-loop.md) の水準に戻った。
- `target/` は `.o` を溜めなくなる。実測の定常状態は約 2.2〜3.0G。
- 掃除は **Darwin 限定**。`st_size` からエントリ数を割り出す 32 バイト定数が APFS
  固有なので、それ以外の OS では掃除ごとスキップする（`dev-test.sh` はローカル
  専用スクリプトで、CI からは呼ばれない）。
- `DEV_TEST_SKIP_TARGET_CLEANUP=1` で掃除ごと止まる。
  `DEV_TEST_CLEAN_MAX_AGE_DAYS` は整数でなければ **exit 2 で落とす** — 黙って受けると
  `find -mtime +abc` が失敗し、「掃除の時期ではない」と見分けがつかないまま永久に
  掃除されなくなるため。
- `RUST_BACKTRACE` のスタックフレームから自前クレートの `at <file>:<line>` が落ちる
  回がある（上記 1）。パニック位置の表示は影響を受けない。
- 週に一度は `cargo clean` が走るので、その次のフルビルドだけ 27.8 秒かかる。
