---
type: Environment
title: Homebrew tap（tomoya-k31/homebrew-tap）
description: "totsuka を brew install で配れるようにするための tap リポジトリ。formula のインストールレイアウトがなぜ bundled plugins の探索順と一致するのか、リリースジョブが何を書き換えるのか、HOMEBREW_TAP_TOKEN のスコープ、bump が失敗したときの復旧、そして public 化までステップを止めている可視性ゲート。"
resource: https://github.com/tomoya-k31/homebrew-tap
tags: [infrastructure, homebrew, distribution, release, token]
generated: { by: claude-code/opus-5, at: 2026-08-22T00:00:00Z }
status: stable
owner: tomoya-k31
sources:
  - id: adr-0053
    resource: /decisions/adr-0053-homebrew-tap-distribution.md
    title: "ADR-0053 配布を Homebrew tap に寄せ、formula は別リポジトリに置く"
  - id: release-runbook
    resource: /operations/release-runbook.md
    title: "リリース Runbook"
  - id: homebrew-tap-trust
    resource: https://docs.brew.sh/Tap-Trust
    title: "Homebrew — Tap Trust"
---

# 何であるか

`github.com/tomoya-k31/homebrew-tap`（public）。`Formula/totsuka.rb` 1 本を持つ。

判断の背景は [ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md)。ここは運用の詳細だけを置く。

利用者から見た入れ方は 2 通りあり、等価である。

```sh
brew install tomoya-k31/tap/totsuka      # tap も同時に行われる

brew tap tomoya-k31/homebrew-tap         # 以後は素の名前で扱える
brew install totsuka
```

更新はどちらで入れても `brew upgrade totsuka`。

`tomoya-k31/tap` と `tomoya-k31/homebrew-tap` は同じ tap を指す（`Tap.fetch` が先頭の `homebrew-` を落とす）。リポジトリ名の `homebrew-` 接頭辞は必須で、これが短縮形を成立させている。

# インストールレイアウト（変更するな、と言うより「変えるなら Rust も見ろ」）

```ruby
bin.install "totsuka"
(libexec/"totsuka").install "plugins"
```

これは様式ではなく仕様である。`crates/orchestrator-cli/src/bundled.rs` は bundled plugins を、実行ファイルのあるディレクトリを基点に探す。探索は次の直積になる:

| | `<dir>/plugins` | `<dir>/../libexec/totsuka/plugins` |
|---|---|---|
| **呼ばれたパスの親** | 候補 1 | 候補 2 |
| **`canonicalize` 後の親** | 候補 3 | 候補 4 |

両方が要るのは `std::env::current_exe()` が macOS で symlink を解決しないため。Homebrew の場合それぞれこうなる:

| 候補 | 実際のパス | 結果 |
|---|---|---|
| 1 | `/opt/homebrew/bin/plugins` | 無い |
| 2 | `/opt/homebrew/libexec/totsuka/plugins` | 無い。**Homebrew は `libexec` を prefix へ link しない** |
| 3 | `<cellar>/bin/plugins` | 無い |
| 4 | `<cellar>/bin/../libexec/totsuka/plugins` | **当たる** |

外れる 3 回はいずれも `is_dir` 呼び出し 1 回で、コストは無視できる。

**plugins をバイナリの隣に平置きして候補 1 に当てる形は採らない。** `bin/` は Homebrew の prefix へ link されるので、`plugins` ディレクトリごと link されてしまう。

**この配置のために Rust 側を変えた事実は無い。** `candidate_roots` は当初から libexec 形を持ち、コメントが Homebrew を名指ししている。formula 側を後から合わせただけである。

# リリース時に何が起きるか

`.github/workflows/release-please.yml` の `universal-binary` ジョブの**最終ステップ**が、アセットを Release に upload した直後に tap へ push する。

書き換えるのは 2 行だけである。

```ruby
  version "0.5.0"
  sha256 "8549404c…"
```

`url` は formula の中で `#{version}` から導出されているので**触らない**。これが URL とタグのドリフトを構造的に防いでいる。`sha256` はリリースジョブが既に作っている `${PREFIX}.tar.gz.sha256`（ファイル名列なしの生ハッシュ）をそのまま読む。

書き換えの直後に `grep -q` で 2 行を検査する。**黙って未編集になるのが一番危ない壊れ方**だからである（formula は前のリリースを指し続け、`brew upgrade` は "already up-to-date" と言う）。

## 手で formula を編集するときの注意

`sed` は `^  version "…"` と `^  sha256 "…"` に**行頭アンカーで**当たる。次をやると次のリリースが赤くなる（黙って壊れるよりはよいが、意図せず踏むと驚く）:

- 2 行のインデントを変える
- `sha256` を `url` のブロックの中へ移す、順序を変える
- `version` を消して URL 直書きに戻す

`test do` 内の `version.to_s` やコメント中の `sha256` という語は、アンカーのおかげで巻き込まれない。

