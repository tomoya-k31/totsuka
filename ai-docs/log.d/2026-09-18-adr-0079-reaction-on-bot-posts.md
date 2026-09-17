* **Creation**: bot が投稿したメッセージへのリアクションでタスクを起こす決定を [ADR-0079](/decisions/adr-0079-reaction-on-bot-posts.md) に追加。緩めるのは反応先の投稿者だけで、起動のジェスチャは操作者本人のリアクションのまま。許可は workflow の trigger 単位の `from_bot` で宣言し、Gateway と wire schema は据え置く。
* **Update**: [task-source-slack](/components/task-source-slack.md) のリアクション判定に `from_bot` を反映。許可済み bot の投稿だけが `to_mention` を通り、`user` を持たない bot 投稿は `bot_id` が送信者として入る。許可済みでも編集・削除の subtype は従来どおり落ちる。
* **Update**: [設定リファレンス](/development/config-reference.md) に `trigger.from_bot` の節を追加。既定は人間の投稿のみであること、グローバル設定にしなかった理由、`reaction` 無し・`channel` 併記・空配列を `initialize` が弾くことを記録した。
