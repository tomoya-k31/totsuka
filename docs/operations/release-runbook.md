---
type: Runbook
title: リリース手順（release-please / ユニバーサルバイナリ / GitHub Releases）
description: totsuka のリリース運用。release-please による Release PR、macOS ユニバーサルバイナリの自動ビルドと GitHub Releases 配布、Release PR の CI/ブランチ保護を通すトークン運用（GitHub App / PAT / admin）、Gatekeeper（ad-hoc 署名）の扱い。
resource: https://github.com/tomoya-k31/totsuka/tree/main/.github/workflows
tags: [release, ci, distribution, gatekeeper, semver, github-app, pat, branch-protection]
timestamp: 2026-07-14T05:00:00Z
status: active
owner: tomoya-k31
---

# 前提: リポジトリ設定（1 回だけ）

release-please は自分で Release PR を作るため、リポジトリ設定で GitHub Actions に PR 作成を許可する必要がある（未設定だと `GitHub Actions is not permitted to create or approve pull requests` で失敗する）。

- **Settings → Actions → General → Workflow permissions** で **「Allow GitHub Actions to create and approve pull requests」を有効化**する。
- Organization で管理している場合は Org 側の同名設定も有効にする（Repo 設定はそれを上回れない）。

これはコード（ワークフローの `permissions:`）では代替できないリポジトリ/Org のセキュリティトグル。

# トークン運用（Release PR の CI とブランチ保護）

既定の `GITHUB_TOKEN` で作られた Release PR には**ワークフローが一切走らない**（GitHub の仕様）。そのため必須ステータスチェック（現状 `lint`）が永久に "Expected" になり、ブランチ保護（Ruleset）が **Release PR のマージをブロック**する。回避策は3つ。**org 展開を見据えるなら GitHub App（1）を推奨**。

> **本リポジトリの現状**: 個人リポジトリのため **2（fine-grained PAT）を採用**。secret `RELEASE_PLEASE_TOKEN` を登録済みで、`release-please.yml` の `release-please` ステップに `token:` を配線済み。これにより Release PR に CI（`lint`）が走り、admin 不要で通常マージできる（この構成では「前提: リポジトリ設定」の GITHUB_TOKEN トグルは Release PR 作成には無関係になる）。org へ移す際は 1（GitHub App）へ切り替える。

## 1. GitHub App（org 所有）— 推奨

App が Release PR を作る → 実 identity 扱いなので CI が走り `lint` を満たす → 人が admin 不要で通常マージできる。人に紐づかず、短命トークンで安全。

**セットアップ（管理者の 1 回操作）**

1. **App 作成**: Org Settings → Developer settings → GitHub Apps → New。Permissions は **Repository: Contents = Read and write / Pull requests = Read and write** のみ。Webhook は不要（Active を外す）。
2. **秘密鍵**: App の "Private keys" で 1 つ生成（`.pem` をダウンロード）。App の **App ID** を控える。
3. **install**: 対象リポジトリ（totsuka）に App を Install。
4. **secret 登録**: リポジトリ（または Org）Secrets に `RELEASE_APP_ID`（App ID）と `RELEASE_APP_KEY`（`.pem` の中身）を登録。

**ワークフロー配線**（`release-please.yml` の `release-please` ジョブ冒頭に追加）

```yaml
      - name: Mint app token
        id: app-token
        uses: actions/create-github-app-token@bcd2ba49218906704ab6c1aa796996da409d3eb1 # v3.2.0
        with:
          app-id: ${{ secrets.RELEASE_APP_ID }}
          private-key: ${{ secrets.RELEASE_APP_KEY }}
      - name: Run release-please
        id: release
        uses: googleapis/release-please-action@45996ed1f6d02564a971a2fa1b5860e934307cf7 # v5.0.0
        with:
          token: ${{ steps.app-token.outputs.token }}   # ← 追加
          config-file: release-please-config.json
          manifest-file: .release-please-manifest.json
```

> 秘密鍵は「都度トークンを発行する材料」で、実際に露出するのは毎回発行される短命トークン。長寿命 PAT より安全側。App 名義で監査でき、担当者の異動にも影響されない。

## 2. fine-grained PAT — 簡易

手早いが**個人アカウントに紐づく**（作成者の退職・失効で自動化が止まる）。org では PAT を制限/禁止していることも多い。ボット専用 machine user で緩和できるが、それなら App が素直。

**セットアップ**

1. GitHub → Settings → Developer settings → **Fine-grained tokens** → Generate。**Resource owner = 対象 Org**、Repository access = totsuka のみ、Permissions = **Contents: Read and write / Pull requests: Read and write**。有効期限を設定（要ローテーション）。
2. リポジトリ Secrets に `RELEASE_PLEASE_TOKEN` として登録。

**ワークフロー配線**（`release-please-action` に 1 行）

```yaml
        with:
          token: ${{ secrets.RELEASE_PLEASE_TOKEN }}   # ← 追加
          config-file: release-please-config.json
          manifest-file: .release-please-manifest.json
```

## 3. トークンなし（admin マージ）— 現状

トークン管理を増やさない。Release PR は CI が走らないままなので、リリースごとに `gh pr merge <PR番号> --squash --admin`（Ruleset の bypass_actors に RepositoryRole=Admin が既存）。保護は最強のまま、毎回 admin フラグの手間だけ。リリース頻度が低いなら現実的。

> **非推奨**: Ruleset の bypass_actors に `github-actions[bot]` を足す案は避ける。bypass はマージ実行 actor にしか効かず（人間マージには無関係）、bot に自動マージさせると Release PR のレビュー関門が消える。さらに全ワークフローが main 保護をバイパスできる広い攻撃面になる。

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