# HOMEBREW_TAP_TOKEN

| 項目 | 値 |
|---|---|
| 種別 | fine-grained PAT |
| Resource owner | `tomoya-k31` |
| Repository access | **`homebrew-tap` のみ** |
| Permissions | **Contents: Read and write** だけ |
| 置き場所 | `tomoya-k31/totsuka` の Actions secret `HOMEBREW_TAP_TOKEN` |
| 失効日 | **未発行**（下の「まだ済んでいないこと」） |

`RELEASE_PLEASE_TOKEN` を流用しない。あれは totsuka リポジトリのみにスコープされていて tap へ push できず、広げるとリリーストークンの爆発半径とローテーション周期が tap に結合する。

失効すると `git clone` が落ち、**リリース run が赤くなる**（タグ・Release・アセットは既に公開済みで無事）。発行したら失効日を [リリース Runbook](/operations/release-runbook.md) のトークン表に書くこと。

## bump が失敗したときの復旧

bump が失敗したときの**復旧は、job の再実行ではなく tap の formula を手で直すこと**。再実行すると tarball が作り直され、`tar`/`gzip` が mtime を埋め込むためバイト同一にならず、`--clobber` が公開済みアセットを別の sha256 のものへ差し替えてしまう。

手順は 1 つ: tap の `Formula/totsuka.rb` の `version` と `sha256` を、公開済みアセットの値に手で合わせて push する。sha256 は Release に `.sha256` として添付されている。

# public 化までステップを止めている可視性ゲート

Homebrew の formula は `url` を**素の `curl`（GitHub 認証なし）**で取る。totsuka リポジトリが private である間、リリースアセットの URL は未認証では 404 を返す。**tap 経路は public 化まで動かない。**

そのため bump ステップは **`if: ${{ !github.event.repository.private }}`** でゲートしてある。private の間はステップごと skip され、**public 化した瞬間に自分で有効になる。外し忘れうる人間の手順は無い。**

**シークレットの有無でゲートしていない**のは意図的である。それは危険を読み違える:

| 状況 | シークレットでゲートした場合 | 可視性でゲートした場合 |
|---|---|---|
| トークンが**失効**した | 非空なので素通り → `git clone` が落ちて赤（見える） | 同じ |
| シークレットが**未登録・削除・改名**された | 空なので**毎リリース緑で skip**。tap が永久に置き去り（見えない） | `git clone` か `push` が落ちて赤（見える） |

下段が問題である。それは `grep -q` の assert を置いて赤に変換しようとしている失敗そのもので、ガードがそれを再導入してしまう。

## まだ済んでいないこと（tap を本番にするまでの手順）

1. **totsuka リポジトリを public にする。** これが済むまで残りは意味を持たない。bump ステップはこの時点で自動的に有効になる
2. `Formula/totsuka.rb` の `version` / `sha256` が最新リリースを指しているか確認する（公開までに何度かリリースが出ていれば古い）。sha256 はアセットとして公開されている:

   ```sh
   gh release view --json tagName -q .tagName -R tomoya-k31/totsuka
   gh release download <TAG> -R tomoya-k31/totsuka \
     -p 'totsuka-*-macos-universal.tar.gz.sha256' -O -
   ```

3. `HOMEBREW_TAP_TOKEN` を上の表のスコープで発行し、Actions secret に登録する。**public 化の後は、これが無いとリリースが赤くなる**（そうなるように設計してある）
4. **クリーンな Mac で実測する**（[ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md) の「検証」節のコマンド）。特に `totsuka doctor` — quarantine されたプラグインはメインのバイナリが動いたまま黙って落ちるので、`--version` が通ることは何の証拠にもならない
5. 実測が通ったら README / setup playbook を brew 主導へ書き換える

# 知っておくとよいこと

- **Homebrew 6.0 は third-party tap に `brew trust` を要求する**（`https://docs.brew.sh/Tap-Trust`）。未 trust の tap は「無視されている」と表示される。`brew install tomoya-k31/tap/totsuka` を**非対話で**実行したときは formula が `trust.json` に自動追加された。**対話実行時に確認プロンプトが出るかは未確認**なので、README にはまだ書いていない
- **`brew install` は formula の `test do` を走らせない。** レイアウトが壊れても、誰かの `setup` がプラグインを見つけられなくなるまで誰も気づけない。手で回すなら:

  ```sh
  brew install --build-from-source ./Formula/totsuka.rb
  brew test totsuka
  ```

  tap 側に定期 CI を置くのが本来の対策で、まだ無い
- bottle が無いので `brew install` には Xcode Command Line Tools が要る
- 旧手動インストール（`/usr/local/bin/totsuka` の symlink）が残っていると `brew link` が "Target already exists" で拒否する。先に消す:

  ```sh
  sudo rm -f /usr/local/bin/totsuka
  sudo rm -rf /usr/local/lib/totsuka
  ```
