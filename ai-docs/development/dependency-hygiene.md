---
type: Guide
title: 依存関係ハイジーン（未使用依存と Cargo.lock ドリフトの検出）
description: cargo-machete による毎 PR の未使用依存チェックの運用、誤検知の抑制手順（package.metadata.cargo-machete）、高精度な cargo-shear / cargo-udeps の定期手動実行手順、および cargo metadata --locked による Cargo.lock ドリフト検出（宣言はあるが lock に無い、という逆方向のドリフト）。
resource: https://github.com/tomoya-k31/totsuka/blob/main/.github/workflows/ci.yml
tags: [rust, ci, dependencies, cargo-machete, cargo-shear, cargo-udeps, cargo-lock, drift]
generated: { by: human:tomoya-k31, at: 2026-07-26T23:30:00+09:00 }
status: stable
owner: tomoya-k31
---

# 背景（#171）

ワークスペースは `[workspace.dependencies]` に依存を集約し、各クレートが必要な feature だけを厳選する運用だが、リファクタで参照が消えた依存は静かに残り続け、ビルド時間・監査対象（cargo audit / deny）・サプライチェーン面積を無駄に増やす。このドリフトを検知する 2 層のガードを置く。

# 第1層: cargo-machete（毎 PR、CI 常設）

`.github/workflows/ci.yml` の **`clippy / rustfmt` ジョブのステップ**として毎 PR で実行する。machete はテキストレベルのスキャンでコンパイル不要（Rust toolchain も不要）なため数秒で終わる。未使用依存を混入させた PR は CI が fail する。

> 0.2.4 まで（#171）は `ubuntu-slim` の独立 `machete` ジョブだったが、Actions がジョブ単位で 1 分未満を切り上げ課金するため、実働 7 秒でも丸 1 分の固定費になっていた。`clippy` ジョブは 35〜45 秒でクリティカルパス（`test` は 100 秒前後）でもないので、[ADR-0018](/decisions/adr-0018-ci-test-time.md) でステップとして吸収した — wall-clock は変わらず課金だけ 1 分減る。**CI ログで machete の失敗を探すときは `clippy / rustfmt` ジョブを見ること。**

ローカルでの事前確認:

```bash
cargo install cargo-machete   # 初回のみ
cargo machete                 # ワークスペースルートで実行
```

## 誤検知の抑制手順

machete はテキストスキャンゆえ、マクロ経由の使用・再エクスポート・`#[doc]` 参照などで**誤検知（false positive）**が出うる。誤検知と判断した場合は、該当クレートの `Cargo.toml` に **理由コメント付きで** ignore を追加する:

```toml
[package.metadata.cargo-machete]
# serde_derive はマクロ展開でのみ参照されるため machete が検知できない
ignored = ["serde_derive"]
```

理由コメントのない ignore を追加しない（レビューで「本当に使っているのか」を判断できなくなるため）。依存を削除した際に ignore が残っていないかも合わせて確認する。

# Cargo.lock のドリフト検出（#290）

上の 2 層が見るのは「`Cargo.toml` に宣言があるが**使われていない**」方向のドリフト。**逆方向** — 「`Cargo.toml` に宣言があるが `Cargo.lock` に反映されていない」 — は別のガードが要る。

## 何が起きていたか

PR #283 が `crates/test-support/Cargo.toml` に `serde_json` を足したが `Cargo.lock` を再生成せずにマージされ、`main` の lock に `test-support` の `dependencies` エントリが無い状態が残った。症状は「**checkout して `cargo` を何か走らせるだけで `Cargo.lock` が dirty になる**」で、無関係な差分が毎回作業ツリーに現れる。

**CI はこれを検出できなかった。** どのワークフローも `--locked` / `--frozen` を使っておらず、`cargo` は lock の更新が必要なら**黙って再生成して成功する**:

```console
$ cargo metadata --format-version 1 > /dev/null   # --locked なし
（成功）
$ git diff --stat Cargo.lock
 Cargo.lock | 1 +
```

