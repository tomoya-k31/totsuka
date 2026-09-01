* **Change**: `task-source-notion` の `repo_hint` が **`multi_select` を読むようになった**（#604）。読む順は `rich_text` → `select`/`status` → `multi_select` → `url` → `title`。**ちょうど 1 つ選ばれているときだけ**その option 名を返し、0 個と 2 個以上はどちらも `None` になる。[config-reference](/development/config-reference.md) の `property_map.repo_hint` の行を更新した。

* **Note**: **実運用の Notion ボードでは、リポジトリ列が `multi_select` になっていることがある。** 社内の実運用ボードはリポジトリ列が `multi_select` で、`<org>/<repo>` 形式の option を 13 個持ち、実際に 30 件以上のページで設定されている。ところが `prop_text()` が探すのは `rich_text` / `select` / `status` / `url` / `title` だけで、Notion が返す JSON には `multi_select` キーしか無い（`prop["rich_text"]` も `prop["select"]` も**存在しない**）。**選択数に関わらず `repo_hint` は必ず `None`** になっていた。

* **Note**: **壊れ方が「ボードに書いてあるのに使われない」だったので、どこにもエラーが出なかった。** `[llm]` 未設定の環境では `NoLlmRouter` が全タスクを `pending` に落とすだけで、`config validate` も `doctor` も緑のままになる。1 個だけ選んでも読めない点が直感に反するところで、「リポジトリを事前登録すれば単一選択は解決できるはず」という読みが実際には成り立たなかった。

* **Note**: **2 個以上を先頭で代表させない**と決めた。`repo_hint` は文字列 1 つなので先頭を採るのが最短だが、それはページが 2 つのリポジトリを指しているときに、**エージェントを任意の一方に対して黙って走らせる**ことになる。`None` を返してリポジトリ選択へ落とすほうが正直で、`pending` は毎 poll 再評価される（`dispatch.rs:161`）ので、人がボード上で絞れば次の tick で拾われる。1 タスクを複数リポジトリへ展開する設計は #605 に切り出した。

* **Note**: `repo_allowed()` は `[[repositories]].name` と**完全一致**で比較する（`config.rs:317`）ので、Notion の option 値をそのまま名前に使う必要がある（`<org>/<repo>` 形式ならその綴りのまま）。既存の設定は `hakoniwa` / `totsuka` のような裸の名前なので、同じ config の中で 2 つの命名流儀が混ざる。正規化を入れるかどうかは #604 では決めていない。
