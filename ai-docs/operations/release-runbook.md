---
type: Runbook
title: リリース手順（release-please / ユニバーサルバイナリ / GitHub Releases）
description: "totsuka のリリース運用。release-please による Release PR、macOS ユニバーサルバイナリと同梱プラグインの自動ビルド・署名・GitHub Releases 配布、リリースごとの Homebrew tap 自動 bump と 2 本のトークン運用、Release PR の CI/ブランチ保護を通すトークン運用（GitHub App / PAT / admin）、Gatekeeper（ad-hoc 署名）の扱い。"
resource: https://github.com/tomoya-k31/totsuka/tree/main/.github/workflows
tags: [release, ci, distribution, homebrew, gatekeeper, semver, github-app, pat, branch-protection]
generated: { by: claude-code/opus-5, at: 2026-08-22T00:00:00Z }
status: stable
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

### セットアップ（管理者の 1 回操作）

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

### セットアップ

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
3. **バイナリ添付**: 同じ `release-please.yml` 実行内の `universal-binary` ジョブが（release-please の `release_created` 出力を条件に）起動し、macOS ランナーで `x86_64-apple-darwin` と `aarch64-apple-darwin` を **`--workspace --bins` で**ビルド、`lipo` でユニバーサル化、**本体と同梱プラグインの全バイナリに** ad-hoc 署名して `totsuka-vX.Y.Z-macos-universal.tar.gz`（+ `.sha256`）を Release に添付する。別ワークフローの `on: release` にしないのは、既定 GITHUB_TOKEN が発行した Release イベントは他ワークフローを起動しないため。ビルドは `--locked` を使わない（release-please は Cargo.toml の版だけ bump し Cargo.lock は更新しないため、初回ビルドでロックを再生成させる）。

   同梱するプラグイン名は `plugins/*/plugin.toml` の `name` を舐めて決めるので、プラグインを追加してもワークフローの編集は要らない。ビルド成果物名がそのまま配布名になるのは [ADR-0027](/decisions/adr-0027-plugin-artifact-naming.md) の不変条件（bin 名 = `plugin.toml` の `name`）が `scripts/arch-lint.sh` で担保されているため。

   **プラグインにも署名すること。** 本体だけ署名すると、プラグインが Gatekeeper に殺されて `totsuka doctor` は「crashed or exited」としか言えない（原因が署名だと分からない）。

> プラグインプロトコルの版はアプリ本体と独立（#50）。totsuka のリリースはプロトコル版の変更を意味しない。CHANGELOG に破壊的プロトコル変更を書く場合は明示する。

# バージョニング（SemVer）

- Conventional Commits の `feat` → minor、`fix` → patch、`feat!`/`BREAKING CHANGE` → major
- v1（0.x）系では `bump-minor-pre-major` により破壊的変更も minor に留める設定

# 配布（GitHub Releases）

- 配布経路は **GitHub Releases の tarball（ユニバーサルバイナリ + 同梱プラグイン）**、`cargo install --git ... orchestrator-cli`（README に併記。CLI のみでプラグインは付かない）、そして **Homebrew tap**（[ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md)）。
  かつてここには「パッケージマネージャ（Homebrew 等）は v1 では扱わない」と書いてあった。**その判断は ADR-0053 で覆っている**（配布層の摩擦がインストール全体の最初の関門で、5 コマンドの手配置と更新手段の不在が実際に古いバイナリを放置させたため）。ただし tap が実際に効くのは本リポジトリが public になってからで、それまでは bump ステップが可視性ゲートで止まっている。
- **「バイナリを配る」ではなく「ツリーを配る」。** 単一バイナリを置けばよいと読めると、利用者が `totsuka` だけを移して同梱プラグインを置き去りにする。本 runbook でも README でも配布物は tarball と呼ぶ。
- 各 Release には `totsuka-vX.Y.Z-macos-universal.tar.gz` と生の SHA-256（`.sha256`）が添付される。**成果物のファイル名と `.sha256` サイドカーの形式は変えない**（ファイル名で取得している自動化を壊さないため）。
- tarball はプレフィックス付きディレクトリ構成で、`totsuka` の隣に同梱プラグインが並ぶ:

  ```text
  totsuka-vX.Y.Z-macos-universal/
  ├── totsuka
  ├── plugins/<name>/{<name>, plugin.toml}
  ├── README.md
  └── LICENSE
  ```

  利用者はツリーごと `/usr/local/lib/totsuka` へ置き、`/usr/local/bin` から symlink する（README のインストール手順）。バイナリだけを移すと同梱プラグインが置き去りになる。プラグインは `totsuka plugin install --bundled <name>` で入れる（#345）。

  > **`--bundled` の探索は symlink 先も見る。** `std::env::current_exe` は macOS で symlink を解決しない（`_NSGetExecutablePath` は起動に使われたパスを返す）ため、CLI は `fs::canonicalize` の結果も明示的に探索する。上記のインストール形はプラグインが**リンク先**の隣にあるので、これが無いと 1 つも見つからない。詳細は [orchestrator-cli](/components/orchestrator-cli.md)。
