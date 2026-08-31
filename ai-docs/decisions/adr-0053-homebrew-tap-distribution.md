---
type: Decision
title: ADR-0053 配布を Homebrew tap に寄せ、formula は別リポジトリに置く
description: "sudo 5 本の tarball 手配置をやめ brew install / brew upgrade へ移す決定。formula は tomoya-k31/homebrew-tap に置き、リリースジョブが version と sha256 の 2 行だけを書き換えて push する。本リポジトリ内の Formula/ 案はブランチ保護で自動化できないため却下。tap が実際に効くのは本リポジトリが public になってから。"
resource: https://github.com/tomoya-k31/homebrew-tap
tags: [decision, distribution, homebrew, release, install, adr]
generated: { by: claude-code/opus-5, at: 2026-08-31T00:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: bundled-rs
    resource: https://github.com/tomoya-k31/totsuka/blob/main/crates/orchestrator-cli/src/bundled.rs
    title: "orchestrator-cli bundled.rs — bundled plugins の探索"
  - id: release-runbook
    resource: /operations/release-runbook.md
    title: "リリース Runbook"
  - id: homebrew-tap-doc
    resource: /infrastructure/homebrew-tap.md
    title: "Homebrew tap（tomoya-k31/homebrew-tap）"
  - id: adr-0028
    resource: /decisions/adr-0028-setup-wizard.md
    title: "ADR-0028 totsuka setup は対話ウィザードにし、機密は一切扱わない"
  - id: homebrew-tap-trust
    resource: https://docs.brew.sh/Tap-Trust
    title: "Homebrew — Tap Trust"
---

# Status

stable。ワークフロー配線と tap リポジトリの作成は本 ADR と同一 PR。

**一部検収済み（2026-08-31）。** 2026-08-31 に本リポジトリを public 化し、`brew install`
→ `brew test` → `doctor` を実測して通した（結果は下の「public 化の後に実測すること」）。

**それでも `verified` は付けていない。** ADR が挙げた実測項目のうち **`brew trust` の
対話プロンプト**がまだ測れていないためである。検証機では 8/22 の非対話実行で
`trust.json` に `tomoya-k31/tap/totsuka` が既に記録されており、**プロンプトが出る経路に
原理的に入らない**。また実測は開発機で行っており、ADR が指定した「totsuka を一度も
入れたことのない Mac」ではない（レイアウト検証の中心である `brew test` は `test do` が
XDG を張り替えるので隔離されているが、その 1 点だけである）。

# Context

`totsuka` は「他人が入れられる」状態になっていない。新マシン 1 台あたりの手作業は 12〜15 個あり、その大半は totsuka の外側にある。配布層だけを取り出すと、README が指示するのは次の 5 コマンドである。

```sh
tar -xzf totsuka-*-macos-universal.tar.gz
sudo rm -rf /usr/local/lib/totsuka
sudo mv totsuka-*-macos-universal /usr/local/lib/totsuka
sudo ln -sf /usr/local/lib/totsuka/totsuka /usr/local/bin/totsuka
sudo xattr -dr com.apple.quarantine /usr/local/lib/totsuka
```

問題は本数だけではない。

1. **`xattr` を忘れると壊れ方が最悪である。** リリースの署名は ad-hoc（`codesign --sign -`）なので、quarantine が付いたままだと **プラグインだけ**が SIGKILL される。`totsuka --version` は動くのに `doctor` は "crashed or exited" としか言えない。
2. **更新手段が存在しない。** `totsuka upgrade` は無く、更新は上の 5 本をもう一度打つこと。実際、開発機の `/usr/local/bin/totsuka` は手コピーの thin arm64 build・0.3.0 のまま 2 リリース遅れており、`/usr/local/lib/totsuka` が無いので `plugin install --bundled` が原理的に動かない状態だった。
3. **「ツリーごと動かす」という制約が暗黙である。** bundled plugins は実行ファイルの隣を探すので、バイナリだけを `mv` すると黙って見つからなくなる。

# Decision Drivers

- 見知らぬ人が最初の `totsuka run` に到達できること。配布は最初の関門であって、ここで落とすと後段の良し悪しは関係ない
- 更新が 1 コマンドで済むこと。「古いまま気づかない」は実際に起きた
- `xattr` の穴を手順から消すこと（覚えている人だけが助かる手順は手順ではない）
- リリース自動化に人手を挟まないこと。リリースごとに手作業が要るなら遅れて腐る
- **Rust 側を変えないこと**。配布形式の都合でアプリのコードを変えるのは尾が犬を振る

# Options Considered

| 案 | 却下/採用の理由 |
|---|---|
| **A. 別リポジトリ `tomoya-k31/homebrew-tap` に formula を置く** | **採用。** 保護が無いのでリリースジョブから push でき、本リポジトリの CI を 1 つも再トリガしない |
| B. 本リポジトリに `Formula/` を置く | **却下。自動化できない**（下記） |
| C. `install.sh`（curl-pipe） | tap リポジトリが要らないのは利点だが、`sudo` が残り、更新は再実行、`xattr` も自前。得るものが A より小さい |
| D. `totsuka upgrade`（自己更新）を実装する | 初回の 5 コマンドが残る。Rust 側の変更が要る（Drivers に反する）。将来 A と併存はできる |
| E. cargo-dist 等の導入 | 現状の `universal-binary` ジョブが既に必要な成果物を作っており、置き換えの利得が無い |

## なぜ B（本リポジトリ内の `Formula/`）が成立しないか

美観の問題ではない。**ブランチ保護の下で自動化できない。**

formula の bump はタグが出来た**後**、つまりリリース run の中から `main` へ bot push する必要がある。`main` の Ruleset は required check `lint` + `bypass_actors = Admin` で、[リリース Runbook](/operations/release-runbook.md) は `github-actions[bot]` を `bypass_actors` に足す案を明示的に却下している（bypass はマージ実行 actor にしか効かず、全ワークフローが main 保護をバイパスできる広い攻撃面になるため）。迂回は「Ruleset を緩める」か「リリースごとに PR を手マージする」しかなく、後者は目的の逆である。

副次的にも噛み合わない。実測した trigger は次のとおり:

| ワークフロー | Ruby 1 行だけの push で発火するか |
|---|---|
| `ci.yml` | **する。** `push: branches: [main]` に paths フィルタが無い。`coverage`（llvm-cov のフルビルド）が回る |
| `release-please.yml` | **する。** 同じく paths フィルタ無し。切ったばかりのリリースの直後に再実行される |
| `okf-lint.yml` | しない。push トリガには paths フィルタがある |

（`GITHUB_TOKEN` の push はワークフローを起動しないが、B では Ruleset がその push 自体を拒否するので PAT を使うことになり、上の再トリガが実際に起きる。）さらに、bump はタグの後に来る以上、**タグ済みツリーは自分を指す formula を含まない**。tap は既定ブランチを読むので、タグと tap が恒久的に食い違う。

## なぜ tap 名が `tap` なのか（`brew install tomoya-k31/tap/totsuka`）

Homebrew 6.0.17 のソースで確認した。

- 真ん中は「リポジトリ名」ではなく **tap 名**で、Homebrew が `homebrew-` を前置してリポジトリ名を導出する（`tap.rb` の `@full_repository = "homebrew-#{@repository}"`）。`tomoya-k31/tap` → `github.com/tomoya-k31/homebrew-tap`
- tap 修飾された formula 参照は **3 セグメント固定**（`tap_constants.rb` の `HOMEBREW_TAP_FORMULA_REGEX`）。2 セグメント形式は存在せず、`/` は formula 名の文字クラスにも入らないので `brew install tomoya-k31/totsuka` は構文として成立しない
- `brew tap tomoya-k31/homebrew-tap` と `brew tap tomoya-k31/tap` は同じもの。`Tap.fetch` が先頭の `homebrew-` を落とす（`tap_constants.rb` の `/\A(home|linux)brew-/`）
- 一度 tap すれば素の `brew install totsuka` / `brew upgrade totsuka` で通る。`Formulary::FromNameLoader` が core の後にインストール済みの非 core tap を探し、一致が 1 つなら解決する（2 つ以上は `TapFormulaAmbiguityError`）。**homebrew-core の formula 名 8,530 件・alias・cask のいずれにも `totsuka` は無い**（実測。最も近いのは `totp-cli`）

`homebrew-totsuka` にすれば `brew install tomoya-k31/totsuka/totsuka` にはできるが、totsuka 以外を出したときに tap をもう 1 本作ることになり、インストール行に `totsuka` が 2 度出る。`homebrew-tap` を採る。

# Decision

## 1. formula は `tomoya-k31/homebrew-tap` の `Formula/totsuka.rb` に置く

public リポジトリ（private tap は他人が入れられない）。配置とレイアウトの詳細は [Homebrew tap](/infrastructure/homebrew-tap.md)。

## 2. install レイアウトは `bin/` + `libexec/totsuka/plugins`。Rust 側は変えない

`bundled.rs` は実行ファイルのあるディレクトリを基点に、**呼ばれたパスと `canonicalize` 後のパスの両方**について `<dir>/plugins` と `<dir>/../libexec/totsuka/plugins` を探す（`current_exe()` が macOS で symlink を解決しないため両方が要る）。この 4 通りのうち、`bin.install "totsuka"` + `(libexec/"totsuka").install "plugins"` は `canonicalize` 後の `<cellar>/bin/../libexec/totsuka/plugins` に当たる。

**これは偶然ではない。** `bundled.rs` の `candidate_roots` は当初から libexec 形を持っており、コメントが "a Homebrew formula would put the binary in `bin/` and its private files in `libexec/`" と明言している。当時の予測どおりに嵌まったので、**このリリースで Rust のコードは 1 バイトも変わらない。**

plugins をバイナリの隣に平置きして第 1 候補に当てる形は採らない。`bin/` は Homebrew の prefix へ link されるので、`plugins` ディレクトリごと link されてしまう。

## 3. formula の `url` は `version` から導出し、自動化は 2 行しか触らない

`version` を `url` より先に宣言し、URL 側は `#{version}` で補間する。リリースジョブが書き換えるのは `^  version "…"` と `^  sha256 "…"` の 2 行だけで、**タグが 2 回現れる URL 文字列には触らない**。`brew audit --strict` は明示 `version` を「URL から読めるので冗長」と言うが、自動化が URL を組み立てないことのほうを取る。

書き換えの後に `grep -q` で 2 行を検査する。黙って未編集だと formula は前のリリースを永久に指し続け、`brew upgrade` は "already up-to-date" と言う — 誰も気づけない壊れ方になるので、赤いリリースに変換する。

## 4. bump は `universal-binary` ジョブの最終ステップ。専用ジョブにしない

Actions の課金はジョブ単位で分単位切り上げ（`ci.yml` に記録がある）。既に回っている 1 分の中のステップは実質ただで、専用ジョブは 1 分丸ごと払う。加えて **sha256 は既にディスク上にある**（`${PREFIX}.tar.gz.sha256`）ので再ダウンロードも再ハッシュも要らず、アセット upload の後に走るので **404 する URL を指す formula は原理的に作れない**。

## 5. トークンは `RELEASE_PLEASE_TOKEN` と分ける

`RELEASE_PLEASE_TOKEN` は本リポジトリのみにスコープされた fine-grained PAT で、tap へは push できない。広げるとリリーストークンの爆発半径とローテーション周期が tap に結合する。**`HOMEBREW_TAP_TOKEN` を別に発行する**（`tomoya-k31/homebrew-tap` の Contents: Read and write だけ）。

## 6. 既存の配布経路は残す

- **tarball**: formula が指す実体そのものであり、Homebrew を使わない人の唯一の経路。`xattr` の注意は tarball 側に残す（ブラウザ落としは実際に quarantine される）
- **`cargo install --git`**: コントリビュータ経路。「CLI だけが入る」という既存の但し書きは正しい

# public 化が前提条件

**Homebrew の formula は `url` を素の `curl`（GitHub 認証なし）で取りに行く。** 本リポジトリは現在 private なので、リリースアセットの URL は未認証では 404 を返す（実測）。したがって **tap 経路は本リポジトリが public になるまで動かない。**

この事実に合わせて、bump ステップは **`if: ${{ !github.event.repository.private }}`** でゲートしてある。private の間はステップごと skip され、**public 化した瞬間に自分で有効になる**。

**シークレットの有無でゲートしていないのは、それが危険を読み違えるからである。** `secrets.HOMEBREW_TAP_TOKEN != ''` 相当のガードは、*失効した*トークン（非空なのでどのみち大声で落ちる）を素通りさせる一方で、*未登録・削除・改名*されたシークレットを**毎リリース黙って緑で skip させ、tap を永久に置き去りにする**。それは `grep -q` の assert を置いて赤に変換しようとしている失敗そのものである。可視性でゲートすれば、**外し忘れうる人間の手順が存在せず**、public 化以降はシークレット欠落が赤くなる。

public 化の後に残る手順は [Homebrew tap](/infrastructure/homebrew-tap.md) の「まだ済んでいないこと」にある。**ゲートを外す作業は含まれない** — 自分で外れる。

# Consequences

## 良くなること

- 新マシンの配布層が **5 コマンド → 1 コマンド**になり、更新が `brew upgrade totsuka` になる
- **`xattr` の穴が手順から消える**見込み（要実測、下記）
- 「ツリーごと動かす」という暗黙の制約が formula の中に閉じ、ユーザーの手順から消える
- `totsuka completion` の shell 補完が bash / zsh / fish で自動的に入る（`generate_completions_from_executable`）

## 悪くなること・注意点

- **リポジトリが 2 つ、トークンが 2 本になる。** tap 側のトークンが失効すると **`git clone` が落ちてリリース run が赤くなる**（タグ・Release・アセットは既に公開済みで無事）。bump が失敗したときの**復旧は、job の再実行ではなく tap の formula を手で直すこと**。再実行すると tarball が作り直され、`tar`/`gzip` が mtime を埋め込むためバイト同一にならず、`--clobber` が公開済みアセットを別の sha256 のものへ差し替えてしまう。 失効日を Runbook に記録すること
- **`brew install` は formula の `test do` を走らせない。** レイアウトが壊れても、誰かの `setup` がプラグインを見つけられなくなるまで気づけない。tap 側の定期 CI が本来の対策で、まだ無い
- **Homebrew 6.0 は third-party tap に `brew trust` を要求する。** 「1 コマンド」の主張はここで少し弱まる。`brew install tomoya-k31/tap/totsuka` を非対話で実行したときは formula が `trust.json` に自動追加されたが、**対話実行時に確認プロンプトが出るかは未確認**
- **quarantine が付かないことは未実測。** ad-hoc 署名がコピーで保存されること、quarantine xattr を書くのが LaunchServices 経由のダウンローダであって `curl` ではないことは構造的に言えるが、実際に測るまで README には書かない
- Homebrew の keg relocation がバイナリに触るかは不明。触っても ad-hoc 再署名なら問題ないが、壊れた場合の症状は「`--version` は動くのにプラグインだけ SIGKILL」という本プロジェクトで最悪の形になる
- bottle が無いので `brew install` に Xcode Command Line Tools が要る

# 検証

本 PR で実測できたこと:

- `sed` の 2 式が狙った 2 行だけを書き換え、`url` の `#{version}` 補間を無傷で残し、書き換え後も Ruby 構文が妥当であること（実 formula に対して実行）
- `grep -q` の 2 つの assert が書き換え後に通ること
- tap 名の解決規則と、`totsuka` が homebrew-core と衝突しないこと
- **リリースアセットが未認証 `curl` で 404 になること**（= public 化が前提条件であること）

public 化の後に実測すること（2026-08-31 に実施済み、結果は下記）:

```sh
brew install tomoya-k31/tap/totsuka

# quarantine が「無い」ことを見る。`xattr -l` が空であることではない（下記）。
# `grep -q … && echo NG` と書いてはいけない —— 正常系（quarantine 無し）で grep が
# 1 を返すため、`set -e` 下では「OK のときだけ止まる」検査になる。
for f in "$(brew --prefix)/bin/totsuka" \
         "$(brew --prefix totsuka)/libexec/totsuka/plugins/github/github"; do
  if xattr "$f" | grep -q com.apple.quarantine; then echo "NG  $f"; else echo "ok  $f"; fi
done

codesign -dv --verbose=2 "$(brew --prefix)/bin/totsuka"   # adhoc
brew test totsuka
totsuka doctor    # 本命。quarantine されたプラグインは無言で殺され、
                  # doctor は "crashed or exited" としか言えない
```

**当初この検査を「`xattr -l` が空であること」と書いていたが、それは誤りだった。**
macOS 13 以降、システムは実行された非システムバイナリに `com.apple.provenance` を
付ける。実測（macOS 15.7.3）では `brew` / `jq` / `gh` を含む**すべての brew バイナリ**に
付いており、totsuka 固有でも tap 固有でもない（`/bin/ls` のようなシステム同梱には付かない）。
空を期待する検査は誰がやっても falsify されるので、**見るべきは `com.apple.quarantine` が
無いこと**である。構造的な主張（`curl` は quarantine xattr を書かない）は変わらず成立する。

`totsuka doctor` が鋭い端である。quarantine された**プラグイン**はメインのバイナリが動いたまま黙って落ちるので、`--version` が通ることは何の証拠にもならない。

実測結果（macOS 15.7.3 / Homebrew 6.0.20 / totsuka 0.6.0）:

- `com.apple.quarantine` は本体・プラグインとも**無し**
- `codesign` は両方とも `flags=0x2(adhoc)` / `TeamIdentifier=not set`
- `brew test totsuka` が **exit 0**。`test do` が XDG を `testpath` へ張り替えるので、
  この検査だけは開発機でも既存の設定・プラグインに汚染されない
- 新規ユーザー相当の `doctor`（一時 XDG）で
  `bundled-plugins — 6 in .../bin/../libexec/totsuka/plugins` を確認。探索順
  `<exe dir>/../libexec/totsuka/plugins` が実環境で解決している
- **`brew trust` の対話プロンプトは未確認のまま**。検証機では 8/22 の非対話実行で
  `trust.json` に `tomoya-k31/tap/totsuka` が既に記録されており、プロンプトが出る
  経路に入らなかった
