* **Update**: [task-source-github](/components/task-source-github.md) に `tests/graphql_http.rs` を追加した。`TcpListener` に canned な HTTP/1.1 応答を並べ、実 `ReqwestTransport` を通して 10 本を固定する —— bearer / User-Agent / Content-Type の 3 ヘッダ、401 → `Unauthorized`、その他ステータス → `Http`、body の 500 文字切り詰め、**冪等なときだけのリトライ**、リトライ枯渇、`errors` 入り 200 の素通し、非 JSON の 200 → `InvalidResponse`。**4 つの変異（User-Agent 削除／401 分岐削除／冪等性ゲート削除／切り詰め削除）がすべてテストを落とすことを確認**してから戻した。

* **Note**: これは [task-source-notion](/components/task-source-notion.md) の同種の穴を塞いだ直後に、**同じ構造が github にも残っている**と気づいて調べたもの。調べてみると **`task-source-slack` の `tests/web_api_http.rs` が既に完全に同じことをやっていた**（TCP モック・retry 規律・ステータス写像）。`agent-ide-herdr` も実 `UnixListener` に実 `SocketTransport` を繋いでいる。**つまりリポジトリには既に正解の型があり、notion と github だけがそこから外れていた。** notion 側を書くとき既存の 2 例を探しておらず、独自の形を発明している。**「初めてのユニットテスト」を書くときは、まず隣のクレートが同じ問題をどう解いているか見る。**

* **Note**: 変異テストで 1 件、**コンパイルが通らない変異を「テストが落ちなかった」と読み違えかけた**。冪等性ゲート `idempotent && …` を削ると引数が未使用になり、`[workspace.lints.rust] warnings = "deny"` で `error` になる。nextest は Summary を 1 行も出さないので、出力だけ見ると素通りに見える。`(idempotent || true)` に置き換えて測り直した。**変異は「コンパイルが通る形」で作る。**

* **Note**: タイムアウト → `Timeout` の写像は**意図的に未検査**。30 秒がハードコードで短くする knob が無く、固定するには production 側に設定項目を足すことになる。テストのために本番の形を変える判断は別途。テストファイルの冒頭にその旨を書いた。
