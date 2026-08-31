* **Update**: Homebrew tap を public 化後に実測し、[ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md) の検査手順を実測結果へ差し替えた。`brew install` → `brew test` → `doctor` まで全て通過（macOS 15.7.3 / Homebrew 6.0.20 / totsuka 0.6.0）。

* **Note**: **`xattr -l` が「空であること」という検査基準が誤りだった。** macOS 13 以降、システムは実行された非システムバイナリに `com.apple.provenance` を付ける。実測では `brew` / `jq` / `gh` を含む**すべての brew バイナリ**に付いており、totsuka 固有でも tap 固有でもない（`/bin/ls` のようなシステム同梱には付かない）。**空を期待する検査は誰がやっても falsify される。** 見るべきは `com.apple.quarantine` が無いことで、構造的な主張（`curl` は quarantine xattr を書かない）のほうは変わらず成立していた —— **主張は正しく、それを測る式だけが間違っていた**という形である。

* **Note**: **開発機でも `brew test` だけは汚染されない。** formula の `test do` が `XDG_CONFIG_HOME` / `DATA` / `STATE` / `CACHE` を `testpath` へ張り替えてから `plugin install --bundled --all --yes` を走らせるので、その機械に既にインストール済みのプラグインは見えない。「クリーンな Mac が要る」と書いていた検査のうち、レイアウト検証という**中心部分は隔離済み**だった。汚染されるのは素の `totsuka doctor` のほうで、これは一時 XDG を渡せば開発機でも新規ユーザー相当になる。

* **Note**: **`brew trust` のプロンプトは検証機では原理的に測れなかった。** 8/22 の非対話実行で `trust.json` に `tomoya-k31/tap/totsuka` が既に記録されており、プロンプトが出る経路に入らない。「未確認」は据え置き。

* **Note**: **0.3.0 → 0.6.0 の実運用アップグレードで壊れたのは 3 層あった。** ① config の形（`poll_interval_secs` の移動でパースエラー。その裏に `trigger.project_status` → `status`（#575）、`on_success.set_status` → `status`（#574）、`plugins/*.toml` の廃止と `[[projects]]` 新設（#554）が隠れていた）② プラグインのバイナリが全て protocol `<0.6` で起動拒否 ③ 本体。**①は最初のパースエラーが後続を隠す形**で、`poll_interval_secs` だけ直しても次の起動でまた止まる。[ADR-0058](/decisions/adr-0058-config-ownership-boundary.md) が「移行はしない」と決めているので、この段差は手で越えることになる。

* **Note**: **検査を `cmd | grep -q X && echo NG` と書くと、`set -e` 下で正常系だけが止まる。** quarantine が「無い」ことを見たいので、正常系では `grep` が 1 を返して行全体が失敗になる —— つまり **OK のときだけ落ちる検査**である。`if … then NG else ok fi` にして常に 0 で終わらせる。Copilot が拾った。**「異常を検出したら真」の道具で「正常であること」を書くと真偽が反転する。**
