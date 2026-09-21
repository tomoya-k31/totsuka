---
type: Decision
title: ADR-0088 Renovate を Mend の GitHub App で入れ、automerge を Renovate 自身に持たせる
description: "規約と ADR が前提にしていたのに実体が無かった Renovate を、Mend ホストの GitHub App と .github/renovate.json5 で導入した決定。必須チェックが lint だけの ruleset では GitHub の auto-merge が赤の PR を通すので automerge は Renovate 自身が全ステータスを待って行い、対象は patch・Actions の非 major・Docker digest・lockFileMaintenance に限り、PR の CI が実行しない release-please.yml の 4 action は人間レビューに残す。コミット type（chore / security は fix）、ブランチ名とラベル、週次スケジュールとグルーピング、minimumReleaseAge 3 日、Cargo の update-lockfile、MSRV の constraints、Dockerfile の tag@digest 化を記録する。"
tags: [decision, ci, dependencies, renovate, release-please, automerge, adr]
generated: { by: claude-code/opus-5, at: 2026-09-21T21:40:00+09:00 }
status: stable
owner: tomoya-k31
sources:
  - id: issue-728
    resource: https://github.com/tomoya-k31/totsuka/issues/728
    title: "chore(ci): Renovate を導入し、SHA ピン・release-please・必須チェック 1 本の ruleset に合わせて設定する（設計の grilling Q1〜Q22 を含む）"
  - id: renovate-source
    resource: "renovate 42.99.0 の配布物（npm）"
    title: "Renovate の実装（generate.js の automerge 合成、vulnerability.js の force、crate datasource の rust_version、docker datasource の releaseTimestamp）"
  - id: renovate-config-manager
    resource: "renovate 44.105.4 の配布物（npm）dist/modules/manager/renovate-config/extract.js"
    title: "Renovate の renovate-config manager（constraints のツール名を depType tool-constraint の依存として登録する）"
  - id: dashboard-749
    resource: https://github.com/tomoya-k31/totsuka/issues/749
    title: Renovate Dashboard 🤖（App 導入直後の Dependency Dashboard）
---

# Status

stable。#728 の 2 本目の PR。1 本目は MSRV ゲート（[ADR-0087](/decisions/adr-0087-msrv-gate.md)）。

**実 PR での確認はマージ後に行う**（下の「マージ後に確かめること」）。この ADR の設定は `renovate-config-validator --strict` を通してあり、挙動の要点は Renovate 42.99.0 のソースで確かめた。App が実際に出す PR はまだ 1 本も見ていない。導入直後の Dependency Dashboard（#749）は確認済みで、そこで見つかった `constraints.rust` の追従を止めた（下の Consequences）。

# Context

`pr-conventions` の「Dependency update PRs (Renovate)」節、[依存関係ハイジーン](/development/dependency-hygiene.md)、[ADR-0029](/decisions/adr-0029-ci-cache-lifecycle.md) と [ADR-0077](/decisions/adr-0077-setup-writes-the-whole-surface.md) の却下理由、`ci.yml` のコメントは、どれも Renovate が動いている前提で書かれていた。しかし設定ファイルも App も無く、手で追う依存はずれていた（`taiki-e/install-action` の v2.83.1 と v2.83.2 が同居していた）。[^issue-728]

追跡対象は 5 系統ある。

1. workspace の `Cargo.toml` / `Cargo.lock`
2. workspace から exclude されたゲートウェイの `Cargo.lock`
3. GitHub Actions（SHA ピン + `# vX.Y.Z` コメント）
4. ゲートウェイの Dockerfile のベースイメージ 2 つ
5. OpenTofu の `hashicorp/google` provider

4 は distroless なので、イメージの中に CVE を当てる先が無い。パッチの手段はこのビルドだけである。

この repo 固有の制約が 3 つある。

- ruleset `main-required-checks` の必須チェックは `lint` だけで、それはほぼ全 PR で緑になる
- release-please は `chore` を CHANGELOG から隠し、`fix` で patch リリースを刻む（`bump-patch-for-minor-pre-major`）
- `Cargo.lock` を変える PR は rust-cache の鍵を変え、cold build になる（ADR-0029）

# Decision

設定は `.github/renovate.json5` の 1 本に置く。各ルールの理由はそのファイルのコメントにも書いてある。

