* **Creation**: [ADR-0067 同梱プラグインはコピーせず、実行中のバイナリのツリーへ毎回解決する](/decisions/adr-0067-bundled-plugin-resolution.md)（#611）。`plugin install --bundled` がマーカー 1 ファイルを書くだけになり、manifest とバイナリは起動のたびに `current_exe` から計算した同梱ツリーへ解決される。

* **Note**: **`brew upgrade totsuka` はインストール済みプラグインを更新せず、更新が要ることも教えてくれなかった。** `plugin install` はバイナリを `$XDG_DATA_HOME/totsuka/plugins/<name>/` へコピーするので、CLI と同梱ツリーが一緒に上がってもコピーは残る。気づけない理由は 3 つ揃っていたこと: 由来が記録されない（インストール済みの `plugin.toml` は元と完全一致）・`doctor` の `bundled-plugins` は同梱**数**を数えるだけでインストール済みの版と比較しない・`plugin upgrade` が無い。唯一の安全網はプロトコル互換検査（F-54）で、**範囲が動いたときだけ** launch を拒否する。範囲内のバグ修正は黙って取り残される。

* **Note**: **設計の中心は「パスを保存しないこと」。** Homebrew は upgrade で旧 Cellar を削除するので、インストール時に記録したパスは必ず腐る。一方「走っているバイナリの隣」は、走っているバイナリが常に現行版であるがゆえに常に正しい。**陳腐化しうる複製が存在しなくなる**ので、検査で鮮度を担保するのではなく食い違える状態を作らない方向に倒した。

* **Note**: **symlink は成立しない。** 同梱ツリーへ symlink を張る案は、パスを保存するのと同じで Homebrew が旧 Cellar を消した瞬間に dangling になる。`/opt/homebrew/opt/<formula>` は版に安定だが Homebrew 固有で、tarball 配置には無い。

* **Note**: **コピーを残す経路を 3 つ切り分けた。** `--bundled`（既定のツリー）だけが記録のみで、`--bundled --bundled-dir <path>` とパス指定・`--from-source` はコピーする。前者は実行時解決が `current_exe` から導くため運用者指定の別ツリーを指さないから、後者 2 つは運用者が選んだスナップショットであり **upgrade が開発ビルドを黙って置き換えてはならない**から。

* **Note**: **「未インストール」と「ツリーが無い」を別のエラーにした。** 同梱由来の記録があるのにツリーが無い（`cargo install` ビルド等）とき、`is_installed` は true のままにして解決時に `NoBundledTree` を返す。「未インストール」と報告すると `plugin install` へ誘導するが、それは誤った修理である —— 宣言は健全で、無いのはツリーのほうだから。

* **Note**: **既存のコピーは後方互換で動く**（マーカーが無いものは `Copied` と読まれる）ので移行手順は無い。陳腐化の検出と `plugin upgrade` は**作らなかった** —— 複製が無いので検出すべき差が存在しない。

* **Update**: `plugin list` に `ORIGIN` 列（`bundled` / `copied`）を追加した。`--json` にも `origin` フィールドが入る。`--bundled` の出力も「Installed … to \<path\>」から「Linked … to the bundled tree」へ変わった。

* **Update**: [orchestrator-cli](/components/orchestrator-cli.md) の `--bundled` の記述と、[orchestrator-core](/components/orchestrator-core.md) の `plugins` モジュールの行を更新した。

* **Note**: **同じディレクトリにマーカーとコピーを同居させない**と決めた（#612 のレビュー指摘）。`commit_link_bundled` はコピーを消してからマーカーを書くが、`commit_install`（コピー側）はマーカーを消していなかった。その状態では `origin_of` が `Bundled` を答え続け、**運用者が今入れたバイナリが解決で飛ばされて一度も起動しない**。両方向で相手の痕跡を消し、ディスク上の表現を常に 1 つに保つことで、「どちらが勝つか」を暗黙の優先順位ではなく到達不能な状態にした。

* **Note**: `origin_of` は `pub` なので**自分で名前を検証する**（同指摘）。`is_installed` が doc で「never probes outside the plugins root」と明記している不変条件を、呼び出し側の検証に委ねると `pub` な入口から破れる。不正な名前は `Copied` と答えてルート内に留まる。実害は読み取り専用の `is_file()` 探査に留まるが、モジュールの約束のほうを守る。
