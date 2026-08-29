* **Update**: [task-source-github](/components/task-source-github.md) / [task-source-notion](/components/task-source-notion.md) / [task-source-slack](/components/task-source-slack.md) の **3 つとも**、タイムアウト → `Timeout` の写像が一度も検査されていなかった。30 秒が `ReqwestTransport::new` にハードコードで短くする手段が無く、固定すると 1 テストに 30 秒かかるためである。テスト用フック `with_timeout` を足して塞いだ。各 3 本 —— `Timeout` へ写ること、**非冪等では再送しないこと**、冪等では再送すること。

* **Note**: **タイムアウトとスロットルは、どちらも retryable でありながら再送可否が逆になる**。スロットル（`is_rejected`）は要求が実行されていないので非冪等でも replay してよい。タイムアウトは**適用済みで応答だけ失われた可能性がある**ので replay してはいけない。この対比が今回一番押さえたかった性質で、実際 `is_rejected` に `Timeout` を混ぜる変異でテストが落ちる。

* **Note**: 応答しないサーバの作り方に一手要る。**ソケットを閉じると reset ＝ `Transport` になってタイムアウトにならない**ので、握ったまま黙る必要がある。ただしタスクを spawn して accept で待たせると、テストより長生きして nextest が `leaky` と報告する。**accept せずに listener を bind するだけ**にすると、カーネルが backlog でハンドシェイクを完了させるのでクライアントは送信して待ち、タスクは 1 つも要らない。listener をテストに返して所有させれば、テスト終了で片付く。

* **Note**: 30 秒ハードコードは **3 プラグイン共通**だった。片方だけ塞ぐと同じクラスの穴が残るので 3 つとも入れた。production の変更は各 1 メソッド（`with_timeout`）だけで、既定値は変えていない。