1. **ホスティングは Mend ホストの GitHub App。** public repo なので無料で使える。workflow を増やさない
2. **automerge は Renovate 自身が行う**（`platformAutomerge: false`、`automergeType: "pr"`、`automergeStrategy: "squash"`）。Renovate はブランチの全ステータスが緑になるのを待つ。GitHub ネイティブの auto-merge は必須チェック（`lint` だけ）しか待たないので、`clippy / rustfmt` / `test` / `msrv` が赤でもマージしうる。dev-flow が `gh pr merge --auto` を禁じているのと同じ理由である
3. **automerge する対象**は、すべての manager の patch、Actions の minor・patch・digest、Docker の digest、週次の `lockFileMaintenance` に限る。major と Cargo の minor は人間がレビューする
4. **`release-please.yml` にだけ出る 4 action は automerge しない。** 対象は `googleapis/release-please-action`、`docker/setup-buildx-action`、`docker/login-action`、`docker/build-push-action`。PR の CI はこれらを実行しないので、緑の CI は何も証明しない。壊れた更新は次のリリースで初めて落ちる。これらは別グループにまとめて、automerge されるグループに混ざらないようにする
5. **コミットの type**: 通常の更新は `chore(deps)`、脆弱性の更新は `fix(deps)` にする。`config:recommended` は Cargo の `dependencies` を `fix` にするので、全パッケージを `chore` に戻すルールを置く。脆弱性の PR は `vulnerabilityAlerts` の `force` で `fix` に戻る。`osvVulnerabilityAlerts`（RustSec を含む OSV）も有効にする
6. **ブランチ名は既定の `renovate/` のまま**にし、git-conventions に bot のブランチの例外として書く。release-please の `release-please--branches--main` も同じ例外で扱う
7. **ラベル**: 全 PR に `renovate` を付ける。あわせて更新種別の `renovate:major|minor|patch|digest|lockfile|security` と、manager の `renovate:cargo|github-actions|dockerfile|terraform` を付ける
8. **PR を出す時間帯と数**: `before 6am on monday`（Asia/Tokyo）、`prConcurrentLimit: 5`、`prHourlyLimit: 2`。Cargo の patch は `cargo patch` の 1 グループにまとめる。Actions の非 major は（4 action を除いて）1 グループにまとめる。脆弱性の PR は時間帯も待ち期間も無視して即座に出る（`vulnerabilityAlerts` の既定）
9. **`minimumReleaseAge: "3 days"`。** automerge を入れる以上、yank された版や乗っ取られた版を取り込まないための猶予が要る。ただし Docker と Actions は `minimumReleaseAgeBehaviour: "timestamp-optional"` にする（下の Consequences を参照）
10. **Cargo の `rangeStrategy` は `update-lockfile`。** 宣言の範囲内の更新は `Cargo.lock` だけを変える
11. **MSRV**: `constraints: { rust: "1.88.0" }` と `constraintsFiltering: "strict"` を入れる。crate datasource は crates.io index の `rust_version` を `constraints.rust` として読むので、MSRV を超える版を候補から外せる[^renovate-source]。ゲートは [ADR-0087](/decisions/adr-0087-msrv-gate.md) の `msrv` ジョブで、こちらは赤になることが確実な PR を出さないための工夫である
12. **Dockerfile の `FROM` を `タグ@ダイジェスト` の形に書き換える。** 以前はタグがコメント行にしか無く、Renovate が現在の版を特定できなかった
13. **Terraform の `required_version` は追わない。** スタックは OpenTofu で動くのに、Renovate は hashicorp/terraform のリリースと照合するからである。provider（`hashicorp/google`）は追う
14. **Dependency Dashboard を有効にする。** タイトルは `Renovate Dashboard 🤖`、ラベルは `renovate`
15. **PR 本文はテンプレートに合わせない。** pr-conventions に「bot の PR はテンプレート対象外」と書く
16. **dev-flow の適用範囲**: automerge される PR にはどの手順も適用しない（CI だけがゲート）。手動でマージする PR は CI と Copilot の指摘の評価を必須にし、`/code-review` は省略する
17. **`audit.yml` は残す。** `audit.yml` は今ある lock を毎日監査し、Renovate は上げる PR を出すので、役割が違う

# Consequences

