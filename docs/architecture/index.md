# architecture

システム構成・コンテキスト図・依存関係・非機能要件。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [ワークスペース依存境界ルール（Fitness Function）](workspace-dependency-rules.md) - ヘキサゴナル構成の依存不変条件（plugins → plugin-protocol / plugin-sdk のみ、plugin-protocol は leaf、依存循環なし）と、それを CI で機械検証する scripts/arch-lint.sh の仕組み・正当な依存追加時の更新手順。
* [フックシグナルフロー（Slack メンション → 完了検知 → 検収 → 出力）](hook-signal-flow.md) - Claude Code フック完了判定のエンドツーエンド経路。Slack メンションの dispatch から herdr pane 起動・env 注入・claude --settings、Stop フックのマーカー抽出・UDS POST、hook_uds の Bearer/冪等検証、SignalPort→Engine::on_signal の検収分岐（llm/human/none）と Publishing/Verifying/Escalated、スプールフォールバックと pane.exited デッドマン、通知クリック → pane フォーカス（click-to-focus、F-94）までを図示する。
<!-- okf:index:end -->
