# qa-service bot_user_id auth.test 自動解決 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** qa-service が起動時に Slack `auth.test` で bot 自身の user ID を取得し、環境変数 `SLACK_BOT_USER_ID` への依存を廃止する。

**Architecture:** `SlackClient` トレイトに `bot_user_id()` を追加し、`HttpSlackClient`(実 API)/`MockSlackClient`(固定値)が実装。`main.rs` は起動シーケンス中に一度呼んで fail-fast し、結果を `QuestionFilter` に渡す。spec: `docs/superpowers/specs/2026-07-04-qa-service-bot-user-id-autoresolve-design.md`。

**Tech Stack:** Rust workspace、`cargo test -p qa-service`、既存 `one_shot_stub` TCP スタブテスト。

## Global Constraints

- `#![forbid(unsafe_code)]`、`Utc::now()`/`SystemTime::now()` 禁止(該当なし)。
- Lint gate: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` + `cargo fmt --check`。
- Commit: `feat(qa-service): ...`、unattended は `git -c commit.gpgsign=false commit`。

---

### Task 1: `SlackClient::bot_user_id` 追加と main.rs 配線

**Files:**
- Modify: `crates/qa-service/src/slack/mod.rs:53` 付近(トレイトにメソッド追加)
- Modify: `crates/qa-service/src/slack/web.rs`(`HttpSlackClient` 実装)
- Modify: `crates/qa-service/src/slack/mock.rs`(`MockSlackClient` 実装)
- Modify: `crates/qa-service/src/main.rs:71` 直後(呼び出し)と `main.rs:173`(env 読み取り削除)
- Test: `crates/qa-service/tests/slack_web.rs`(2 テスト追加)

**Interfaces:**
- Produces: `async fn bot_user_id(&self) -> Result<String, QaError>`(`SlackClient` トレイト、全実装必須)

- [ ] **Step 1: 失敗するテストを書く** — `tests/slack_web.rs` 末尾に追加:

```rust
#[tokio::test]
async fn bot_user_id_returns_user_id_on_ok() {
    let addr = one_shot_stub(r#"{"ok":true,"user_id":"U0BOT"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    assert_eq!(c.bot_user_id().await.unwrap(), "U0BOT");
}

#[tokio::test]
async fn bot_user_id_errors_on_not_ok() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"invalid_auth"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let s = c.bot_user_id().await.unwrap_err().to_string();
    assert!(s.contains("invalid_auth"), "got: {s}");
}
```

- [ ] **Step 2: 失敗確認** — `cargo test -p qa-service --test slack_web`。Expected: コンパイルエラー(`bot_user_id` 未定義)。

- [ ] **Step 3: 実装** — 3 ファイル:

`slack/mod.rs` のトレイト末尾(`add_reaction` の後)に:

```rust
    /// Resolve the bot's own user id (`auth.test`). Called once at startup;
    /// mention detection depends on it, so failure should abort boot.
    async fn bot_user_id(&self) -> Result<String, QaError>;
```

`slack/web.rs` の `impl SlackClient for HttpSlackClient` 末尾に:

```rust
    async fn bot_user_id(&self) -> Result<String, QaError> {
        let v = self.post_form("auth.test", &[]).await?;
        v["user_id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| QaError::Slack("auth.test: missing user_id".into()))
    }
```

`slack/mock.rs` の `impl SlackClient for MockSlackClient` 末尾に:

```rust
    async fn bot_user_id(&self) -> Result<String, QaError> {
        Ok("UBOTMOCK".into())
    }
```

`main.rs`: `let slack: Arc<dyn SlackClient> = ...;`(68-71 行)の直後に:

```rust
    // Mention detection depends on knowing the bot's own user id; resolve it
    // from the token itself so operators don't have to export it manually.
    let bot_user_id = slack.bot_user_id().await?;
    tracing::info!(%bot_user_id, "resolved bot user id via auth.test");
```

`main.rs:171-174` の `QuestionFilter::new(..., std::env::var("SLACK_BOT_USER_ID").unwrap_or_default())` を `QuestionFilter::new(config.qa_service.allowed_user_ids.clone(), bot_user_id.clone())` に変更(クロージャへ move するため spawn 前に `let bot_user_id = bot_user_id.clone();` を他の clone 群と並べる)。

- [ ] **Step 4: 通過確認** — `cargo test -p qa-service`。Expected: 全 PASS。

- [ ] **Step 5: workspace ゲート** — 順に全てクリーン:

```bash
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all && cargo fmt --check
```

- [ ] **Step 6: コミット**

```bash
git add crates/qa-service docs/superpowers/plans/2026-07-04-qa-bot-user-id-autoresolve.md
git -c commit.gpgsign=false commit -m "feat(qa-service): resolve bot user id via auth.test at startup"
```
