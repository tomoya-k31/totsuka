* **Update**: `[[workflows]].trigger` に予約キー `reaction` を追加し、絵文字 → workflow の対応を config.toml 側へ統一（#396）。catch-all より後の workflow は到達不能として警告する。`plugins/slack.toml` の `trigger_reactions` は非推奨（削除は 0.3）、併用は `CONFIG_INVALID` [config.toml リファレンス](/development/config-reference.md)
* **Update**: 新記法の設定例と、やりがちな誤構成の表を追加 [config.toml の設定例集](/development/config-examples.md)
* **Update**: `ReactionTriggers` による記法解決と、`reactions:read` スコープ警告が**解決結果**を見るようになった点を反映（設定フィールドを見ていると、非推奨の案内に従って移行した瞬間に警告が消える） [task-source-slack](/components/task-source-slack.md)
