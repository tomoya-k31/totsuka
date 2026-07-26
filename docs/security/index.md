# security

脅威モデル・認証認可設計・脆弱性対応方針。

* [Slack ユーザートークンの取り扱いポリシー](slack-user-token.md) - task-source-slack が使う User OAuth Token（xoxp）/ App-Level Token（xapp）の保管・権限・漏えい時の Revoke 手順・社用ワークスペースでの確認事項。
* [端末出力の信頼境界（外部由来テキストの無害化）](terminal-output-sanitization.md) - CLI が第三者の書いたテキスト（Slack 本文・GitHub issue タイトル・author・url・source_task_id）を端末へ出す際の制御シーケンス無害化ポリシー。safe() の適用範囲、エスケープであって除去ではない理由、--json を通さない理由、one_line の 3 段の順序、未カバー経路を定める。
* [Claude Code フック機構のセキュリティポリシー](hook-security.md) - フック完了判定の UDS Bearer トークン管理（keychain 参照・socket 0600 第一層・定数時間比較・herdr env 配送）、スプールファイルの機密保持（N-05: last_assistant_message は機微・$XDG_STATE_HOME 配下・drain 後削除・隔離の注意）、フックアセットの改ざん耐性（N-02: 0700/0600・内容ハッシュ冪等修復・静的埋め込み）を定める。

<!-- concept を追加したら、ここに 1 行追加する:
* [Title](file.md) - frontmatter の description を転記
-->
