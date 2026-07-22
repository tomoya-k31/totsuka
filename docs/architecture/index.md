# architecture

システム構成・コンテキスト図・依存関係・非機能要件。

* [ワークスペース依存境界ルール（Fitness Function）](workspace-dependency-rules.md) - ヘキサゴナル構成の依存不変条件（plugins → plugin-protocol / plugin-sdk のみ、plugin-protocol は leaf、依存循環なし）と、それを CI で機械検証する scripts/arch-lint.sh の仕組み・正当な依存追加時の更新手順。
* [フックシグナルフロー（Slack メンション → 完了検知 → 検収 → 出力）](hook-signal-flow.md) - Claude Code フック完了判定のエンドツーエンド経路。Slack メンションの dispatch から herdr pane 起動・env 注入・claude --settings、Stop フックのマーカー抽出・UDS POST、hook_uds の Bearer/冪等検証、SignalPort→Engine::on_signal の検収分岐（llm/human/none）と Publishing/Verifying/Escalated、スプールフォールバックと pane.exited デッドマンまでを図示する。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
