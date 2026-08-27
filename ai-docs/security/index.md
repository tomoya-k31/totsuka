# security

脅威モデル・認証認可設計・脆弱性対応方針。

<!-- concept を追加・改名・削除したら `bash scripts/okf-index-build.sh` を実行する。
     description は frontmatter から転記される（手で書かない）。
     並び順と表示タイトルは手で決めてよい — スクリプトはそれを保存する。 -->
<!-- okf:index:begin -->
* [Slack ユーザートークンの取り扱いポリシー](slack-user-token.md) - task-source-slack が使う User OAuth Token（xoxp）/ App-Level Token（xapp）/ Bot User OAuth Token（xoxb、通知ナッジ専用・任意）の保管・権限・漏えい時の Revoke 手順・社用ワークスペースでの確認事項。
* [端末出力の信頼境界（外部由来テキストの無害化）](terminal-output-sanitization.md) - totsuka が第三者の書いたテキスト（Slack 本文・GitHub issue タイトル・author・url・source_task_id）を端末へ出す際の制御シーケンス無害化ポリシー。safe() の置き場所（core の terminal モジュール）と適用範囲、エスケープであって除去ではない理由、--json と JSON ログを通さない理由、one_line の 3 段の順序、menu が足す SwiftBar 書式の第 2 層、未カバー経路を定める。
* [Claude Code フック機構のセキュリティポリシー](hook-security.md) - フック完了判定の UDS Bearer トークン管理（keychain 参照・socket 0600 第一層・定数時間比較・herdr env 配送）、スプールファイルの機密保持（N-05: last_assistant_message は機微・$XDG_STATE_HOME 配下・drain 後削除・隔離の注意）、フックアセットの改ざん耐性（N-02: 0700/0600・内容ハッシュ冪等修復・静的埋め込み）を定める。
<!-- okf:index:end -->