- **スモークテスト**: 添付の直前に、展開した tarball からスクラッチな XDG 環境へ全プラグインを install し、`plugin list --json` の件数が `plugins/*/plugin.toml` の数と一致することを検証する。**利用者が実際にダウンロードする成果物に対して実行する**ので、ドキュメントの約束が嘘になっていないことをここで担保できる（かつては README が存在しないディレクトリを指していて必ず失敗する状態が放置されていた）。

# Homebrew tap

`Formula/totsuka.rb` は `tomoya-k31/homebrew-tap` にあり、**リリースごとに自動で bump される**。`universal-binary` ジョブの最終ステップが、アセットを Release へ添付した直後に `version` と `sha256` の 2 行だけを書き換えて push する。

運用の詳細（レイアウトがなぜ `bundled.rs` の探索順と一致するのか、手で formula を編集するときの注意、public 化後にやること）は [Homebrew tap](/infrastructure/homebrew-tap.md)。

## トークン

| secret | スコープ | 用途 | 失効日 |
|---|---|---|---|
| `RELEASE_PLEASE_TOKEN` | `totsuka` のみ / Contents + Pull requests: RW | Release PR を実 identity で作り CI を走らせる | （記録なし） |
| `HOMEBREW_TAP_TOKEN` | `homebrew-tap` のみ / Contents: RW | tap へ formula の bump を push する | **2026-09-30**（2026-08-31 発行 / 30 日） |

**2 本を兼用しない。** `RELEASE_PLEASE_TOKEN` を tap まで届くよう広げると、リリーストークンの爆発半径とローテーション周期が tap に結合する。

`HOMEBREW_TAP_TOKEN` が失効すると **リリース run が赤くなる**（タグ・Release・アセットは既に公開済みで無事）。発行し直したら失効日を上の表に書くこと。

**落ちるのは `git clone` ではなく `git push`。** tap 自体が public なので、トークンが空でも失効していても **clone は成功する**（実測: `git ls-remote 'https://x-access-token:@github.com/tomoya-k31/homebrew-tap.git'` は rc=0）。`sed` も直後の `grep -q` の表明も通り、最後の push で初めて認証が要る。赤くなった run の原因を clone のログに探しても何も無い。

**失効日が短いことを前提に運用する。** 現行の 30 日はリリース間隔より短くなりうるので、**失効後の最初のリリースは高い確率で赤くなる**。上の表の日付を過ぎていたら、リリース前に PAT を再発行して `HOMEBREW_TAP_TOKEN` を更新すること。

**復旧は job の再実行ではない。** 再実行すると tarball が作り直され、`tar`/`gzip` が mtime を埋め込むためバイト同一にならず、`--clobber` が公開済みアセットを別の sha256 のものへ差し替えてしまう。 tap の `Formula/totsuka.rb` を公開済みアセットの値に手で合わせて push すること。

## 可視性ゲート（2026-08-31 に発火済み）

Homebrew の formula は `url` を**素の `curl`（GitHub 認証なし）**で取る。本リポジトリが private である間、リリースアセットの URL は未認証では 404 になり、**tap 経路は動かなかった**。

そのため bump ステップは `if: ${{ !github.event.repository.private }}` でゲートしてある。**2026-08-31 の public 化で自分で有効になった**（未認証 `curl` が 200 を返すことを実測済み）。外す作業は無かった。

**ゲートの副作用が 1 度出た。** v0.6.0（2026-08-29）は private 中のリリースだったため bump がスキップされ、formula が v0.5.0 に取り残された。v0.5.0 のアセットは実在するので `brew install` は**成功したうえで古いものを入れる**。public 化に合わせて手で 0.6.0 へ合わせた（[homebrew-tap#1](https://github.com/tomoya-k31/homebrew-tap/pull/1)）。**private のままリリースを重ねると、そのぶん静かに開く差**なので、次に可視性を戻すことがあれば同じ手当てが要る。

シークレットの有無でゲートしていないのは意図的で、そちらは危険を読み違える（失効トークンは素通りする一方、未登録のシークレットが毎リリース緑で skip され tap が永久に遅れる）。詳細は [Homebrew tap](/infrastructure/homebrew-tap.md)。

# Gatekeeper（macOS）

- v1 は **ad-hoc 署名**（`codesign --sign -`）。本体だけでなく**同梱プラグインの全バイナリ**に打つ。初回起動で Gatekeeper に阻まれた場合、利用者はツリー全体の quarantine 属性を除去する: `xattr -dr com.apple.quarantine /usr/local/lib/totsuka`。
- Developer ID 署名 / notarization は Open Question #5（社外公開判断）の決定後に対応する。決定したら本 runbook と `release-please.yml` の `universal-binary` ジョブの署名ステップを更新する。

# ロールバック

- 問題のあるリリースは GitHub 上で該当 Release/タグを削除するか、修正版を通常のリリースフローで前に進める。
- `main` が壊れた場合は PR 規約に従い revert 優先（`type: revert`、元コミットハッシュと理由を body に）。

# 事前確認

タグを切る前に [リリース前手動チェックリスト](/quality/release-checklist.md)（herdr/orca 実機・通知・回復）を実施する。テスト戦略は [テスト戦略](/quality/test-strategy.md)。
