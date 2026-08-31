* **Update**: README（英日）と [setup-playbook](/operations/setup-playbook.md) のインストール手順を **Homebrew 主導**へ書き換えた（[ADR-0053](/decisions/adr-0053-homebrew-tap-distribution.md) の目的そのもの）。`sudo` 3 行 + `xattr` 1 行の tarball 配置は「Homebrew を使わない場合」へ降ろした。生成物の `docs/setup-playbook.md` / `.ja.md` も同時に更新。

* **Note**: **`brew trust` は「1 コマンドで入る」を壊さなかった。** ADR-0053 と #506 が「未確認」として残していた最後の項目で、実測の結論は **formula を名指しすれば同じコマンドの中で trust が付与される** —— `==> Trusted formula tomoya-k31/tap/totsuka` の 1 行が出て進むだけで、プロンプトは無い。**対話・非対話の両方で同じ**だった（`trust.json` の該当エントリを退避して再現）。

* **Note**: **測り方を 2 回間違えた。** ① `brew info` では発火しない —— 出力に `Installed (on request)` とあるとおりインストール済みの receipt を読んでおり、tap の formula を load しないため。`brew trust --help` の「Homebrew may **load** them」から `info` でも出ると踏んだが外れた。発火するのは `reinstall`（= formula を実際に load して実行する経路）である。② 警告の切れ端 `This is not recommended and will be removed in a later release.` を「自動 trust が将来消える」と読みかけたが、全文を採ると**直前の `export HOMEBREW_NO_REQUIRE_TAP_TRUST=1` を指していた**。消えるのは env var の抜け道のほうで、名指し時の自動 trust ではない。**切れ端で読んでいたら逆の結論を README に書いていた。**

* **Note**: quarantine が付かないことは brew 経路の性質であって tarball 経路の性質ではないので、`xattr -dr` の説明は tarball 側へ移し「**この経路だけが必要**」と明示した。両方に同じ注意書きを残すと、brew で入れた人が不要な `sudo xattr` を打つ。
