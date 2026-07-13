---
type: Runbook
title: リリース手順（release-please / ユニバーサルバイナリ / GitHub Releases）
description: totsuka のリリース運用。release-please による Release PR、macOS ユニバーサルバイナリの自動ビルドと GitHub Releases 配布、Gatekeeper（ad-hoc 署名）の扱い。
resource: https://github.com/tomoya-k31/totsuka/tree/main/.github/workflows
tags: [release, ci, distribution, gatekeeper, semver]
timestamp: 2026-07-14T04:00:00Z
status: active
owner: tomoya-k31
---

# リリースの流れ

1. **Release PR**: `main` への push ごとに [release-please](https://github.com/googleapis/release-please)（`.github/workflows/release-please.yml`）が Conventional Commits を集計し、SemVer 版と CHANGELOG（Keep a Changelog 形式）を持つ Release PR を作成・更新する。設定は `release-please-config.json` / `.release-please-manifest.json`。
2. **リリース確定**: Release PR をマージすると release-please が `vX.Y.Z` タグと GitHub Release を作成し、`Cargo.toml` の `[workspace.package] version`（`# x-release-please-version` 注釈行）と `CHANGELOG.md` を bump する。
3. **バイナリ添付**: 同じ `release-please.yml` 実行内の `universal-binary` ジョブが（release-please の `release_created` 出力を条件に）起動し、macOS ランナーで `x86_64-apple-darwin` と `aarch64-apple-darwin` をビルド、`lipo` でユニバーサル化、ad-hoc 署名して `totsuka-vX.Y.Z-macos-universal.tar.gz`（+ `.sha256`）を Release に添付する。別ワークフローの `on: release` にしないのは、既定 GITHUB_TOKEN が発行した Release イベントは他ワークフローを起動しないため。ビルドは `--locked` を使わない（release-please は Cargo.toml の版だけ bump し Cargo.lock は更新しないため、初回ビルドでロックを再生成させる）。

> プラグインプロトコルの版はアプリ本体と独立（#50）。totsuka のリリースはプロトコル版の変更を意味しない。CHANGELOG に破壊的プロトコル変更を書く場合は明示する。

# バージョニング（SemVer）

- Conventional Commits の `feat` → minor、`fix` → patch、`feat!`/`BREAKING CHANGE` → major
- v1（0.x）系では `bump-minor-pre-major` により破壊的変更も minor に留める設定

# 配布（GitHub Releases）

- 配布経路は **GitHub Releases のユニバーサルバイナリ** と `cargo install --git ... orchestrator-cli`（README に併記）。パッケージマネージャ（Homebrew 等）は v1 では扱わない。
- 各 Release には `totsuka-vX.Y.Z-macos-universal.tar.gz` と生の SHA-256（`.sha256`）が添付される。利用者は tarball を展開して `totsuka` を PATH に置く。

# Gatekeeper（macOS）

- v1 は **ad-hoc 署名**（`codesign --sign -`）。初回起動で Gatekeeper に阻まれた場合、利用者は quarantine 属性を除去する: `xattr -d com.apple.quarantine /usr/local/bin/totsuka`。
- Developer ID 署名 / notarization は Open Question #5（社外公開判断）の決定後に対応する。決定したら本 runbook と `release-please.yml` の `universal-binary` ジョブの署名ステップを更新する。

# ロールバック

- 問題のあるリリースは GitHub 上で該当 Release/タグを削除するか、修正版を通常のリリースフローで前に進める。
- `main` が壊れた場合は PR 規約に従い revert 優先（`type: revert`、元コミットハッシュと理由を body に）。

# 事前確認

タグを切る前に [リリース前手動チェックリスト](/quality/release-checklist.md)（herdr/orca 実機・通知・回復）を実施する。テスト戦略は [テスト戦略](/quality/test-strategy.md)。
