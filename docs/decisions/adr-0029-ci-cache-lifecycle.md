---
type: Decision
title: ADR-0029 main スコープの rust-cache を push 時に再生し、キャッシュ鍵に workspace root の Cargo.toml を含める
description: ADR-0007 で clippy/test を pull_request 限定にした結果 main スコープの rust-cache を書く主体が消え、全 PR の初回 run が依存 215 中 186 クレートを再ビルドしていた問題に対し、main への push で専用の warm ジョブがキャッシュを再生する決定。あわせて virtual manifest ゆえ鍵に入らなかった workspace root の Cargo.toml を明示的に鍵へ加え、coverage にも TEST_SUPPORT_PREBUILT_BINS を立てる。リンカ差し替え・単一 shared-key の共有・テストバイナリ統合は不採用とする。
resource: https://github.com/tomoya-k31/totsuka/issues/341
tags: [decision, ci, cost, cache, performance, build, adr]
generated: { by: claude-code/opus-5, at: 2026-08-01T07:00:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-341
    resource: https://github.com/tomoya-k31/totsuka/issues/341
    title: "Issue 341 — CI ビルドが遅い"
  - id: adr-0007
    resource: /decisions/adr-0007-ci-cost-optimization.md
    title: "ADR-0007 CI 実行タイミングの再設計"
  - id: adr-0018
    resource: /decisions/adr-0018-ci-test-time.md
    title: "ADR-0018 CI テスト時間の削減"
  - id: rust-lld
    resource: https://blog.rust-lang.org/2025/09/01/rust-lld-on-1.90.0-stable/
    title: "Rust 1.90.0 で rust-lld が x86_64-unknown-linux-gnu の既定リンカに"
  - id: gha-cache-scope
    resource: https://docs.github.com/actions/using-workflows/caching-dependencies-to-speed-up-workflows
    title: "GitHub Actions — Caching dependencies (ブランチスコープの規定)"
---

# Status

Accepted — 2026-08-01（[issue 341](https://github.com/tomoya-k31/totsuka/issues/341)）

[ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) が**実行タイミング**を、
[ADR-0018](/decisions/adr-0018-ci-test-time.md) が**実行時間そのもの**を扱ったのに続き、
本 ADR は**キャッシュのライフサイクル**を扱う。前 2 者が組み合わさって生んだ欠陥の是正でもある。

# Context

## 発端

「CI が遅い、ビルドに時間がかかりすぎる。`cargo build --workspace --all-targets` は
すべての target をビルドする意味があるのか」という問いから調査した。

**`--all-targets` は原因ではなかった。** `cargo metadata --no-deps` で全 target を
列挙したところ、このワークスペースには `examples/` も `benches/` も存在せず
（`[[example]]` / `[[bench]]` 宣言もゼロ）、`--all-targets` は `--lib --bins --tests`
と等価である。これは直後の `cargo test --workspace` がどのみちコンパイルする集合
そのもので、Build ステップを消しても同じ作業が Test ステップへ移動するだけになる。
また `[features]` を宣言しているクレートが 1 つも無く `#[cfg(feature = ...)]` も
ソースに 1 箇所も無いため、`--all-features` は完全な no-op である。

## 実測

同一ブランチの 1 回目と 2 回目の run を比較した（run `30607247441` / `30607678368`）:

| | PR 初回 run（cold） | 同一 PR の 2 回目（warm） |
|---|---|---|
| `clippy` の `Run clippy` | 52s — 38 `Compiling` + 149 `Checking` | 22s — 0 `Compiling` + 12 `Checking` |
| `test` の `Build` | 115〜129s — **186** `Compiling` | 58s — **11** `Compiling` |
| `test` の `Test` | 20s | 21s |
| 1 run あたり課金 | 5 分 | 3 分 |

`Cargo.lock` のパッケージ数は 215。cold 時の 186 は「依存グラフのほぼ全体」を意味する。
直近 7 日間で 237 PR run / 62 ブランチ（3.8 run/ブランチ）なので、**週に 62 回**この
cold run が発生していた。

## 欠陥 A — main スコープのキャッシュを誰も書いていない

[ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) の `050d41a` で `clippy` /
`test` が `if: github.event_name == 'pull_request'` になり、**main では両ジョブが
走らなくなった**。GitHub Actions のキャッシュはブランチスコープで、PR スコープ
（`refs/pull/N/merge`）のエントリは他の PR から読めない。したがって新しい PR は
必ず「最後に main で `test` が走った時点」のエントリへフォールバックする。

その結果、`v0-rust-test-Linux-x64-35300eb9-e721b7e7 @ refs/heads/main` は
**2026-07-17 作成のまま 2 週間更新されなかった**。`push` で走る `coverage` の
エントリだけが新しいことが、原因を端的に示している。

## 欠陥 B — キャッシュ鍵に workspace root の Cargo.toml が入っていない

`Swatinem/rust-cache` が鍵に含めるマニフェストは `cargo metadata` のパッケージ由来で、
workspace root は virtual manifest ゆえパッケージに現れない。ジョブログの
`Lockfiles considered` には `Cargo.lock` と member 11 個と `rust-toolchain.toml`
しか並ばず、root の `Cargo.toml` が無い。

そして root には `[profile.dev]` / `[profile.dev.package."*"]` / `[profile.test]` が
置かれている。**[ADR-0018](/decisions/adr-0018-ci-test-time.md) の `debug = false` 化は、
キャッシュ鍵を 1 ビットも変えないまま全成果物のフィンガープリントを無効化した。**
これが欠陥 A の化石キャッシュが単に「古い」だけでなく「全滅」している理由であり、
`full match: false` のフォールバックで済むはずの状況が「proc-macro2 から全部再ビルド」
になっていた理由でもある。

## 副次的問題

Actions キャッシュ総量が上限 10 GB を超過していた（調査時 10.49 GB / 39 エントリ、
一時 11.67 GB）。ただし rust-cache は full match のとき `Cache up-to-date` と出して
保存をスキップするため、**PR スコープのエントリは cold run のときしか書かれない**。
cold run が消えれば重複エントリの生成も止まるので、欠陥 A の是正で自然に収束する。

**ただし本 ADR は main スコープ側の増加要因を新たに作る。** 従来 `v0-rust-test-*` の
main エントリは 1 個が放置されているだけだったが、これからは鍵が変わるたびに新しい
エントリが増え、古い方は誰も復元しなくなる。`cache-cleanup.yml` は PR スコープしか
掃除しない（`*) continue ;;` でブランチスコープを意図的に残す）ので、回収は GitHub の
7 日無アクセス LRU に委ねることになる。概算では鍵変更が週約 11 回 × 2 レグ × 約 190MB
で 7 日以内に生きているのは 4 GB 程度、一方 PR スコープの 327MB × 約 20 個（≒6.5 GB）が
ほぼ消えるので差し引きでは減る見込みだが、**実測で確認するまでは仮説**である。
PR の検証項目に「数日後にキャッシュ総量が 10 GB を下回る」を入れてあるのはこのため。
下回らなければ `cache-cleanup.yml` に「`shared-key` 接頭辞ごとに最新 1 個だけ残す」
規則を足す。

# Decision

## 1. main への push で warm ワークフローがキャッシュを再生する

`.github/workflows/warm-cache.yml` を新設する。matrix 2 本で、`test` レグが
`cargo build --workspace --all-targets`、`clippy` レグが
`cargo clippy --workspace --all-targets --all-features -- -D warnings` を実行する。

**`ci.yml` のジョブではなく独立したワークフローにする。** パスフィルタ（Decision 3）は
`on:` 単位でしか書けず、`ci.yml` の `push` に `paths` を足すと `coverage` まで止まる。
`coverage` は main の per-merge テストを兼ねるので全 merge で走らねばならない。

**`env` は `ci.yml` と一字一句同じでなければならない。** rust-cache は `CARGO` /
`CC` / `CFLAGS` / `CXX` / `CMAKE` / `RUST` 接頭辞の環境変数を鍵に含めるので、
ワークフローを分けた結果 `env` がずれると別の鍵ができ、温めたキャッシュが永久に
使われない。**しかも CI は緑のままなので気付けない。**

**コマンドは消費側ジョブと完全一致させる。** 違うとフィンガープリントが揃わず、
温めたつもりのキャッシュが効かない。

**`clippy` と `test` で鍵を分けるのは必須。** `cargo clippy` は check ベースで
依存クレートを `.rmeta` としてしか emit しない（cold ログの `149 Checking` vs
`38 Compiling` が証拠）のに対し `cargo build` は `.rlib` を要求する。1 つの
`shared-key` を共有すると先に保存した側が勝ち、clippy が勝つと `test` は rlib の
無いキャッシュを復元して結局ビルドし直す。

`fail-fast: false` を付ける。片方が落ちてももう片方のキャッシュは温めたい。

## 2. キャッシュ鍵に root の Cargo.toml を加える

`clippy` / `test` / `coverage` / `warm-cache` すべての rust-cache に
`shared-key: <名前>-${{ hashFiles('Cargo.toml') }}` を与える。`[profile]` 変更が鍵を
壊すようになり、欠陥 B は構造的に再発しない。

`shared-key` を明示すること自体にも意味がある。既定の鍵はジョブ id 由来の暗黙依存なので、
ジョブを改名するとキャッシュが無言で孤児化する。warm ジョブがこの鍵空間へ書き込む
前提もあるため明示が要る。

**ハッシュを `shared-key` に畳み込むのは、`key` 入力が使えないため。** 実装時に
`shared-key: test` と `key: ${{ hashFiles('Cargo.toml') }}` を併記したところ、CI が
出力した Cache Key は `v0-rust-test-Linux-x64-<envhash>-<lockhash>` のままで、
ハッシュがどこにも現れなかった。rust-cache v2.9.1 の `src/config.ts:74-88` は
両者を**排他**に扱う:

```js
const sharedKey = core.getInput("shared-key");
if (sharedKey) {
  key += `-${sharedKey}`;
} else {
  const inputKey = core.getInput("key");   // shared-key があると到達しない
  if (inputKey) { key += `-${inputKey}`; }
  ...
}
```

README の「`key` は自動ジョブキーと併存する」は `shared-key` 未設定時のみ真で、
**`shared-key` を設定すると `key` は読まれずに黙って捨てられる**。エラーも警告も
出ないため、CI ログの Cache Key を実際に読むまで気付けない。ドキュメントではなく
出力で確認すること。

## 3. warm はパスフィルタ + 週次 cron で走らせる

鍵は `Cargo.lock` / 各 `Cargo.toml` / `rust-toolchain.toml` / rustc バージョン /
`RUSTFLAGS` に依存し、`.rs` の変更では変わらない。よって warm はキャッシュ鍵が
依存するファイルを触った merge のときだけ走ればよい。実測で **main への merge
178 件のうち Cargo 系を触るのは 46 件（25%）** なので、warm は週 45 → 約 11 回になる。

当初はフィルタ無しを採る予定だった。**rustc の新リリース（6 週ごと）はリポジトリに
変更が無いまま鍵を壊す**（`dtolnay/rust-toolchain` が `toolchain: stable` で最新
安定版を入れるため）のに、パスフィルタではそれを検知できないからである。この判断は
「フィルタ無しなら full match 時は実質 no-op（課金 1 分）」という**誤った見積もり**に
基づいていた。実測すると `warm cache (test)` は 65 秒＝課金 2 分で、no-op ではない。
rust-cache は既定で workspace クレートをキャッシュしない（`cache-workspace-crates:
false`）ため、full match でも 11 クレートを毎回建て直すからである。

そのためフィルタ無しでは課金が週 835 → 846 分と**増えて**しまい、前提を満たせない。
フィルタを入れ、rustc リリースの穴は週次 cron（3 分/週）で塞ぐ。なお Cargo 系を
触る merge が週約 11 回あるので、実際には穴は半日程度で自然に閉じる。

**フィルタには `ci.yml` と `warm-cache.yml` 自身も含める。** 鍵空間を決めるのは
Cargo 系ファイルだけではない。消費側の `shared-key` と `env`（`RUSTFLAGS` /
`CARGO_TERM_COLOR` — rust-cache が鍵に含める）は `ci.yml` にあり、生成側の同じものは
`warm-cache.yml` にある。これを入れ忘れると、**鍵を変える変更が merge されても warm が
走らず、次の PR は cron か手動 dispatch まで cold のまま**になる。この ADR を導入する
PR 自身がまさにその形（ワークフローと docs しか触らない）だったため、初版はこの穴を
持っていた。

## 4. coverage にも TEST_SUPPORT_PREBUILT_BINS を立てる

`cargo llvm-cov` は内部で `cargo test --workspace` 相当を走らせるため、テスト実行前に
workspace の全 bin をビルド済みである。`test_support::sibling_bin` は anchor
（`env!("CARGO_BIN_EXE_totsuka")`）の親ディレクトリからパスを導くので、llvm-cov が
`CARGO_TARGET_DIR` を差し替えても自動追従する。

これまで `coverage` は Build ステップも当該 env も持たず、main への merge のたびに
E2E が `cargo build` をシェルアウトしていた。すなわち
[ADR-0018](/decisions/adr-0018-ci-test-time.md) の「CI では 0 回」は PR の `test`
ジョブでしか成立していなかった。この 1 行で main でも真になる。

仮定が外れた場合は `sibling_bin` の `assert!(path.exists(), ...)` で落ちるので、
**黙って古いバイナリを検証する経路は無い**。

## 5. `--all-targets` / `--all-features` は現状維持

`--all-features` は現時点で完全な no-op だが、将来クレートが feature を生やしたときに
lint / test 対象から漏れる方が損なので残す。`--all-targets` も上記のとおり無駄ではない。
再燃を防ぐため `ci.yml` の Build ステップに根拠をコメントとして残す。

## 課金

すべて CI の実測値（ジョブ単位で分に切り上げ）。

| | 週次 |
|---|---|
| 現状 | 62 cold × 5 分 + 175 warm × 3 分 = **835 分** |
| 変更後 | 237 run × 3 分 + 11 merge × 3 分 + cron 3 分 = **747 分** |

**課金は週 88 分減る。** wall clock は PR 初回 run で `test` 177s → 96s、
`clippy` 93s → 45s。

# 不採用案

## リンカを lld / mold に差し替える

warm 状態の Build 58s のうち **41s が codegen + リンク**である（ログ上、最後の
`Compiling`（`orchestrator-cli`）が `05:46:13`、`Finished` が `05:46:54` で、その間に
新しいクレートは 1 つも始まらない）。約 51 バイナリ（8 bin + 25 統合テスト +
18 unit テスト）を 2 コアランナーでリンクしており、一見すると大きな削減余地に見える。

しかし **Rust 1.90.0（2025-09-18）で `x86_64-unknown-linux-gnu` の既定リンカは既に
`rust-lld`** である。[^rust-lld] CI のツールチェーンは 1.97.1、ランナーは x86_64 Linux
なので **CI は今すでに lld でリンクしている**。`-C link-arg=-fuse-ld=lld` は良くて
no-op、悪ければ self-contained linker の既定構成を壊し、`RUSTFLAGS` が変わる分だけ
rust-cache の鍵が全部変わって一度全ジョブが cold になるコストだけを払う。

mold は lld からの伸びしろが GNU ld → lld より遥かに小さいのに、第三者 action の
SHA ピン留めと Renovate 追従対象を 1 つ増やす。**この件におけるデファクトスタンダードは
「既定のまま何もしない」である。**

## clippy と test で 1 つの shared-key を共有する

warm ジョブを 1 本に減らせて課金が浮くが、Decision 1 のとおり **壊れる**。

## PR 側で `save-if: false` にして main のキャッシュのみに一本化する

課金は最安（週 756 分）でキャッシュ容量も 1 GB 未満に落ちるが、`Cargo.lock` を変える
PR（Renovate、release-please）がフォールバック先を失い**毎 run cold のまま**になる。

## coverage を `show-env --sh` パターンで分割する

`cargo-llvm-cov` の README が external tests 向けに明記している手順で、PR の `test`
ジョブと同じ Build/Test 分離の形に揃う。しかしカバレッジ成果物を生むジョブの構造を
変える割にリターンが無い。ネストした `cargo build` は既にビルド済みバイナリに対する
no-op 確認で実時間コストは数秒に過ぎず（`Collect coverage` は 90s、ジョブ全体で
118〜133s）、これは性能問題ではなく記述の整合性問題である。加えて `show-env` が
`RUSTFLAGS` から `-D warnings` を落とさないかの検証が別途必要になる。

## 統合テストバイナリを統合する

25 個の `tests/*.rs` をクレートごとに 1 バイナリへまとめればリンク回数が減り 41s は
確実に縮む。しかし `cargo test --test <name>` の粒度が粗くなり、テスト間のプロセス
分離が失われる（[ADR-0018](/decisions/adr-0018-ci-test-time.md) が cargo-nextest の
プロセス分離を評価しているのとトレードオフ）。テスト配置のリファクタであって
キャッシュの話ではないので分離する。

## Build ステップを削除する

`cargo test --workspace --all-features` は同じ target 集合を（doctest を加えて）
コンパイルするので、削除しても総作業量は変わらず時間が移動するだけ。失うものは 3 つ:
`TEST_SUPPORT_PREBUILT_BINS` が前提とする順序保証（`sibling_bin` は鮮度を検査せず
`path.exists()` しか見ないので、失敗モードはビルドエラーではなく**古いバイナリの黙認**
になる）、ステップ時間による build/test の内訳（ADR-0018 が計測手順として要求）、
`RUSTFLAGS: -D warnings` による fail-fast。

## clippy と test をジョブ統合する

チェックアウト・ツールチェーン・キャッシュ復元/保存の二重払い（1 run あたり約 25s）と
cold 時の依存ビルド二重払いが消え、cold 5 分 → 約 4 分になる。しかし並列でなくなる分
wall clock は悪化し、ADR-0007 / ADR-0018 が意図的に分離した構成（「clippy は
クリティカルパス上に無い」）を覆す。キャッシュ修正だけで課金目標を達成できるため見送る。

# Consequences

- 全 PR run が warm になる。従来は「2 回目以降だけ warm」で、初回 run 分は捨てていた。
- Cargo 系ファイルを触る merge のたびに warm 2 ジョブ（実測 65s + 32s = 課金 3 分）が
  増えるが、PR 側の削減が上回る。
- **キャッシュ鍵が全部変わるため、この変更を入れた直後の 1 回だけ全ジョブが cold になる。**
  既存の open PR は rebase するまで古い鍵のまま化石にフォールバックし続ける。
- 化石キャッシュ（`*-e721b7e7`、2026-07-17 作成）は参照されなくなるが自動削除はされない。
  容量を早く戻したい場合のみ手動で削除する。
- warm ジョブのコマンドは消費側と一致し続けなければならない。`test` の Build や
  `clippy` の実行コマンドを変えたら warm 側も同時に変える。片方だけ変えると
  **CI は緑のまま黙って遅くなる**（キャッシュが効かないだけなので失敗しない）。

# 計測方法

この変更の効果は**原理的にマージ後にしか本番計測できない**。warm ジョブは `push` to
main でしか走らず、PR run（`refs/pull/N/merge`）が読めるのは自分の PR スコープと
base ブランチ（main）だけで、feature ブランチに保存したキャッシュは PR run からは
見えないためである。[^gha-cache-scope]

そこで 2 段階で計測する。

- **段階 1（マージ前）**: warm と `test` / `clippy` を一時的に feature ブランチの
  `push` でも走らせ、同一ブランチスコープ内で連続 push する。**実施済み**:

  | | cold | warm |
  |---|---|---|
  | `test` の `Compiling` | 186 | **11** |
  | `test` の `Build` | 115s | **65s** |
  | `test` のジョブ課金 | 3 分 | **2 分** |
  | `clippy` の `Compiling` / `Checking` | 38 / 149 | **0 / 12** |
  | `clippy` の `Run clippy` | 52s | **22s** |
  | `clippy` のジョブ課金 | 2 分 | **1 分** |

  あわせて `warm-cache.yml` が出力する鍵が `ci.yml` の消費側と 1 バイト違わず
  一致することを CI ログで確認した（機械照合だけで済ませない — Decision 2 の罠を
  一度踏んでいる）。この一時トリガはマージ前に外す。
- **段階 2（マージ後）**: main の warm ワークフロー成功 → main スコープに当日付
  エントリ生成 → 次に開く PR の初回 run が warm であることを確認する。

[ADR-0018](/decisions/adr-0018-ci-test-time.md) の「性能変更は必ず CI 実測で検証する。
ローカル計測は CI の代理にならない」に従う。

[^rust-lld]: Rust 1.90.0 で rust-lld が x86_64-unknown-linux-gnu の既定リンカに
[^gha-cache-scope]: GitHub Actions — Caching dependencies（ブランチスコープの規定）
