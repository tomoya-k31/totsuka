# totsuka-testkit — テストごとの一時 DB による dev DB 汚染防止

日付: 2026-07-03
状態: 承認済み

## 背景 / 問題

DB 依存テストは `DATABASE_URL` の指す DB に直接接続し、共有テーブル
（`catchup_cursor`, `gh_item_status`, `tasks` など）へフィクスチャを書き込む。
CI は専用 Postgres なので無害だが、ローカルでは `just pgmq-up` の dev
コンテナ = 稼働スタックと同じ DB を指すため、テスト残骸が実運用状態を汚す。

実害（2026-07-03 に発生）: e2e テストが残したモックカーソル
`projectv2_items = "endCursor-1"` を github-watcher が実 GitHub に送り、
`after does not appear to be a valid cursor` で **project ポーリングが
全 tick 失敗**していた。

## 設計

### 新クレート `crates/totsuka-testkit`

dev-dependency 専用（他クレートの `[dev-dependencies]` からのみ参照）。

```rust
/// DATABASE_URL 未設定なら None（既存の「skip して return」規約を維持）。
pub async fn ephemeral_db() -> Option<EphemeralDb>

pub struct EphemeralDb {
    pub pool: PgPool,   // 一時 DB へ接続済み・migration 適用済み
}
impl EphemeralDb {
    /// 一時 DB の接続 URL。実バイナリを spawn する e2e が子プロセスの
    /// DATABASE_URL として渡す。
    pub fn url(&self) -> &str;
}
```

### `ephemeral_db()` の動作

1. `DATABASE_URL` 未設定 → `None`。
2. admin 接続（DATABASE_URL そのまま）で
   `CREATE DATABASE totsuka_test_<unix秒>_<uuid8>`。
3. 新 DB に接続し `sqlx::migrate!("../../migrations")` を適用
   （`0000_schema_meta.sql` の `CREATE EXTENSION pgmq` により
   pgmq キューもテストごとに独立）。
4. **sweep**: 名前に埋め込んだ unix 秒が 10 分より古い `totsuka_test_*`
   を機会的に `DROP DATABASE`。panic で残った残骸は次の実行で回収される。
   実行中のテストの DB は必ず新しいので誤爆せず、さらにアクティブ接続の
   ある DB はスキップする（長時間のローカルデバッグセッションを保護。
   終了/panic 済みの run は接続を残さないので回収は妨げられない）。
   DROP 対象は本クレートが生成しうる名前形状
   （`totsuka_test_<digits>_<英小文字数字>`、識別子安全な文字のみ）に
   限定し、SQL への識別子注入を防ぐ。

テスト終了時の明示的 DROP は行わない（panic 時に走らず、非同期 Drop の
複雑さに見合わない）。回収は sweep に一本化する。

### テスト側の置換（機械的）

```rust
// before
let Some(url) = std::env::var("DATABASE_URL").ok() else { return };
let pool = PgPoolOptions::new().connect(&url).await.unwrap();

// after
let Some(db) = totsuka_testkit::ephemeral_db().await else {
    eprintln!("DATABASE_URL not set, skipping");
    return;
};
let pool = db.pool.clone();
```

- 対象: DATABASE_URL を参照する全テストファイル（26 ファイル）。
- 実バイナリ spawn 型の e2e は子プロセス env に `db.url()` を渡す。
- 自前で migration/schema 準備をしているテストは testkit の適用済み
  schema に乗り換える。

### 保証

- テストは DATABASE_URL の指す DB 自体へ一切書き込まない
  （同サーバー上に別 DB を作るだけ）→ dev スタックの汚染が構造的に不可能。
- `cargo test`（DATABASE_URL なし）は従来どおり silent skip。
- CI は DATABASE_URL を同様に設定するだけで無変更で動く。

## トレードオフ

- テストごとに DB 作成 + migration（数百 ms）が乗る。DB テストは数十個で
  許容範囲。遅くなったら template database 方式に最適化する余地あり。

## スコープ外

- `#[sqlx::test]` への移行(skip 規約が壊れる)
- justfile / CI の変更（不要）
