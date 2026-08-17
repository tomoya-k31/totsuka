---
type: Decision
title: ADR-0049 ローカルのテストランナーを nextest にし、変更影響範囲の絞り込みは採らない
description: "ローカルの PR 前テストを scripts/dev-test.sh（build → nextest → doctest）に一本化し、RUSTFLAGS と --all-features をローカルコマンドから外す決定。#459 が主眼としていた rdeps() による変更影響範囲の絞り込みは、実測でフルランが 8 秒だったため採らない。CI のランナー構成は変えない。"
resource: https://github.com/tomoya-k31/totsuka/issues/459
tags: [decision, testing, performance, developer-experience, tooling, adr]
generated: { by: claude-code/opus-5, at: 2026-08-17T17:09:03+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-459
    resource: https://github.com/tomoya-k31/totsuka/issues/459
    title: "ローカルテストループを nextest + rdeps フィルタセットで変更影響範囲に絞る"
  - id: adr-0018
    resource: /decisions/adr-0018-ci-test-time.md
    title: "ADR-0018 CI テスト時間の削減（待ちの構造化・profile・計測の作法）"
  - id: adr-0029
    resource: /decisions/adr-0029-ci-cache-lifecycle.md
    title: "ADR-0029 CI キャッシュのライフサイクル"
---

# Status

Accepted — 2026-08-17（[#459](https://github.com/tomoya-k31/totsuka/issues/459)）

# Context

[#459](https://github.com/tomoya-k31/totsuka/issues/459) は「ローカルの PR 前チェック
`RUSTFLAGS="-D warnings" cargo test --workspace --all-features` が毎回 20 分弱かかる」
という観測から出発し、`cargo nextest` の `rdeps()` filterset で**変更影響範囲のテストだけを
実行する**仕組みを提案していた。その 20 分は直接計測されたものではなく、`target/` に残った
アーティファクトの mtime から復元した推定である。

**実装前に基準を測ったところ、この前提が崩れた。** `target/` は 1,131,673 ファイル /
80.2 GiB まで肥大していた（`cargo clean` に 222 秒）。その clean 直後に、
**同じコマンドをそのまま計測すると 32 秒**だった。

| 計測（同一マシン・warm。すべて下記「テスト 1 本」の修正**前**） | 結果 |
|---|---|
| 現行フロー `RUSTFLAGS="-D warnings" cargo test --workspace --all-features` | **32s**（1,201 テスト / 45 バイナリ） |
| 同上・`cargo clean` 直後の cold | 86s（うちワークスペース全体ビルド 34s） |
| 新フロー（build 2s + `cargo nextest run --workspace` 16s + `cargo test --doc` 1s） | **約 19s**、テスト数 **1,201 で一致** |

本 PR で遅いテスト 1 本を直したあとの、同条件での比較が下記である。**これが実際に
置き換わる前後**で、どちらも 1,201 テストを実行している。

| 計測（同一マシン・warm・修正後） | 結果 |
|---|---|
| 現行フロー `RUSTFLAGS="-D warnings" cargo test --workspace --all-features` | **20s** |
| 新フロー `bash scripts/dev-test.sh` | **10s** |

肥大した `target/` での実行そのものは計測していないので、20 分の原因が肥大であったと
断定はできない。断定できるのは「clean 後は 32 秒である」ことだけで、**#459 が解こうとした
問題は、少なくともこの形では存在しなかった**。

## 実時間を決めていたのはテスト 1 本だった

nextest の per-test 時間を採ると、**フルランの実時間 15.138 秒に対し、単一のテスト
`agent-ide-herdr::integration a_prompt_that_never_becomes_ready_re_issues_agent_start`
が 15.099 秒**を占めていた（2 位は 2.115 秒）。全体の実時間はこのテスト 1 本の待ちで
決まっており、**変更影響範囲で絞っても縮まない**構造になっていた。

このテストは `PROMPT_READY_WINDOW`（15 秒、[ADR-0018](/decisions/adr-0018-ci-test-time.md)
以降も private const のまま）を待ち切って `agent.start` を再発行する経路を検証している。
待ちを消してから測り直した数値が下記で、絞り込みの利得はここに全部現れている。

| filterset | テスト数 | 実時間 | フルラン（8.0s）との差 |
|---|---|---|---|
| フルラン | 1,201 | 8.0s | — |
| `rdeps(=orchestrator-core)` | 681 | 6.0s | −2.0s |
| `rdeps(=agent-ide-herdr)` | 109 | 1.7s | −6.4s |
| `rdeps(=task-source-notion)` | 35 | 0.2s | −7.8s |

# Decision

## 1. ローカルのテスト実行は `scripts/dev-test.sh` に一本化する

```bash
cargo build --workspace --all-targets
TEST_SUPPORT_PREBUILT_BINS=1 cargo nextest run --workspace
cargo test --doc --workspace
```

3 コマンドをスクリプトにしてあるのは、順序と環境変数に**壊れ方の分かっている前提**が
あるからで、設定や絞り込みのためではない。

- `TEST_SUPPORT_PREBUILT_BINS` は「直前にワークスペース全体をビルドした」ことが前提。
  nextest はテストごとにプロセスを分けるので、立て忘れるとネストした `cargo build` が
  **テストの数だけ**走る（[ADR-0018](/decisions/adr-0018-ci-test-time.md) §3 が CI で
  消したコストが、ローカルでは桁違いに戻ってくる）。逆にビルドせずに立てると古い
  バイナリを検証してしまう。スクリプトはビルドの直後に立てるので、この前提を
  構造的に満たす。
- nextest は doctest を実行できない（上流の制限）。libtest で別に回さないと、
  黙って検査対象から消える。

引数はすべて `cargo nextest run` へそのまま転送する。スクリプト独自のオプションは
持たない — 絞りたいときは nextest の filterset をそのまま使う
（`scripts/dev-test.sh -E 'package(=agent-ide-herdr)'`）。

## 2. `rdeps()` による変更影響範囲の絞り込みは採らない

#459 の主眼だったが、上表のとおり**節約は最大でも 8 秒**で、しかも一番よく触る
`orchestrator-core` では 2 秒しか減らない。これに対して必要な機構は、変更パス →
パッケージの解決、無害と断定できるパスの許可リスト、判定できないパスのフルへの格上げ、
ワークスペース全体の入力（`Cargo.lock` など）のトリガ — いずれも**間違えると
「取りこぼしたのに緑」になる**種類のコードである。8 秒のためにその面積を維持しない。

実装して分岐まで検証したうえで捨てている。捨てた実装から得た事実は残す:

- nextest の `rdeps()` は **dev-dependency の辺も辿る**（`cargo nextest list -E
  'rdeps(=task-source-slack)'` に `orchestrator-cli` のテストが入ることを確認）。
  したがって `test-support` を「常にフル実行へ格上げ」する特別扱いは不要だった。
- `rdeps()` の既定マッチャは **glob**（部分一致ではない）。式を機械生成するなら
  `=` を付けろと nextest のリファレンス自身が警告している。
- 変更ファイル収集をコマンド置換の中で行うと、その中の `exit` は親シェルを
  終わらせられない。`x="$(f | sort -u || true)"` の形は失敗を握り潰し、
  **「変更 0 件だったので成功」という一番危ない誤りに化ける**（再現を確認）。

## 3. `RUSTFLAGS="-D warnings"` と `--all-features` をローカルコマンドから外す

どちらもローカルでは**結果を変えずにビルドを増やすだけ**である。

- `--all-features` はこのワークスペースでは完全な no-op（全 11 パッケージの
  `[features]` が空。[ADR-0029](/decisions/adr-0029-ci-cache-lifecycle.md) に既出）。
- `RUSTFLAGS="-D warnings"` は冗長。root `Cargo.toml` の
  `[workspace.lints.rust] warnings = "deny"` に全 11 メンバーが `[lints] workspace = true`
  で opt-in しており、**`tests/` ターゲットにも効くことを実測した** — 結合テストに
  未使用変数を 1 つ仕込むと、`RUSTFLAGS` なしの `cargo build -p plugin-protocol --tests` が
  `error: unused variable` で exit 101 する。一方でこのフラグは**フィンガープリントには
  入る**ので、clippy / `cargo doc` / rust-analyzer（いずれも `RUSTFLAGS` なし）とは
  別のアーティファクト空間を作り、同じコードを二度ビルドさせる。

**CI の `env: RUSTFLAGS` は別問題で、絶対に触らない。** rust-cache のキャッシュ鍵に
入っており（`warm-cache.yml` が「一字一句同じでなければならない」と明記）、変えると
全キャッシュが無効化される。

## 4. 15 秒のテストは待ちを消す（本番コードには触らない）

`#[tokio::test(start_paused = true)]` にする。fake herdr もテストと**同じランタイム上**の
`tokio::spawn` で動いており、`PROMPT_READY_WINDOW` の待ちは `tokio::time::sleep` なので、
ポーズした時計の上では実時間を消費しない。

- **15.099s → 0.047s**。herdr スイート全体では 15.2s → 1.66s、ワークスペース全体では
  15.1s → 8.0s。
- 検証している経路は変わらない（`agent.start` が 2 回・CLI へのプロンプトが 1 回、という
  アサーションはそのまま通る）。
- フレークがないことを **30 回連続実行で確認**した（失敗 0 件）。
- 本番の定数 `PROMPT_READY_WINDOW` は 15 秒のままで、**production コードは 1 行も
  変えていない**。必要なのは `agent-ide-herdr` の dev-dependency に tokio の
  `test-util` フィーチャを足すことだけ。

## 5. CI は一切変えない

CI のテストランナーは `cargo test` のままで、`.config/nextest.toml` は CI からは
読まれない。[ADR-0018](/decisions/adr-0018-ci-test-time.md) §5 の「cargo-nextest は
不採用」は **2 コアの CI ランナーでの実測に基づく CI 限定の結論**であり、ADR 自身が
「随時手で走らせる位置づけ」と書いている。本 ADR はその位置づけをローカルの既定に
格上げするもので、§5 の撤回ではない。

# 不採用案

| 案 | 不採用の理由 |
|---|---|
| `rdeps()` による絞り込み（#459 の主眼） | 実測で節約は最大 8 秒、`orchestrator-core` では 2 秒。取りこぼしが「緑」に見える種類の機構を、その額のために維持しない（上記 2） |
| `cargo test -p <crate>` で絞る | `-p` は feature unification が変わり**別アーティファクト空間**を作る。フルランと交互に回すと両方を温め続けることになり、`target/` 肥大の一因になる |
| CI にも nextest を導入 | [ADR-0018](/decisions/adr-0018-ci-test-time.md) §5 で CI 実測により却下済み。本件はローカル限定 |
| 遅いテストの待ちを config ノブにする（`HerdrConfig` にフィールド追加） | [#465](https://github.com/tomoya-k31/totsuka/issues/465) で公開ノブを 15 個削ったばかりで、テストのためだけに増やす理由がない。`start_paused` は本番コードにも公開面にも触らない |
| 遅いテストを削除・`#[ignore]` にする | #387 の 40% 失敗を捕まえている経路で、消すのは検査の弱体化そのもの |
| `.cargo/config.toml` にローカル設定を置く | env の `RUSTFLAGS` が `rustflags` を完全に上書きする既知の罠（[ADR-0018](/decisions/adr-0018-ci-test-time.md)）。そもそもローカル専用設定を repo に置く理由がない |
| `[profile.*]` の追加調整 | [ADR-0018](/decisions/adr-0018-ci-test-time.md) で調整済み。これ以上触ると rust-cache の鍵を変えずに全成果物のフィンガープリントを壊す（[ADR-0029](/decisions/adr-0029-ci-cache-lifecycle.md)） |

# Consequences

- ローカルと CI でテストランナーが違う（nextest はプロセス分離、libtest はプロセス共有）。
  乖離は理論上ありうるが、**最終ゲートは CI の `cargo test`** で、そちらは変わっていない。
  逆向きの利点として、nextest のプロセス分離は [ADR-0018](/decisions/adr-0018-ci-test-time.md) §3 の
  env 命名バグを実際に検出した実績がある。
- `agent-ide-herdr` の dev-dependency に tokio の `test-util` が入る。API が増えるだけで、
  `pause()` を呼ばないテストの挙動は変わらない。
- `cargo-nextest` がローカルの前提ツールになる（`scripts/dev-test.sh` が無ければ
  エラー終了して案内する）。CI には要らない。
- `target/` は誰も GC しない。定期的な `cargo clean` が要る事実は
  `.claude/rules/dev-flow.md` に残した。ローカルのフラグ集合を 1 つに保つ（上記 3）ことは、
  アーティファクト空間の増殖を抑えるという意味でも効く。
- **遅いテストは他にも残っている**。`start_paused` を当てたのは実時間を支配していた
  1 本だけで、次点は `orchestrator-core::hook_integration` の 2.077s、herdr にも
  1.5s 級が 2 本ある。全体が 8 秒になった今、これ以上の短縮は別途判断する。
