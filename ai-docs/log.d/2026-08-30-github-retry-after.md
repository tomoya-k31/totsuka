* **Update**: [task-source-github](/components/task-source-github.md) がレート制限の待ち時間を**言われたとおりに待つ**ようになった。`GithubError::RateLimited { retry_after_secs }` を新設し、`transport` が**ヘッダで**スロットルを判定する —— GitHub は primary / secondary のどちらのレート制限でも **403 か 429** を返すので、状態コードだけでは権限エラーと区別できない。優先順は GitHub 自身の決定木どおり `retry-after` →（`x-ratelimit-remaining: 0` かつ `x-ratelimit-reset`）→ 429 なら 60 秒で、**ヘッダの無い素の 403 は権限エラーとして再送しない**。あわせて `is_rejected()` を入れ、**スロットルは非冪等な呼び出しでも replay してよい**ことにした（絞られた要求は実行されていないので、応答を失った 5xx と違い副作用が重ならない）。1 回の呼び出しの合計 sleep は 90 秒で頭打ちにし、超えるなら再試行せず本当の原因を返す。

* **Note**: これは前日「**現状の挙動を固定しただけで直したわけではない**」と明記した既知のギャップの解消。`task-source-slack` に完成形（`RateLimited` 変種・`is_rejected`・`retry_delay`・`retry_budget`・`with_retry_timing`）があったので、**構造ごと写した**。前回の反省がそのまま効いた形で、設計判断はほぼ発生していない。違うのは GitHub 固有の 2 点だけ —— **403 もレート制限になる**ことと、ヘッダ欠落時のフォールバックが 30 秒でなく 60 秒（GitHub のドキュメントが "at least one minute" と書いている）。

* **Note**: 変異テストで**また**「コンパイルが通らない変異を『落ちなかった』と読み違えかけた」。`retry_delay` から `retry-after` の分岐を消すと `error` 引数が未使用になり `warnings = "deny"` で `error`。**同じ罠を前日に記録したばかりで、翌日踏んだ。** 変異を作ったら、まず `cargo build --tests` が通ることを確認してからテストを回す。

* **Note**: `max_retries` の説明が「最大再試行回数」だけだったので、**90 秒の予算で頭打ちになる**ことを [設定リファレンス](/development/config-reference.md) に足した。`slack` 側も同じ予算を持ちながら未記載だったので同時に書いた —— 挙動を変えた側だけ書くと、ドキュメント上に新しい非対称を作ってしまう。

* **Note**: 振る舞いを変えた後、それを説明する散文が **3 箇所**古いまま残っていた —— `is_retryable` の doc（「rate limiting (429)」＝ステータスで判定していた頃の記述）、component の表（「リトライは冪等なときだけ」と書いた直後に例外を述べて自己矛盾）、`post_graphql` の trait doc（スロットルが `idempotent` の外に出たことに触れていない）。Copilot が前 2 つを、grep が 3 つ目を見つけた。**このセッションだけで同じ形を 4 回やっている。** 挙動を変えたら、変えた対象名ではなく**「その挙動を説明している文」を探す**（`冪等` / `idempotent` / `429` で引いた）。

* **Note**: レビューで **判定条件そのものが的外れだった**ことが分かった。**GraphQL の primary rate limit は HTTP 200 で返る**（"the response status will still be `200`"）、secondary は 200 か 403。**このプラグインは GraphQL しか使っていない**ので、403/429 だけを見る実装では**最も一般的な形が素通り**し、`RATE_LIMITED` の envelope が恒久的な GraphQL エラーとして client に渡っていた。**原因は読んだドキュメントを間違えたこと** —— REST の rate-limit ページを読んで実装し、GraphQL のページを見ていない。「GitHub のドキュメントで裏を取った」と PR に書いたが、**裏を取った対象が実際に使っている API ではなかった**。判定を本文（`errors[].type == "RATE_LIMITED"` 等）ベースに変え、これで 403 の secondary も同じ経路で拾えるようになった。

* **Note**: 待ち時間に**下限 1 秒**を入れた。`retry-after: 0`・過去の reset・時計のずれで 0 秒になると、予算が `max_retries` 回の連打を賄ってしまう —— フォールバック定数が防ごうとしているものそのもの。`slept + delay` も `saturating_add` にした（`Duration` の `Add` は overflow でパニックする）。
