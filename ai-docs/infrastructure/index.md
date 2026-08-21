# infrastructure

GCPプロジェクト構成・環境・IaCモジュール・Secret方針。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [Homebrew tap（tomoya-k31/homebrew-tap）](homebrew-tap.md) - totsuka を brew install で配れるようにするための tap リポジトリ。formula のインストールレイアウトがなぜ bundled plugins の探索順と一致するのか、リリースジョブが何を書き換えるのか、HOMEBREW_TAP_TOKEN のスコープ、そして public 化後に外す暫定ガードの手順。
<!-- okf:index:end -->