- **Renovate の automerge は「全ステータスが緑」で決まる。** 必須チェックではないジョブ（`msrv` を含む）もゲートとして効く。逆に、PR の CI に載っていないもの（4 action がその例）はゲートされない。新しい workflow に action を足すときは、それが PR 上で実行されるかを確かめ、されないなら 4 action のリストに加える
- **グループの automerge は「全 upgrade が automerge」のときだけ成立する**（`config.automerge = upgrades.every(u => u.automerge)`）[^renovate-source]。automerge しない upgrade が 1 つでも混ざると、グループ全体が人間待ちになる。4 action を別グループに分けたのはこのためである
- **`lockFileMaintenance` は `cargo patch` グループとは別の PR になる。** lock を書き換える PR は週に最大 2 本（`cargo patch` と lockFileMaintenance）になる。lockFileMaintenance は `cargo update` 相当なので、範囲内の minor も含めて最新まで上げ、`minimumReleaseAge` も効かない。範囲内の minor は semver 互換という Cargo の約束に乗っている
- **`minimumReleaseAge` はリリース時刻を要求する。** 既定（`timestamp-required`）のままだと、時刻の取れない更新は永久に出ない。docker datasource が時刻を持つのは Docker Hub だけなので[^renovate-source]、gcr.io の distroless は黙って止まってしまう。そこで Docker と Actions だけ `timestamp-optional` にした。代償として、時刻の取れない更新には 3 日の猶予が効かない
- **MSRV の版は 2 箇所に書かれる**（`Cargo.toml` の `rust-version` と `constraints.rust`）。上げるときは同じ PR で両方を直す。**`constraints.rust` は必ず `x.y.z` の 3 要素で書く。** strict filtering はリリースの `rust_version` を範囲として `matches(<設定値>, <rust_version>)` で判定し、cargo versioning は `"1.88"` を版として解釈しない。そのため `"1.88"` と書くと、`rust_version` を宣言する crate のほぼ全リリースが黙って候補から消える（1.70 も 1.88.0 も落ちる）。`"1.88.0"` なら 1.70 / 1.88 は通り、1.90 は落ちる（renovate 42.99.0 の cargo versioning で実測）[^renovate-source]
- **`constraints.rust` そのものは Renovate の追従対象から外す。** App（Renovate 44.x）の `renovate-config` manager は、設定ファイルの `constraints` を依存として読み（`depType: "tool-constraint"`）、`rust 1.88.0 → 1.98.1` のような PR を出そうとする。導入直後の Dependency Dashboard（#749）で見つかった。[^renovate-config-manager][^dashboard-749]この値は MSRV のフィルタで、上げると `msrv` ジョブが落とす版を候補に戻してしまう。そこで `matchManagers: ["renovate-config"]` + `matchDepTypes: ["tool-constraint"]` + `matchDepNames: ["rust"]` を `enabled: false` にした。上げるのは上の規則どおり `rust-version` と同じ PR で、手で行う。ローカルの検証も `renovate@latest` で行う（npx のキャッシュに残る 42.x にはこの manager が無く、効いているかを確かめられない）
- **Alpine の接尾辞（`alpine3.24`）は Renovate では上がらない。** 別系統のタグとして扱われるので、手で上げる
- **`warm-cache.yml` の `env: RUSTFLAGS` は触られない。** github-actions manager が書き換えるのは `uses:` 行だけである
- Renovate のブランチに人間やエージェントが commit を積むと、Renovate はそのブランチの更新を止める（[ADR-0085](/decisions/adr-0085-branch-hint.md)）

# マージ後に確かめること

App のインストールは人間が行う。以下は実際の PR を見て #728 で消し込む。

- 5 系統それぞれで少なくとも 1 本ずつ PR が出た
- Actions の PR が SHA と `# vX.Y.Z` コメントの両方を更新している。`taiki-e/install-action` の 2 版の同居が解消した
- コミットの type が `chore(deps)` / `fix(deps)` になっており、Release PR に意図どおり出る / 出ない
- automerge が赤で止まることを 1 回実測した
- 4 action の PR が automerge されず、レビュー待ちで止まる
- `constraints.rust` で MSRV を超える版が候補から外れる
- Renovate の PR と Dashboard が project board に載らない
- ruleset の `require_extra_approval_for_unattributed_changes` が bot の PR に承認を要求しないか。要求するなら追記して対処を決める
- Renovate の PR に Copilot のレビューが付いたときのノイズが許容範囲か

# 不採用案

- **self-hosted（Actions で Renovate を動かす）**: workflow が 1 本増え、SHA ピンと追従の対象も増える。ADR-0029 が mold を却下したのと同じ論法である。トークンを第三者に預けずに済む利点はあるが、public repo の依存更新の範囲では App の権限で足りる
- **GitHub ネイティブの auto-merge（`platformAutomerge: true`）**: 必須チェックが `lint` だけの ruleset では赤の PR を通す。ruleset に必須チェックを足せば使えるが、それはリポジトリ設定の変更で、この作業の範囲外である
- **`chore/renovate-` の branchPrefix**: 規約に合わせられるが、release-please のブランチはどのみち例外なので、例外を 1 つ書けば両方を扱える
- **全部 `chore(deps)`**: 脆弱性の修正がリリースにも CHANGELOG にも出ず、利用者が修正済みの版を特定できない
- **全部 `fix(deps)`**: 依存を上げるたびに patch リリースが出る
- **zenn-blog の `type:major` 系ラベル**: Renovate 由来だと一目でわからない
- **`prBodyTemplate` を日本語テンプレートに合わせる**: 得るものが無い
- **1 依存 1 PR / 同時数無制限**: `Cargo.lock` を変える PR はそのたびに cold build になる
- **`# renovate:` 注釈と regex manager で Dockerfile を追う**: 設定が増えるだけで、`タグ@ダイジェスト` なら標準の dockerfile manager で足りる
- **Cargo の `rangeStrategy: bump`**: 毎回 `Cargo.toml` の下限を上げることになり、差分が増えるだけである
- **`vulnerabilityAlerts` に寄せて `audit.yml` を止める**: `audit.yml` は「今ある lock」を毎日見る役割で、Renovate にはそれが無い

[^issue-728]: chore(ci): Renovate を導入し、SHA ピン・release-please・必須チェック 1 本の ruleset に合わせて設定する
[^renovate-source]: Renovate の実装
[^renovate-config-manager]: Renovate の renovate-config manager
[^dashboard-749]: Renovate Dashboard 🤖