ローカルでも同じことが起きるので、`git status` を見ない限り誰も気づかない。[#240 の rustdoc ギャップ](https://github.com/tomoya-k31/totsuka/issues/240)と同型で、「壊れていても何も鳴らないので `main` に静かに溜まる」。

## ガード

`.github/workflows/ci.yml` の **`clippy / rustfmt` ジョブのステップ**として毎 PR で実行する:

```yaml
- name: Cargo.lock is in sync
  run: cargo metadata --locked --format-version 1 > /dev/null
```

`--locked` は「lock の更新が必要なら**エラーで止まる**」ので、これだけでドリフトを PR で捕まえられる:

```text
error: cannot update the lock file /.../Cargo.lock because --locked was passed to prevent this
```

ビルドを伴わないため数秒で終わる（`scripts/arch-lint.sh` が既に `cargo metadata --no-deps` を使っている前例がある。ドリフト検出は依存解決が要るので `--no-deps` は使えない）。

**キャッシュ復元より前に置く。** `Swatinem/rust-cache` は `Cargo.lock` をキャッシュキーに含むため、ドリフトしたままのキーでエントリを作らせない。代償として登録レジストリ索引の取得がキャッシュに乗らないが、`clippy` ジョブはクリティカルパスではない（[ADR-0018](/decisions/adr-0018-ci-test-time.md)）。

## ビルド/テストは `--locked` にしない

より厳格な `cargo build --locked` / `cargo test --locked` は**採らない**。CI が lock を直せなくなり、依存更新 PR（Renovate）や release-please の `sync-lockfile` ジョブとの相互作用が変わる。`release-please.yml` は「stray lock drift がリリースビルドを止めないよう」**意図的に `--locked` を外している**。ここは**検出専用**に留める。

## `--all-features` は要らない

clippy / test は `--all-features` を付けているのに、このステップは付けていない。**`Cargo.lock` の解決は feature フラグに非依存**（潜在的な依存グラフ全体をロックする）なので、optional / feature ゲート付きの依存を足して lock を再生成しなかった場合も、`--all-features` の有無に関わらず同じように `--locked` が落ちる。実際に検証済み。

なお、**逆方向のドリフト**（lock に使われていない古いエントリが残っている）は `--locked` では落ちない。cargo はそれをエラーにしないため。本ステップの対象は「宣言があるのに lock に無い」方向だけである。

## release-please の Release PR では一時的に赤くなる（仕様どおり）

`release-please` は `Cargo.toml` のバージョンを上げるが `Cargo.lock` は 1 バージョン遅れたままにする（`release-please.yml` にその旨のコメントがある）。したがって **Release PR の最初の push では本ステップが落ちる**。

これは誤検出ではなく**正しい検出**で、`sync-lockfile` ジョブの追従コミットが次の run で解消する。加えて `clippy / rustfmt` は**必須チェックではない**（ruleset が要求するのは `okf-lint` の `lint` のみ）ため、リリースがブロックされることはない。

Renovate の PR は `Cargo.toml` と `Cargo.lock` を同時に更新するので影響を受けない。

## 罠

- **workspace member 間の依存追加は見落としやすい。** 外部 crate の追加は `Cargo.lock` に大きな差分を生むので気づくが、#283 のケース（`test-support` への依存追加）は **3 行しか動かない**。
- `cargo audit` / `cargo deny` は `Cargo.lock` を入力にする。lock が実際の依存グラフとずれていれば**監査対象もずれる**。今回は workspace 内 crate なので実害はなかったが、外部依存で同じことが起きれば脆弱性を見落とす。
- `cargo build --locked` を使う経路（再現ビルド・SBOM 生成・オフラインビルド・vendoring）が入った瞬間に壊れる。

# 第2層: cargo-shear / cargo-udeps（定期の手動実行）

machete より精度の高いツールを、四半期に 1 回程度（または大きなリファクタ・依存整理の後）に手動で実行し、machete がすり抜けた未使用依存・未使用 feature を拾う。

## cargo-shear（推奨: stable で動く）

```bash
cargo install cargo-shear   # 初回のみ
cargo shear                 # ワークスペースルートで実行
```

- マクロ展開を考慮したより精度の高い検出を行う。誤検知の抑制は machete と同様に `[package.metadata.cargo-shear] ignored = [...]`（理由コメント付き）。
- 検出された未使用依存は通常 PR（`chore(deps): ...`）として削除する。

## cargo-udeps（代替: nightly が必要）

```bash
cargo install cargo-udeps                 # 初回のみ
cargo +nightly udeps --workspace --all-targets --all-features
```

- 実際にコンパイルして判定するため最も精度が高いが、nightly toolchain が必要でビルド時間もかかる。shear で不十分な場合の精査用。

## 手動実行の記録

手動実行で削除・ignore 追加を行った PR には、実行したツールとバージョンを PR 本文に記録する（次回実行時の基準点になる）。

# 関連

- [ADR-0002 Rust workspace 構成と CI 品質ゲート](/decisions/adr-0002-rust-workspace-ci.md)
- [ADR-0018 CI テスト時間の削減](/decisions/adr-0018-ci-test-time.md)（machete のジョブ→ステップ統合）
- CI 定義: `.github/workflows/ci.yml`（`clippy / rustfmt` ジョブ内の `Run cargo-machete` ステップ）、`.github/workflows/audit.yml`（cargo-audit / cargo-deny）
