* **Update**: [task-source-github](/components/task-source-github.md) に `tests/graphql_http.rs` を追加した。`TcpListener` に canned な HTTP/1.1 応答を並べ、実 `ReqwestTransport` を通して 10 本を固定する —— bearer / User-Agent / Content-Type の 3 ヘッダ、401 → `Unauthorized`、その他ステータス → `Http`、body の 500 文字切り詰め、**冪等なときだけのリトライ**、リトライ枯渇、`errors` 入り 200 の素通し、非 JSON の 200 → `InvalidResponse`。**4 つの変異（User-Agent 削除／401 分岐削除／冪等性ゲート削除／切り詰め削除）がすべてテストを落とすことを確認**してから戻した。

* **Note**: これは [task-source-notion](/components/task-source-notion.md) の同種の穴を塞いだ直後に、**同じ構造が github にも残っている**と気づいて調べたもの。調べてみると **`task-source-slack` の `tests/web_api_http.rs` が既に完全に同じことをやっていた**（TCP モック・retry 規律・ステータス写像）。`agent-ide-herdr` も実 `UnixListener` に実 `SocketTransport` を繋いでいる。**つまりリポジトリには既に正解の型があり、notion と github だけがそこから外れていた。** notion 側を書くとき既存の 2 例を探しておらず、独自の形を発明している。**「初めてのユニットテスト」を書くときは、まず隣のクレートが同じ問題をどう解いているか見る。**

* **Note**: 変異テストで 1 件、**コンパイルが通らない変異を「テストが落ちなかった」と読み違えかけた**。冪等性ゲート `idempotent && …` を削ると引数が未使用になり、`[workspace.lints.rust] warnings = "deny"` で `error` になる。nextest は Summary を 1 行も出さないので、出力だけ見ると素通りに見える。`(idempotent || true)` に置き換えて測り直した。**変異は「コンパイルが通る形」で作る。**

* **Note**: タイムアウト → `Timeout` の写像は**意図的に未検査**。30 秒がハードコードで短くする knob が無く、固定するには production 側に設定項目を足すことになる。テストのために本番の形を変える判断は別途。テストファイルの冒頭にその旨を書いた。

* **Note**: `generated.at` に時計を見ずに丸めた値（`02:10`）を入れ、**実時刻より 47 分未来**になっていた。Copilot が拾った。同じ誤りを直前の notion 側にも入れており（`01:30`／実コミットは `01:10`）、そちらは既に main に入っていたのでこの PR で一緒に直した。**時刻フィールドは `date` の出力を使う。手で「だいたいこのくらい」を書かない。**

* **Note**: レビューが**このテスト自体を変異テストして、主張している 2 つの性質が固定されていない**ことを見つけた。(1) 切り詰めの検査本文が ASCII だったので `chars().take(500)` をバイトスライスに変えても緑（本来は非 ASCII の本文で**エラー処理の中で char 境界パニック**になり、元の HTTP 失敗が消える経路）。(2) 失敗系のテストが全部 `max_retries = 0` だったので `is_retryable` の `_ => false` を `true` に変えても緑（本来は期限切れトークンを毎 tick 再送する）。**「変異させたら落ちる」を 4 つ確認したのに、確認していない性質が 2 つ残っていた** —— 変異を選ぶとき、自分が「守った」と思っている性質の一覧から選んでおらず、書きやすいものから選んでいた。多バイト本文と予算つきの非リトライ検査 2 本を足し、3 つの変異で捕まることを確認した。

* **Note**: 意図的に消費されない canned 応答を queue すると accept ループがテストより長生きし、nextest が `leaky` と報告する。abort するガードを足しかけたが、**写した元の `task-source-slack` は消費される分しか queue していない**（ループが終わってlistener が落ちるので、起きてはいけない呼び出しは接続拒否になり、エラー表明側で捕まる）。**既存の形に合わせるほうが、独自の仕組みを足すより良かった。**

* **Note**: 429 の `Retry-After` を github 側は見ていない（slack は見ている）。このテストは**現状の挙動を固定しただけ**で、直したわけではない。ファイル冒頭と component の表に「既知のギャップ」と明記した。
