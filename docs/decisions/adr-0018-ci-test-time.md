---
type: Decision
title: ADR-0018 CI テスト時間の削減（待ちの構造化・profile・計測の作法）
description: CI の test ジョブ短縮にあたり、テスト用のタイミングノブを型付きの値（RetryPolicy / EngineSettings フィールド）として持たせ、profile はデバッグ情報のみを絞り、性能変更は必ず CI 実測で検証するという決定。依存への opt-level 引き上げと cargo-nextest は計測に基づいて不採用とする。
resource: https://github.com/tomoya-k31/totsuka/issues/281
tags: [ci, cost, testing, performance, build-profile]
timestamp: 2026-07-26T20:00:00+09:00
status: accepted
owner: tomoya-k31
---

# Status

Accepted — 2026-07-26（[#281](https://github.com/tomoya-k31/totsuka/issues/281)。[ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) が**実行タイミング**を再設計したのに続き、本 ADR は**実行時間そのもの**を扱う）

# Context

[ADR-0007](/decisions/adr-0007-ci-cost-optimization.md) は「チェック内容は変えず実行タイミングだけ変える」方針で Actions を約 3,500 分/月 → 約 1,900 分/月に下げた。それでも `test` ジョブは 142〜186 秒（課金 3 分）かかっており、ジョブ単位の切り上げ課金と合わせて PR 1 push あたり 6 分を消費していた。

ステップ単位で実測したところ、**テスト実行 66 秒のうち約 47 秒が実質 `tokio::time::sleep`、またはテスト実行中のネストした `cargo build`** で、計算そのものは 1 秒未満だった。内訳は `cargo test` がテストバイナリを 1 つずつ逐次実行するため、各バイナリの時間がそのまま積み上がる:

| テストバイナリ | 当初 | 原因 |
|---|---|---|
| `agent-ide-herdr/tests/integration.rs` | 31.5s | `agent.rs` の private const が決める待ち時間 |
| `orchestrator-cli/tests/e2e.rs` | 8.7s | `ONE_SHOT_GRACE` 2s × 4 + ネスト `cargo build` |
| `orchestrator-core/tests/run_loop.rs` | 8.4s | 同じ `ONE_SHOT_GRACE`（一回性 run が 4 箇所） |

# Decision

## 1. テスト用のタイミングノブは「型付きの値」として持たせる

`agent.rs` の 5 定数は `pub struct RetryPolicy`（`Default` が本番値）へ、`ONE_SHOT_GRACE` は `EngineSettings.one_shot_grace` へ移す。後者は `worktree_sweep_interval`（[ADR-0010](/decisions/adr-0010-worktree-cleanup-pane-release.md)）が既に確立していた「config には露出せず、テストだけが差し替える `pub` フィールド」パターンの踏襲である。

`Default` が実機検証値と一致することを unit test で固定する。テスト値の編集が本番のタイミングを黙って変えないようにするため。

**テストが縮めるのは待ち時間だけで、回数は本番値のまま維持する。** 諦め系のテストは「本当に 5 回送信し、11 回 Enter を押す」ことを検証しており、回数を縮めるとアサーションの意味が失われる。

不採用にした代替案:

| 案 | 不採用の理由 |
|---|---|
| `#[cfg(test)]` | **正しさの問題で不可**。`tests/*.rs` は別クレートで、`--cfg test` なしでコンパイルされた rlib にリンクするため、`#[cfg(test)]` の項目はそこに存在しない |
| `#[cfg(debug_assertions)]` | debug と release でバイナリの挙動が「実 TUI とのタイミング競合」という当該プラグインの存在意義そのものの次元で変わる |
| cargo feature | CI は `--all-features` を使うため、同じジョブがビルドする出荷用バイナリでも feature が ON になる |
| 環境変数 | edition 2024 の `set_var` は unsafe かつプロセスグローバルで、1 プロセスを共有する並列テストが互いに競合する。加えて [ADR-0009](/decisions/adr-0009-env-override-whitelist.md) のホワイトリストに足すと、内部定数がドキュメント義務のあるユーザー向けノブへ昇格してしまう |

CLI バイナリを起動する E2E だけは構造体を触れないため、`run` に `hide = true` の `--one-shot-grace-ms` を置く。CLI フラグは設定優先順位の第 1 層（ADR-0009）であり、config でも環境変数でもない値の置き場所として最も自然だからである。

## 2. profile はデバッグ情報のみを絞る（依存への `opt-level` 引き上げは不採用）

ワークスペースに `[profile.*]` が無く、`cargo test` が約 50 個のバイナリをリンクするのにフルデバッグ情報を積んでいた。`[profile.dev] debug = "line-tables-only"` と依存の `debug = false` を入れる。

依存への `opt-level = 1` は**計測して却下した**:

| 構成 | cold build | warm 再リンク | `target/` |
|---|---|---|---|
| baseline（`debug=2`, deps `opt-level=0`） | 46s | 7s | 2.5 G |
| ＋ deps `opt-level = 1` | **93s** | 3s | 1.6 G |
| **デバッグ情報のみ（採用）** | **33s** | **2s** | **1.7 G** |

約 200 個の依存を最適化するコストがデバッグ情報の削減を大きく上回り、rust-cache が退避・失効するたびに CI がコールドビルドでそれを払う。`debug-assertions` は触らない（`opt-level` が 0 のままなので削減効果はほぼ無く、テスト対象コードの overflow チェックを黙って無効化してしまう）。`incremental` は `Cargo.toml` に書かない（ローカル開発では有用。必要なら `ci.yml` の env で）。

> **罠**: リンカ変更を検討する場合、`ci.yml` は workflow レベルで `RUSTFLAGS` を設定しており、**env の `RUSTFLAGS` は `.cargo/config.toml` の `rustflags` を完全に上書きする**。`.cargo/config.toml` に書くとローカルでは効くのに CI では黙って無視される。

## 3. テスト実行時にビルドしない

`e2e.rs` / `slack_e2e.rs` は 1 回の `cargo test` で計 15 回 `cargo build` をシェルアウトし、全て同じ target ロックを奪い合っていた。`test_support::sibling_bin` へ集約し、`OnceLock` で 1 プロセス 1 回、CI は `TEST_SUPPORT_PREBUILT_BINS=1` で 0 回にする。

**この env は `TOTSUKA_` 接頭辞を避けなければならない。** `apply_env_overrides` は未知の `TOTSUKA_*` を stderr へ警告する（ADR-0009）ため、E2E が起動する `totsuka` 子プロセスの stderr に警告行が前置され、stderr を JSON エラーエンベロープとしてパースするテストが壊れる。警告は**設定ファイルが存在するときだけ**出るので、設定なしの単体確認では再現しない。

## 4. 性能変更は CI で測り直す（ローカル計測は代理にならない）

本件で 2 度、ローカル計測が CI を誤って予測した。

- herdr の Enter ループ改修はローカル 31.5→15.2s だったが **CI では 31.5→28.5s**。多コア機は libtest が高並列でテストを回すので「削った仕事量」がそのまま実時間になるが、CI は 2 コアでクリティカルパス（15 秒のテスト 2 本）に律速される。
- `cargo test` 100s / `nextest` 7s というローカル A/B の差は大半が doctest で、その 88 秒はローカル `target/` の状態に起因するアーティファクトだった。**CI の doctest は全 11 クレートで約 1.1 秒**しかかかっていない。

したがって、性能を理由とする変更は**キャッシュを温めた 2 回目の CI run** で比較する（profile 変更は rust-cache を全無効化するので 1 回目は cold で比較にならない）。per-binary の内訳はジョブログの `test result: ... finished in Xs` を抽出する。

## 5. cargo-nextest は現時点では不採用

当初は逐次実行の解消に有効と見込んだが、上記 1〜3 で待ち時間そのものが消えた後は、CI 実測で有意な短縮が確認できなかった。導入コスト（CI の install ステップ、設定ファイル、doctest の別ステップ化、ローカル `cargo test` との乖離）に見合わない。

ただし **nextest はプロセス分離により §3 の env 命名バグを検出した**。この種の隠れた結合を洗い出す道具としては有効なので、破棄せず「随時手で走らせる」位置づけとする。将来テストバイナリが増えて逐次コストが再び支配的になったら再検討する。

## 6. ジョブ数とキャッシュ

`cargo-machete` は実働 7 秒でも切り上げで 1 分課金されるため、クリティカルパスでない `clippy` ジョブのステップへ吸収する（wall-clock は変えず課金だけ −1 分/push）。

Actions キャッシュは 10.19 GB と上限 10 GB を超過し常時 LRU 退避が起きていた。PR ごとに約 350 MB を PR スコープ（`refs/pull/<N>/merge`）で作り捨て、他 PR から再利用できないためである。クローズ済み PR のキャッシュを回収する **週次 cron**（`cache-cleanup.yml`）を置く。PR クローズ契機にしないのは課金のため（PR ごとに 1 ジョブ足すと切り上げ 1 分 × PR 数、週次なら月 4 分）。

# Consequences

- `EngineSettings` に新フィールドを足すと、`EngineSettings { .. }` を構造体リテラルで組む結合テスト 3 本がコンパイルエラーになる。これは意図的（新しい待ちノブの既定値をテストが黙って引き継がない）。
- `RetryPolicy` / `one_shot_grace` は `pub` なので、プラグインや埋め込み利用者から見える API になった。`agent-ide-herdr` は `publish = false` なので semver 契約は無い。
- デバッグ情報を絞ったため、ローカルでデバッガの変数情報が必要なときは `cargo build --config 'profile.dev.debug=2'` で一時的に戻す必要がある。バックトレースの file:line は保持される。
- **herdr のリトライ経路は実機検証でしか守れない**。`Default` の unit test は値の取り違えを防ぐが、実 CLI との競合は [リリース前チェックリスト](/quality/release-checklist.md) の herdr 項目に依存する。
- `TEST_SUPPORT_PREBUILT_BINS` を立てたまま `cargo test` を回すと、バイナリが古いまま検証される。CI は必ず直前に `cargo build --workspace --all-targets` を走らせる前提。

# Citations

[1] [Issue #281](https://github.com/tomoya-k31/totsuka/issues/281)
[2] [ADR-0007 CI 実行タイミングの再設計](/decisions/adr-0007-ci-cost-optimization.md)
[3] [ADR-0009 TOTSUKA_* 環境変数オーバーライド](/decisions/adr-0009-env-override-whitelist.md)
[4] [ADR-0010 worktree 掃除と pane 解放](/decisions/adr-0010-worktree-cleanup-pane-release.md)
[5] [テスト戦略](/quality/test-strategy.md)
