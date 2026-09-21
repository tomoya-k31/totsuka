---
type: Decision
title: ADR-0087 MSRV（rust-version）を CI の msrv ジョブで検査する
description: "CI が stable でしかビルドせず rust-version = 1.88 が宣言でしかなかったため、Renovate の導入に先立って、その版で workspace とゲートウェイを cargo check する msrv ジョブを ci.yml に足した決定。版は Cargo.toml の rust-version から読み、test は回さず、キャッシュは専用の shared-key で warm-cache には足さない。"
tags: [decision, ci, msrv, rust, renovate, adr]
generated: { by: claude-code/opus-5, at: 2026-09-21T16:00:00+09:00 }
status: stable
owner: tomoya-k31
---

# Status

stable。#728（Renovate 導入）の 1 本目の PR。

# Context

workspace とゲートウェイ（`services/slack-event-gateway`）はどちらも `rust-version = "1.88"` を宣言しているが、CI の Rust ジョブ（`clippy / rustfmt` / `test` / `gateway`）はすべて `stable` でビルドする。MSRV を検査するものはどこにも無く、宣言が本当かは誰も知らない状態だった。

#728 で Renovate を入れると依存が自動で上がり、patch は automerge される。そのままでは、MSRV を超える依存が緑の CI を通って黙って入る。Renovate 側にも `constraints` を置く予定だが、それは PR を出させない工夫であって、守られていることの保証ではない。

実測（2026-09-21, Rust 1.88.0）:

- `cargo check --workspace --all-targets` もゲートウェイの `cargo check --all-targets` も通る。lock に入っている依存が宣言する `rust_version` の最大値も 1.88（`time 0.3.47` ほか）
- 1.89 で安定化した `File::lock` を一時的に使うと `E0658 use of unstable library feature file_lock` で赤になる（このジョブが実際に止めることの確認）

# Decision

1. **`ci.yml` に `msrv` ジョブを足す。** PR のときだけ走り、`cargo check --workspace --all-targets` とゲートウェイの `cargo check --all-targets` を MSRV の toolchain で実行する
2. **版は root `Cargo.toml` の `rust-version` から読む。** ジョブに版を書かない。`RUSTUP_TOOLCHAIN` で `rust-toolchain.toml`（`channel = "stable"`）を上書きする
3. **check のみで test は回さない。** MSRV で割れるのは「その版の API / 言語機能でコンパイルが通るか」で、テストを回しても分かることは増えない
4. **キャッシュは専用の `shared-key`（`msrv-<Cargo.toml のハッシュ>`）で持ち、`warm-cache.yml` には足さない。** PR ごとに毎回走るジョブなので同じ PR の 2 回目以降に効かせる価値はあるが、古い rustc の成果物は stable のキャッシュと共有できない
5. **1.88 で通らなくなったら、コードを古い Rust に合わせず `rust-version` を上げる。** totsuka は stable でしか使わないツールで、古い toolchain に合わせる理由が無い

必須チェックにはしない（ruleset が要求するのは `lint` のみで、変えない）。Renovate の automerge はブランチの全ステータスを待つので、必須でなくてもゲートとして効く。

# Consequences

- MSRV を上げるときは `rust-version` を書き換えるだけで CI が追従する。ただし Renovate の `constraints.rust`（#728 の 2 本目の PR で入る）にも同じ版が書かれるので、そこは同じ PR で直す
- ゲートウェイが自分の `rust-version` を root より上げた場合、このジョブは root の版でゲートウェイを check するので cargo が「requires rustc X」で落とす。そのときはジョブをゲートウェイ側の版で走らせるよう分ける
- warm-cache に足さないので、main スコープの `msrv-*` エントリは作られない。**各 PR の初回 run は cold**（依存込みの `cargo check`）で、同じ PR の 2 回目以降だけが温まる。check は codegen をしないので cold でも軽い
- `RUSTFLAGS: -D warnings`（`ci.yml` の env）はこのジョブにも効く。古い rustc だけが出す警告があれば赤になる

# 不採用案

- **Renovate の `constraints` だけにする**: PR を出させない工夫で、手で足した依存や `lockFileMaintenance` の推移的更新は素通りする。検査が無い限り MSRV は宣言でしかない
- **MSRV で test まで回す**: CI 時間が増えるだけで、コンパイル可否以上のことはほぼ分からない
- **版をジョブに直書きする**: MSRV を上げるたびに宣言と CI がずれる
- **`dtolnay/rust-toolchain` に版を渡す**: 版を `Cargo.toml` から読むには前段のステップが要り、runner に入っている `rustup` を直接叩くのと手数が変わらない。action を 1 箇所増やさずに済む
- **warm-cache に msrv レグを足す**: main への push ごとに 1 レグ増える。cold でも check は軽いので、その費用に見合わない
