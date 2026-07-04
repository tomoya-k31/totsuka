# qa-service: 自分宛メンションのカンペ回答(self-mention watch)— 実装プラン

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 同僚があなた(`self_mention_user_id`)をメンションした発言を User トークン購読で検知し、bot を lazy join/invite でチャンネルに入れて、あなたにだけ見えるカンペ(エフェメラル + DM)を返す。join システムメッセージは best-effort で自動削除する。

**Architecture:** 検知は既存 Socket Mode(user events が同じ WebSocket に届く。envelope はほぼ不変、`channel_join` の 1 分岐のみ追加)。`question_filter` に `SelfMention` トリガーを追加し、`channel_entry.rs` が join→invite→DM-only のフォールバックを担う。回答は既存パイプラインを `author`/`dm_only` の 2 フィールド追加で共用。xoxp(user トークン)は private への invite と join メッセージ削除の 2 箇所でだけ使う第 2 の `HttpSlackClient` インスタンス。

**Tech Stack:** Rust / tokio / 既存 `post_form` パターン / serde(config)

**スペック:** `docs/superpowers/specs/2026-07-05-qa-self-mention-watch-design.md`(gitignore 対象・リポジトリ外)

## Global Constraints

- `#![forbid(unsafe_code)]`;`Utc::now()` / `SystemTime::now()` を production コードで呼ばない(`Instant::now()` は可 — pipeline.rs に前例あり)
- clippy は CI 相当の `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` で確認
- DB 依存テストは `DATABASE_URL` 必須。`DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka`(dev コンテナ稼働中)。「skipping」が出たらテストは走っていない
- コミットは Conventional Commits + **gpgsign 無効**: `git -c commit.gpgsign=false commit ...`
- 作業ブランチ: `feat/qa-self-mention-watch`(main から作成)
- SelfMention の回答は **recipient(自分)にだけ**見える。`default_mode` に関係なく **Delegated 強制**
- xoxp 未設定(空)なら invite・削除は無効化され、private では DM のみにフォールバック

## 前提知識(このリポジトリ固有)

- `SlackClient` トレイト: `crates/qa-service/src/slack/mod.rs`。HTTP 実装 `web.rs`(`post_form` ヘルパ)、モック `mock.rs`(`Mutex<MockState>` に記録、`set_fail_*` で失敗注入)
- HTTP スタブテスト: `crates/qa-service/tests/slack_web.rs` の `one_shot_stub`(1 リクエスト受けて固定 JSON を返す)
- dispatch ループは `crates/qa-service/src/main.rs:211-281`(`SlackEvent` を受けて filter → classify → `handle_answer` を spawn)
- e2e は `totsuka_testkit::ephemeral_db()` で DB 必須、未設定なら silent skip

---

### Task 1: config — `self_mention_user_id` / `slack_user_token`

**Files:**
- Modify: `crates/totsuka-config/src/schema.rs:308-323`(`QaServiceSection`)
- Modify: `crates/totsuka-config/tests/example_parses.rs`(デフォルト値テスト追加)
- Modify: `examples/totsuka.toml.example`(`[qa_service]` セクション、`repo_select_mode` 行の後)

**Interfaces:**
- Produces: `QaServiceSection.self_mention_user_id: String`(default `""`)、`QaServiceSection.slack_user_token: Secret<String>`(default 空)。Task 7 の main.rs が読む

- [ ] **Step 1: デフォルト値テストを書く**

`crates/totsuka-config/tests/example_parses.rs` の末尾に追加:

```rust
#[test]
fn self_mention_defaults_disabled() {
    let toml = render("/tmp/sock/a.sock", "/tmp/sock/o.sock", "");
    let cfg = Config::from_toml_str(&toml).expect("parse");
    assert_eq!(cfg.qa_service.self_mention_user_id, "");
    assert!(cfg.qa_service.slack_user_token.expose().is_empty());
}
```

- [ ] **Step 2: red を確認**

Run: `cargo test -p totsuka-config --test example_parses self_mention_defaults_disabled`
Expected: コンパイルエラー `no field self_mention_user_id`

- [ ] **Step 3: フィールド追加**

`crates/totsuka-config/src/schema.rs` の `QaServiceSection` 内、`repo_select_mode` の直後に追加(`default_secret` は同ファイル既存):

```rust
    /// 自分宛メンション監視の対象ユーザー ID(空文字 = 機能無効)。
    /// このユーザー宛のメンションを検知すると、本人にだけ見えるカンペ回答を返す。
    #[serde(default)]
    pub self_mention_user_id: String,
    /// User OAuth Token(xoxp)。private チャンネルへの bot 招待と
    /// join システムメッセージ削除にのみ使用(空 = 両機能無効)。
    #[serde(default = "default_secret")]
    pub slack_user_token: Secret<String>,
```

- [ ] **Step 4: example config に追記**

`examples/totsuka.toml.example` の `repo_select_mode      = "llm_classify"   # ...` 行の直後に追加:

```toml
self_mention_user_id  = ""               # 自分宛メンション監視の対象ユーザー ID (空 = 無効)。回答は本人だけに見える
# slack_user_token は secrets.toml の [qa_service] に置く (xoxp-、private への bot 招待と join メッセージ削除に使用)
```

- [ ] **Step 5: green を確認**

Run: `cargo test -p totsuka-config && cargo build --workspace`
Expected: 全 PASS・ビルド成功(`QaServiceSection` は serde 経由でのみ構築されるため他クレートの追随は不要)

- [ ] **Step 6: コミット**

```bash
git add crates/totsuka-config/src/schema.rs crates/totsuka-config/tests/example_parses.rs examples/totsuka.toml.example
git -c commit.gpgsign=false commit -m "feat(totsuka-config): add self_mention_user_id and slack_user_token"
```

---

### Task 2: `SlackClient` — `join_channel` / `invite_users` / `delete_message`

**Files:**
- Modify: `crates/qa-service/src/slack/mod.rs`(トレイト、`bot_user_id` の直前)
- Modify: `crates/qa-service/src/slack/web.rs`
- Modify: `crates/qa-service/src/slack/mock.rs`
- Test: `crates/qa-service/tests/slack_web.rs`

**Interfaces:**
- Produces(Task 4/7 が使う):
  - `async fn join_channel(&self, channel: &str) -> Result<(), QaError>` — conversations.join(冪等)
  - `async fn invite_users(&self, channel: &str, users: &str) -> Result<(), QaError>` — conversations.invite。`already_in_channel` エラーは Ok に丸める
  - `async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), QaError>` — chat.delete
  - Mock: `joins()` / `invites()` / `deletes()` レコーダ、`set_fail_join(bool)` / `set_fail_invite(bool)` / `set_fail_delete(bool)`。fail_join のエラーは `"conversations.join: method_not_supported_for_channel_type"`、fail_invite は `"conversations.invite: missing_scope"`、fail_delete は `"chat.delete: cant_delete_message"`

- [ ] **Step 1: web スタブテストを書く**

`crates/qa-service/tests/slack_web.rs` の末尾に追加:

```rust
#[tokio::test]
async fn join_channel_ok() {
    let addr = one_shot_stub(r#"{"ok":true,"channel":{"id":"C1"}}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    c.join_channel("C1").await.unwrap();
}

#[tokio::test]
async fn join_channel_errors_on_private() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"method_not_supported_for_channel_type"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let s = c.join_channel("C1").await.unwrap_err().to_string();
    assert!(s.contains("method_not_supported_for_channel_type"), "got: {s}");
}

#[tokio::test]
async fn invite_users_treats_already_in_channel_as_ok() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"already_in_channel"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxp-test".into()),
        Some(format!("http://{addr}/api")),
    );
    c.invite_users("C1", "UBOT").await.unwrap();
}

#[tokio::test]
async fn delete_message_errors_on_cant_delete() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"cant_delete_message"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxp-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let s = c.delete_message("C1", "1.2").await.unwrap_err().to_string();
    assert!(s.contains("cant_delete_message"), "got: {s}");
}
```

- [ ] **Step 2: red を確認**

Run: `cargo test -p qa-service --test slack_web`
Expected: コンパイルエラー `no method named join_channel`

- [ ] **Step 3: トレイトにメソッド追加**

`crates/qa-service/src/slack/mod.rs` の `SlackClient` トレイト内、`bot_user_id` の直前に追加:

```rust
    /// conversations.join — 公開チャンネルへ self-join(冪等)。要 channels:join。
    /// private では method_not_supported_for_channel_type / channel_not_found で失敗する。
    async fn join_channel(&self, channel: &str) -> Result<(), QaError>;

    /// conversations.invite — users をチャンネルに招待。user トークン(xoxp)の
    /// クライアントで呼び、bot を private チャンネルへ入れる用途。要 groups:write。
    /// already_in_channel は成功扱い(冪等)。
    async fn invite_users(&self, channel: &str, users: &str) -> Result<(), QaError>;

    /// chat.delete — join システムメッセージの best-effort 削除に使用。
    /// user トークン(管理者)のクライアントで呼ぶ。
    async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), QaError>;
```

- [ ] **Step 4: `HttpSlackClient` に実装**

`crates/qa-service/src/slack/web.rs` の impl 内、`bot_user_id` の直前に追加:

```rust
    async fn join_channel(&self, channel: &str) -> Result<(), QaError> {
        self.post_form("conversations.join", &[("channel", channel)])
            .await?;
        Ok(())
    }

    async fn invite_users(&self, channel: &str, users: &str) -> Result<(), QaError> {
        match self
            .post_form("conversations.invite", &[("channel", channel), ("users", users)])
            .await
        {
            Ok(_) => Ok(()),
            // 冪等: 既に居るなら目的は達成されている。
            Err(QaError::Slack(ref e)) if e.contains("already_in_channel") => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), QaError> {
        self.post_form("chat.delete", &[("channel", channel), ("ts", ts)])
            .await?;
        Ok(())
    }
```

- [ ] **Step 5: `MockSlackClient` に実装**

`crates/qa-service/src/slack/mock.rs`:

`MockState` に追加(`fail_permalink: bool,` の直後):

```rust
    joins: Vec<String>,
    invites: Vec<(String, String)>,
    deletes: Vec<(String, String)>,
    fail_join: bool,
    fail_invite: bool,
    fail_delete: bool,
```

`impl MockSlackClient` に追加(`set_fail_permalink` の直後):

```rust
    pub fn joins(&self) -> Vec<String> {
        self.state.lock().unwrap().joins.clone()
    }
    pub fn invites(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().invites.clone()
    }
    pub fn deletes(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().deletes.clone()
    }
    pub fn set_fail_join(&self, fail: bool) {
        self.state.lock().unwrap().fail_join = fail;
    }
    pub fn set_fail_invite(&self, fail: bool) {
        self.state.lock().unwrap().fail_invite = fail;
    }
    pub fn set_fail_delete(&self, fail: bool) {
        self.state.lock().unwrap().fail_delete = fail;
    }
```

`impl SlackClient for MockSlackClient` に追加(`bot_user_id` の直前):

```rust
    async fn join_channel(&self, channel: &str) -> Result<(), QaError> {
        let mut s = self.state.lock().unwrap();
        if s.fail_join {
            return Err(QaError::Slack(
                "conversations.join: method_not_supported_for_channel_type".into(),
            ));
        }
        s.joins.push(channel.into());
        Ok(())
    }

    async fn invite_users(&self, channel: &str, users: &str) -> Result<(), QaError> {
        let mut s = self.state.lock().unwrap();
        if s.fail_invite {
            return Err(QaError::Slack("conversations.invite: missing_scope".into()));
        }
        s.invites.push((channel.into(), users.into()));
        Ok(())
    }

    async fn delete_message(&self, channel: &str, ts: &str) -> Result<(), QaError> {
        let mut s = self.state.lock().unwrap();
        if s.fail_delete {
            return Err(QaError::Slack("chat.delete: cant_delete_message".into()));
        }
        s.deletes.push((channel.into(), ts.into()));
        Ok(())
    }
```

- [ ] **Step 6: green を確認**

Run: `cargo test -p qa-service --test slack_web && cargo build -p qa-service --tests`
Expected: 新 4 テスト PASS、コンパイル成功

- [ ] **Step 7: コミット**

```bash
git add crates/qa-service/src/slack/ crates/qa-service/tests/slack_web.rs
git -c commit.gpgsign=false commit -m "feat(qa-service): SlackClient join_channel/invite_users/delete_message"
```

---

### Task 3: envelope — `BotJoined` イベント

**Files:**
- Modify: `crates/qa-service/src/slack/envelope.rs`
- Test: `crates/qa-service/tests/slack_envelope.rs`

**Interfaces:**
- Produces(Task 7 が使う): `SlackEvent::BotJoined { channel: String, ts: String, user: String }` — `subtype == "channel_join"` の message。user が bot かどうかの判定は呼び出し側(dispatch)が行う

- [ ] **Step 1: テストを書く**

`crates/qa-service/tests/slack_envelope.rs` の末尾に追加:

```rust
#[test]
fn parses_channel_join_as_bot_joined() {
    let raw = r#"{"type":"events_api","envelope_id":"env-j","payload":{
        "event_id":"EvJ1",
        "event":{"type":"message","subtype":"channel_join",
                 "user":"UBOT","channel":"C9","ts":"17500000009.000100",
                 "text":"<@UBOT> has joined the channel"}}}"#;
    match parse(raw).unwrap() {
        SlackEnvelope::EventsApi { event, .. } => assert_eq!(
            event,
            SlackEvent::BotJoined {
                channel: "C9".into(),
                ts: "17500000009.000100".into(),
                user: "UBOT".into(),
            }
        ),
        _ => panic!(),
    }
}
```

既存の `ignores_subtype_messages` テストが `channel_join` を使っている場合は、`message_changed` 等の別 subtype に変更して「channel_join 以外の subtype は従来どおり Other」を維持する(使っていなければ変更不要)。

- [ ] **Step 2: red を確認**

Run: `cargo test -p qa-service --test slack_envelope`
Expected: コンパイルエラー(`BotJoined` 未定義)

- [ ] **Step 3: 実装**

`crates/qa-service/src/slack/envelope.rs` の `SlackEvent` に追加:

```rust
    /// subtype=channel_join の system message。join メッセージの best-effort
    /// 削除に使う。user が bot かどうかは dispatch 側で判定する。
    BotJoined {
        channel: String,
        ts: String,
        user: String,
    },
```

`parse_event` の `Some("message")` arm 先頭(既存の subtype/bot_id フィルタの前)に追加:

```rust
            if ev["subtype"].as_str() == Some("channel_join") {
                return Ok(SlackEvent::BotJoined {
                    channel: ev["channel"].as_str().unwrap_or("").to_string(),
                    ts: ev["ts"].as_str().unwrap_or("").to_string(),
                    user: ev["user"].as_str().unwrap_or("").to_string(),
                });
            }
```

- [ ] **Step 4: green を確認**

Run: `cargo test -p qa-service --test slack_envelope && cargo build -p qa-service`
Expected: 全 PASS。main.rs の `match ev` は `SlackEvent::Other => {}` の catch-all が無い(`Other` を明示)ため、`BotJoined` 追加で **main.rs がコンパイルエラーになる場合**は、main.rs の match に一時的に `SlackEvent::BotJoined { .. } => {}` を足して通す(Task 7 で本実装に置き換える)

- [ ] **Step 5: コミット**

```bash
git add crates/qa-service/src/slack/envelope.rs crates/qa-service/tests/slack_envelope.rs crates/qa-service/src/main.rs
git -c commit.gpgsign=false commit -m "feat(qa-service): parse channel_join system messages as BotJoined"
```

---

### Task 4: `channel_entry.rs` — join-or-invite フォールバック

**Files:**
- Create: `crates/qa-service/src/channel_entry.rs`
- Modify: `crates/qa-service/src/lib.rs`(`pub mod channel_entry;` を `pub mod catchup;` の前に追加)

**Interfaces:**
- Consumes: `SlackClient::{join_channel, invite_users}`(Task 2)
- Produces(Task 7 が使う):
  - `pub enum ChannelEntry { Full, DmOnly }`
  - `pub async fn ensure_channel_access(bot: &dyn SlackClient, user: Option<&dyn SlackClient>, channel: &str, bot_user_id: &str) -> ChannelEntry`

- [ ] **Step 1: テスト付きで新ファイル作成(実装は todo!())**

`crates/qa-service/src/channel_entry.rs`:

```rust
//! SelfMention 回答前のチャンネル参加確保。
//! 公開: conversations.join(bot)。private: conversations.invite(user トークン、
//! メンバーである本人名義で bot を招待)。両方失敗なら DM のみで回答する。
//! private チャンネルも ID が `C` 始まりのため事前判別はせず、試行順で解決する。

use crate::slack::SlackClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelEntry {
    /// bot がチャンネルに入った(or 元々居た)— エフェメラル可。
    Full,
    /// 参加手段なし — DM だけで回答する。
    DmOnly,
}

/// join → invite → DmOnly の試行フォールバック。すべて best-effort で、
/// 失敗は warn ログに落として先へ進む(呼び出し元にエラーは返さない)。
pub async fn ensure_channel_access(
    bot: &dyn SlackClient,
    user: Option<&dyn SlackClient>,
    channel: &str,
    bot_user_id: &str,
) -> ChannelEntry {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack::MockSlackClient;

    #[tokio::test]
    async fn public_channel_joins_directly() {
        let bot = MockSlackClient::new();
        let user = MockSlackClient::new();
        let e = ensure_channel_access(&bot, Some(&user), "C1", "UBOT").await;
        assert_eq!(e, ChannelEntry::Full);
        assert_eq!(bot.joins(), vec!["C1".to_string()]);
        assert!(user.invites().is_empty(), "join succeeded; no invite");
    }

    #[tokio::test]
    async fn private_channel_falls_back_to_invite() {
        let bot = MockSlackClient::new();
        bot.set_fail_join(true);
        let user = MockSlackClient::new();
        let e = ensure_channel_access(&bot, Some(&user), "C2", "UBOT").await;
        assert_eq!(e, ChannelEntry::Full);
        assert_eq!(user.invites(), vec![("C2".to_string(), "UBOT".to_string())]);
    }

    #[tokio::test]
    async fn both_fail_means_dm_only() {
        let bot = MockSlackClient::new();
        bot.set_fail_join(true);
        let user = MockSlackClient::new();
        user.set_fail_invite(true);
        let e = ensure_channel_access(&bot, Some(&user), "C3", "UBOT").await;
        assert_eq!(e, ChannelEntry::DmOnly);
    }

    #[tokio::test]
    async fn no_user_client_means_dm_only_on_join_failure() {
        let bot = MockSlackClient::new();
        bot.set_fail_join(true);
        let e = ensure_channel_access(&bot, None, "C4", "UBOT").await;
        assert_eq!(e, ChannelEntry::DmOnly);
    }
}
```

- [ ] **Step 2: red を確認**

Run: `cargo test -p qa-service --lib channel_entry`
Expected: FAIL(todo! panic ×4。コンパイルは通ること)

- [ ] **Step 3: 実装**

```rust
pub async fn ensure_channel_access(
    bot: &dyn SlackClient,
    user: Option<&dyn SlackClient>,
    channel: &str,
    bot_user_id: &str,
) -> ChannelEntry {
    match bot.join_channel(channel).await {
        Ok(()) => return ChannelEntry::Full,
        Err(e) => {
            tracing::debug!(error=%e, channel, "join failed; trying invite (likely private)");
        }
    }
    let Some(user) = user else {
        tracing::warn!(channel, "join failed and no user token; answering via DM only");
        return ChannelEntry::DmOnly;
    };
    match user.invite_users(channel, bot_user_id).await {
        Ok(()) => ChannelEntry::Full,
        Err(e) => {
            tracing::warn!(error=%e, channel, "invite failed; answering via DM only");
            ChannelEntry::DmOnly
        }
    }
}
```

- [ ] **Step 4: green を確認**

Run: `cargo test -p qa-service --lib channel_entry`
Expected: 4 テスト PASS

- [ ] **Step 5: コミット**

```bash
git add crates/qa-service/src/channel_entry.rs crates/qa-service/src/lib.rs
git -c commit.gpgsign=false commit -m "feat(qa-service): channel_entry — join-or-invite fallback for self-mention answers"
```

---

### Task 5: question_filter — `SelfMention` トリガー

**Files:**
- Modify: `crates/qa-service/src/question_filter.rs`
- Modify: `crates/qa-service/tests/question_filter.rs`(既存 5 テストのコンストラクタ追随 + 新テスト)
- Modify: `crates/qa-service/src/main.rs:179-180`(コンストラクタ呼び出しの追随のみ — `""` ではなく config 値を渡すのは Task 7)

**Interfaces:**
- Produces(Task 7 が使う): `Trigger::SelfMention`、`QuestionFilter::new(allowed_user_ids: Vec<String>, bot_user_id: String, self_mention_user_id: String)`(第 3 引数空文字 = SelfMention 無効)
- 優先順位: Mention > SelfMention > ThreadContinuation

- [ ] **Step 1: 既存テストのコンストラクタを追随し、新テストを追加**

`crates/qa-service/tests/question_filter.rs`: 既存 5 テストの `QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into())` をすべて `QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new())` に変更し、末尾に追加:

```rust
#[test]
fn self_mention_fires_for_non_allowed_author() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    // 同僚(allowed 外)が自分をメンション → 発火
    assert_eq!(
        f.evaluate(&msg("U_COLLEAGUE", "<@U_ME> これどうなってる?", None), false),
        Trigger::SelfMention
    );
}

#[test]
fn self_mention_does_not_fire_for_own_message() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f.evaluate(&msg("U_ME", "<@U_ME> メモ", None), false),
        Trigger::None
    );
}

#[test]
fn bot_mention_takes_precedence_over_self_mention() {
    // allowed ユーザーが bot と自分の両方をメンション → 既存フロー優先
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ALLOWED".into());
    assert_eq!(
        f.evaluate(&msg("U_OTHER", "<@U_BOT> <@U_ALLOWED> hi", None), false),
        Trigger::SelfMention,
        "bot メンションでも author が allowed 外なら Mention にはならず SelfMention"
    );
    let f2 = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), "U_ME".into());
    assert_eq!(
        f2.evaluate(&msg("U_ALLOWED", "<@U_BOT> <@U_ME> hi", None), false),
        Trigger::Mention
    );
}

#[test]
fn empty_self_mention_id_disables_feature() {
    let f = QuestionFilter::new(vec!["U_ALLOWED".into()], "U_BOT".into(), String::new());
    assert_eq!(
        f.evaluate(&msg("U_COLLEAGUE", "hi <@> there", None), false),
        Trigger::None
    );
}
```

- [ ] **Step 2: red を確認**

Run: `cargo test -p qa-service --test question_filter`
Expected: コンパイルエラー(引数 3 つ / `SelfMention` 未定義)

- [ ] **Step 3: 実装**

`crates/qa-service/src/question_filter.rs` を以下に変更(モジュール doc コメントに SelfMention の 1 行を追記):

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    Mention,
    SelfMention,
    ThreadContinuation,
    None,
}

pub struct QuestionFilter {
    allowed_user_ids: HashSet<String>,
    bot_user_id: String,
    self_mention_user_id: String,
}

impl QuestionFilter {
    pub fn new(
        allowed_user_ids: Vec<String>,
        bot_user_id: String,
        self_mention_user_id: String,
    ) -> Self {
        Self {
            allowed_user_ids: allowed_user_ids.into_iter().collect(),
            bot_user_id,
            self_mention_user_id,
        }
    }

    pub fn evaluate(&self, msg: &SlackMessage, existing_mapping: bool) -> Trigger {
        let allowed = self.allowed_user_ids.contains(&msg.user);
        if allowed && msg.text.contains(&format!("<@{}>", self.bot_user_id)) {
            return Trigger::Mention;
        }
        // SelfMention は allowed_user_ids 外の同僚が対象。自分の発言では発火しない。
        if !self.self_mention_user_id.is_empty()
            && msg.user != self.self_mention_user_id
            && msg
                .text
                .contains(&format!("<@{}>", self.self_mention_user_id))
        {
            return Trigger::SelfMention;
        }
        if allowed && msg.thread_ts.is_some() && existing_mapping {
            return Trigger::ThreadContinuation;
        }
        Trigger::None
    }
}
```

`crates/qa-service/src/main.rs` のコンストラクタ呼び出しを追随(config 配線は Task 7):

```rust
            let filter = QuestionFilter::new(
                config.qa_service.allowed_user_ids.clone(),
                bot_user_id,
                config.qa_service.self_mention_user_id.clone(),
            );
```

module 内の既存 `#[cfg(test)] mod tests`(question_filter.rs 内にあれば)も同様にコンストラクタを追随する。

- [ ] **Step 4: green を確認**

Run: `cargo test -p qa-service --test question_filter && cargo build -p qa-service`
Expected: 既存 5 + 新 4 テスト全 PASS

- [ ] **Step 5: コミット**

```bash
git add crates/qa-service/src/question_filter.rs crates/qa-service/tests/question_filter.rs crates/qa-service/src/main.rs
git -c commit.gpgsign=false commit -m "feat(qa-service): SelfMention trigger — watch mentions of the configured user"
```

---

### Task 6: pipeline — `author`/`dm_only` 分離と DM の from 句

**Files:**
- Modify: `crates/qa-service/src/answer/pipeline.rs`(`AnswerInput` + Delegated arm)
- Modify: `crates/qa-service/src/answer/dm_copy.rs`(`build_dm_text`/`send_dm_copy` に author 追加)
- Modify: `crates/qa-service/src/main.rs`(`AnswerInput` 構築 1 箇所に `author`/`dm_only` 追加)
- Modify: `crates/qa-service/tests/e2e_high_conf_answer.rs`(`AnswerInput` 構築 6 箇所追随 + 新テスト 2 本)
- Modify: `crates/qa-service/tests/e2e_thread_continuation.rs`(`AnswerInput` 構築箇所の追随)

**Interfaces:**
- Consumes: `dm_copy::send_dm_copy`(#36)
- Produces(Task 7 が使う):
  - `AnswerInput { channel, user /* = recipient(回答宛先) */, author /* 質問者 */, thread_ts, question, repo, mode, dm_only }`
  - `pub fn build_dm_text(question: &str, permalink: Option<&str>, answer: &str, author: Option<&str>) -> String` — author ありなら質問行末尾に `(from <@AUTHOR>)`
  - `pub async fn send_dm_copy(slack, user, channel, thread_ts, question, answer, author: Option<&str>)`

- [ ] **Step 1: dm_copy の単体テストを更新・追加**

`crates/qa-service/src/answer/dm_copy.rs` の tests: 既存 7 テストの `build_dm_text(q, p, a)` 呼び出しを `build_dm_text(q, p, a, None)`、`send_dm_copy(..., "A!")` を `send_dm_copy(..., "A!", None)` に変更し、追加:

```rust
    #[test]
    fn dm_text_includes_author_when_present() {
        let t = build_dm_text("q?", Some("https://x/p1"), "A", Some("U_COLLEAGUE"));
        assert_eq!(
            t,
            "💬 *質問:* 「q?」(from <@U_COLLEAGUE>)\n🔗 https://x/p1\n\nA"
        );
    }
```

- [ ] **Step 2: red を確認**

Run: `cargo test -p qa-service --lib dm_copy`
Expected: コンパイルエラー(引数 4 つ)

- [ ] **Step 3: dm_copy 実装を更新**

`build_dm_text` のシグネチャと質問行を変更:

```rust
pub fn build_dm_text(
    question: &str,
    permalink: Option<&str>,
    answer: &str,
    author: Option<&str>,
) -> String {
    let flat = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = flat.chars();
    let mut excerpt: String = chars.by_ref().take(QUESTION_EXCERPT_CHARS).collect();
    if chars.next().is_some() {
        excerpt.push('…');
    }
    let mut out = match author {
        // SelfMention: 誰からの質問かが recipient(自分)に分かるようにする。
        Some(a) => format!("💬 *質問:* 「{excerpt}」(from <@{a}>)\n"),
        None => format!("💬 *質問:* 「{excerpt}」\n"),
    };
    if let Some(link) = permalink {
        out.push_str(&format!("🔗 {link}\n"));
    }
    out.push('\n');
    out.push_str(answer);
    out
}
```

`send_dm_copy` に `author: Option<&str>` を末尾引数で追加し、`build_dm_text(question, permalink.as_deref(), answer, author)` に渡す。

- [ ] **Step 4: pipeline を更新**

`crates/qa-service/src/answer/pipeline.rs`:

`AnswerInput` に追加:

```rust
pub struct AnswerInput {
    pub channel: String,
    /// 回答の宛先(エフェメラル・DM を受け取る人)。
    pub user: String,
    /// 質問の投稿者。既存フローでは user と同一。SelfMention では同僚。
    pub author: String,
    pub thread_ts: String,
    pub question: String,
    pub repo: String,
    pub mode: AnswerMode,
    /// チャンネルに入れなかった(private で invite 失敗等)— エフェメラルを
    /// スキップし DM を主回答チャネルにする。
    pub dm_only: bool,
}
```

Delegated arm を以下に変更:

```rust
        AnswerMode::Delegated => {
            let from = (input.author != input.user).then_some(input.author.as_str());
            if !input.dm_only {
                // Address the asker explicitly: ephemeral messages carry no
                // notification badge of their own, so the leading mention is
                // what makes the answer discoverable in a busy thread.
                let mention_text = match from {
                    Some(a) => {
                        format!("<@{}> *<@{}> からの質問への回答:*\n{}", input.user, a, text)
                    }
                    None => format!("<@{}> {}", input.user, text),
                };
                ctx.slack
                    .post_ephemeral(
                        &input.channel,
                        &input.user,
                        Some(&input.thread_ts),
                        &mention_text,
                    )
                    .await?;
            }
            // The ephemeral above evaporates on reload and never notifies —
            // the DM copy is the durable, notifying record (best-effort).
            // dm_only のときは DM が唯一の回答経路なので flag に関係なく送る。
            if ctx.answer_cfg.dm_copy_enabled || input.dm_only {
                if let Err(e) = super::dm_copy::send_dm_copy(
                    ctx.slack.as_ref(),
                    &input.user,
                    &input.channel,
                    &input.thread_ts,
                    &input.question,
                    &text,
                    from,
                )
                .await
                {
                    tracing::warn!(error=%e, thread_ts=%input.thread_ts, "DM copy failed");
                }
            }
            SlackPostResult {
                ts: format!("ephemeral-{}", input.thread_ts),
            }
        }
```

`crates/qa-service/src/main.rs` の `AnswerInput` 構築に `author: m.user.clone(), dm_only: false,` を追加(`user: m.user.clone(),` の直後。SelfMention 用の値は Task 7)。

- [ ] **Step 5: e2e テストを追随・追加**

`crates/qa-service/tests/e2e_high_conf_answer.rs` と `e2e_thread_continuation.rs` の全 `AnswerInput { ... }` に `author: "U1".into(), dm_only: false,` を追加(`user:` の値と同じ ID を使う。`e2e_thread_continuation.rs` で user が別値ならそれに合わせる)。

`e2e_high_conf_answer.rs` の末尾に追加(冒頭 `use` に変更不要、既存パターンを踏襲):

```rust
#[tokio::test]
async fn self_mention_style_answer_targets_recipient_not_author() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_sm1".into(),
        terminal_id: "term_e2e_sm1".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: answer_cfg(),
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C1".into(),
        user: "U_ME".into(),        // recipient = 自分
        author: "U_COLLEAGUE".into(), // 質問者 = 同僚
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: false,
    };
    handle_answer(&ctx, input).await.unwrap();

    // エフェメラルは自分宛・from 句付き。
    let ephemerals = slack.ephemerals();
    assert_eq!(ephemerals.len(), 1);
    assert_eq!(ephemerals[0].1, "U_ME");
    assert!(ephemerals[0].3.contains("<@U_COLLEAGUE> からの質問"), "got: {}", ephemerals[0].3);
    // DM も自分宛(D_U_ME)・from 句付き。
    let posts = slack.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].0, "D_U_ME");
    assert!(posts[0].2.contains("(from <@U_COLLEAGUE>)"), "got: {}", posts[0].2);

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn dm_only_skips_ephemeral_and_sends_dm_even_when_copy_disabled() {
    let Some(db) = totsuka_testkit::ephemeral_db().await else {
        eprintln!("DATABASE_URL not set, skipping");
        return;
    };
    let pool = db.pool.clone();
    let clock = Arc::new(SystemClock);

    let adapter = Arc::new(MockAdapter::new());
    adapter.set_spawn_response(SpawnRes {
        agent_id: "agent_e2e_sm2".into(),
        terminal_id: "term_e2e_sm2".into(),
        worktree_path: "/tmp/wt".into(),
    });
    adapter.set_read_response(ReadRes {
        revision: 1,
        text: "<answer>OK</answer><<TOTSUKA_DONE>>".into(),
        is_newer: true,
    });

    let slack = Arc::new(MockSlackClient::new());
    let thread_map = Arc::new(ThreadMapRepo::new(pool.clone(), clock.clone()));
    let thread_history = Arc::new(ThreadHistoryRepo::new(pool.clone(), clock.clone()));
    let thread_ts = format!("e2e_{}", uuid::Uuid::new_v4().simple());

    let mut cfg = answer_cfg();
    cfg.dm_copy_enabled = false; // dm_only は flag に優先して DM を送る
    let ctx = AnswerCtx {
        adapter: adapter.clone() as Arc<dyn AdapterClient>,
        slack: slack.clone() as Arc<dyn SlackClient>,
        thread_map: thread_map.clone(),
        thread_history: thread_history.clone(),
        clock: clock.clone(),
        answer_cfg: cfg,
        system_prompt_template: "answer with {open_tag}…{close_tag}+{sentinel}".into(),
    };
    let input = AnswerInput {
        channel: "C_PRIVATE".into(),
        user: "U_ME".into(),
        author: "U_COLLEAGUE".into(),
        thread_ts: thread_ts.clone(),
        question: "where is auth?".into(),
        repo: "acme/api".into(),
        mode: AnswerMode::Delegated,
        dm_only: true,
    };
    let outcome = handle_answer(&ctx, input).await.unwrap();
    assert!(matches!(
        outcome,
        qa_service::answer::pipeline::AnswerOutcome::Posted { .. }
    ));

    assert!(slack.ephemerals().is_empty(), "dm_only must skip ephemeral");
    let posts = slack.posts();
    assert_eq!(posts.len(), 1, "DM is the sole answer channel");
    assert_eq!(posts[0].0, "D_U_ME");

    sqlx::query("DELETE FROM qa_thread_agent WHERE thread_ts = $1")
        .bind(&thread_ts)
        .execute(&pool)
        .await
        .unwrap();
}
```

- [ ] **Step 6: green を確認**

Run: `DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test -p qa-service`
Expected: 全 PASS、skip なし(既存 delegated テストは author == user のため from 句なし・期待値不変)

- [ ] **Step 7: コミット**

```bash
git add crates/qa-service/src/answer/ crates/qa-service/src/main.rs crates/qa-service/tests/e2e_high_conf_answer.rs crates/qa-service/tests/e2e_thread_continuation.rs
git -c commit.gpgsign=false commit -m "feat(qa-service): split answer author/recipient; dm_only fallback with from-attribution"
```

---

### Task 7: main.rs 配線 — SelfMention dispatch + join メッセージ削除

**Files:**
- Modify: `crates/qa-service/src/main.rs`

**Interfaces:**
- Consumes: Task 1-6 の全成果物(`self_mention_user_id` / `slack_user_token` / `ensure_channel_access` / `Trigger::SelfMention` / `BotJoined` / `AnswerInput.author/dm_only`)
- Produces: なし(配線のみ。ロジックは各 lib モジュールでテスト済み — dispatch ループ自体は既存慣行どおり main.rs 内で、実機検証は Task 8)

- [ ] **Step 1: user トークンクライアントの構築**

`main.rs` の `let slack: Arc<dyn SlackClient> = ...`(70-73 行目)の直後に追加:

```rust
    // xoxp(user トークン)クライアント: private への bot 招待と join メッセージ
    // 削除にのみ使用。未設定なら None(private は DM フォールバック)。
    let user_slack: Option<Arc<dyn SlackClient>> = {
        let t = config.qa_service.slack_user_token.clone();
        if t.expose().is_empty() {
            None
        } else {
            Some(Arc::new(HttpSlackClient::new(t, None)))
        }
    };
```

- [ ] **Step 2: dispatch ループへ配線**

`dispatch_h` ブロックの clone 群に `let user_slack = user_slack.clone();` と `let self_mention_user_id = config.qa_service.self_mention_user_id.clone();` を追加し、ループ前に pending マップを用意:

```rust
            // join/invite 直後の channel_join システムメッセージを best-effort 削除
            // するための照合マップ(channel → 記録時刻)。BotJoined 到着時に消費。
            let mut pending_join_delete: std::collections::HashMap<String, std::time::Instant> =
                std::collections::HashMap::new();
```

(dispatch ループは単一タスクなので `Mutex` 不要 — `mut` ローカルでよい)

`SlackEvent::Message(m)` arm の `if trig == Trigger::None { continue; }` の直後に追加:

```rust
                                // SelfMention: 回答前にチャンネル参加を確保する。
                                // 分類の前に行う — 低確信度通知(エフェメラル)も参加が前提のため。
                                let (recipient, author, dm_only) = if trig == Trigger::SelfMention {
                                    let entry = qa_service::channel_entry::ensure_channel_access(
                                        slack.as_ref(),
                                        user_slack.as_deref(),
                                        &m.channel,
                                        &bot_user_id_dispatch,
                                    )
                                    .await;
                                    if entry == qa_service::channel_entry::ChannelEntry::Full {
                                        pending_join_delete.insert(m.channel.clone(), std::time::Instant::now());
                                    }
                                    (
                                        self_mention_user_id.clone(),
                                        m.user.clone(),
                                        entry == qa_service::channel_entry::ChannelEntry::DmOnly,
                                    )
                                } else {
                                    (m.user.clone(), m.user.clone(), false)
                                };
```

注意: `bot_user_id` は filter 構築で move 済みのため、`QuestionFilter::new(..., bot_user_id.clone(), ...)` に変えた上で `let bot_user_id_dispatch = bot_user_id;` をループ前に置く(あるいは filter へ渡す前に clone)。既存の `let bot_user_id = bot_user_id.clone();`(176 行目、spawn 用 clone)との整合はコンパイラに従って調整する。

低確信度通知の宛先を recipient に変更(`&m.user` → `&recipient`):

```rust
                                        if let Err(e) = slack.post_ephemeral(
                                            &m.channel, &recipient, Some(&thread_key),
                                            "リポジトリを特定できませんでした。明示的に指定してください。",
                                        ).await {
```

`AnswerInput` 構築を変更:

```rust
                                let input = AnswerInput {
                                    channel: m.channel.clone(),
                                    user: recipient,
                                    author,
                                    thread_ts: thread_key,
                                    question: m.text.clone(),
                                    repo,
                                    // SelfMention の回答は本人にだけ見せる — default_mode に
                                    // かかわらず Delegated 強制。
                                    mode: if trig == Trigger::SelfMention { AnswerMode::Delegated } else { mode },
                                    dm_only,
                                };
```

- [ ] **Step 3: BotJoined arm を実装**

`SlackEvent::ReactionAdded { .. }` arm の後(Task 3 で仮置きした `BotJoined` arm を置き換え):

```rust
                            SlackEvent::BotJoined { channel, ts, user } => {
                                // 自分(bot)の join メッセージ、かつ直近に join/invite した
                                // チャンネルのものだけ削除する(他人の join には触らない)。
                                const PENDING_TTL: std::time::Duration = std::time::Duration::from_secs(300);
                                pending_join_delete.retain(|_, t| t.elapsed() < PENDING_TTL);
                                if user == bot_user_id_dispatch {
                                    if let Some(u) = pending_join_delete
                                        .remove(&channel)
                                        .and(user_slack.as_ref())
                                    {
                                        if let Err(e) = u.delete_message(&channel, &ts).await {
                                            tracing::warn!(error=%e, %channel,
                                                "join message delete failed (best-effort)");
                                        }
                                    }
                                }
                            }
```

- [ ] **Step 4: 全体 green + clippy を確認**

Run:
```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --check
```
Expected: 全 PASS・警告ゼロ(`HashMap::remove(...).and(...)` の型が合わない場合は素直に `if pending_join_delete.remove(&channel).is_some() { if let Some(u) = user_slack.as_ref() { ... } }` に展開してよい — 動作は同一)

- [ ] **Step 5: コミット**

```bash
git add crates/qa-service/src/main.rs
git -c commit.gpgsign=false commit -m "feat(qa-service): wire self-mention watch — lazy channel entry, forced-delegated answers, join-message cleanup"
```

---

### Task 8: ドキュメント更新(slack-app-setup.md)+ 最終検証

**Files:**
- Modify: `docs/slack-app-setup.md`

**Interfaces:** なし(ドキュメントのみ)

- [ ] **Step 1: マニフェスト YAML を更新**

`docs/slack-app-setup.md` のマニフェスト YAML を以下に置き換え(bot スコープ追加 + user スコープ/イベント新設):

```yaml
display_information:
  name: totsuka
  description: Local agent orchestration QA bot
features:
  bot_user:
    display_name: totsuka
    always_online: true
oauth_config:
  scopes:
    bot:
      - chat:write        # chat.postMessage / chat.postEphemeral(回答投稿)
      - im:write          # conversations.open(Delegated 回答の DM コピー)
      - reactions:write   # reactions.add(受付リアクション)
      - reactions:read    # reaction_added イベント受信(GitHub issue 起票トリガー)
      - channels:history  # conversations.history / replies + message.channels 受信
      - groups:history    # ↑の private チャンネル版(使わないなら削除可)
      - channels:join     # conversations.join(self-mention 検知時の lazy join)
    user:                 # self-mention watch 用(使わないならセクションごと削除可)
      - channels:history  # あなたが参加する public チャンネルの発言イベント
      - groups:history    # あなたが参加する private チャンネルの発言イベント
      - groups:write      # conversations.invite(private への bot 自動招待)
      - chat:write        # chat.delete(join システムメッセージの自動削除)
settings:
  event_subscriptions:
    bot_events:
      - message.channels  # public チャンネルの発言(質問トリガー)
      - message.groups    # private チャンネルの発言(使わないなら削除可)
      - reaction_added    # reaction_trigger(既定 "memo")による issue 起票
    user_events:          # self-mention watch 用: あなたが見える範囲の発言
      - message.channels
      - message.groups
  socket_mode_enabled: true
  interactivity:
    is_enabled: false
```

- [ ] **Step 2: 「User トークン(xoxp)の取得」セクションを追加**

「## 3. ワークスペースにインストールする」の直後に新セクションを挿入:

```markdown
## 3.5 User OAuth Token を取得する(`xoxp-`、self-mention watch を使う場合のみ)

self-mention watch(自分宛メンションのカンペ回答)を使う場合、User トークンが必要になる。
OAuth リダイレクトフローの実装は不要 — アプリ設定画面からの再インストールだけで発行される:

1. **OAuth & Permissions → User Token Scopes** に `channels:history` / `groups:history` /
   `groups:write` / `chat:write` を追加(マニフェストから作成した場合は設定済み)
2. **Event Subscriptions → Subscribe to events on behalf of users** に
   `message.channels` / `message.groups` を追加(同上)
3. **Reinstall to Workspace** — 認可画面に「あなたのユーザーとしてのアクセス」が
   表示されるので許可する。**必ず監視対象ユーザー本人(管理者)のアカウントで操作する**
   (トークンは認可したユーザーに紐づき、イベントも「そのユーザーが見える範囲」になる)
4. **OAuth & Permissions** ページ上部に現れた **User OAuth Token**(`xoxp-...`)を
   `secrets.toml` の `[qa_service] slack_user_token` に設定する

注意:
- Token Rotation は有効にしない(refresh フロー未実装のため失効するようになる)
- xoxp はあなたの閲覧権限そのもの。`secrets.toml`(0600)か `op://` 参照で管理する
- アプリを再認可すると xoxp は再発行される — その際は secrets.toml も更新する
```

- [ ] **Step 3: config 説明と機能説明を更新**

「## 6. config.toml 側の設定」の TOML 例に 1 行追加:

```toml
self_mention_user_id  = "U08XXXXXXXX"    # 自分宛メンション監視 (空 = 無効)。同僚があなたをメンションすると本人だけに見えるカンペ回答が届く
```

同セクション末尾に挙動説明を追加:

```markdown
### self-mention watch の挙動

`self_mention_user_id` を設定すると、**bot がチャンネルに参加していなくても**、あなたが
参加している全チャンネルのあなた宛メンションを検知して回答を用意する(User トークンの
イベント購読による。事前の `/invite` 行脚は不要):

1. 同僚が `@あなた <質問>` を投稿 → 検知
2. bot がそのチャンネルへ自動参加(public: self-join / private: あなた名義で自動招待。
   `slack_user_token` 未設定なら private では参加せず DM のみで回答)
3. 「参加しました」システムメッセージは best-effort で自動削除(管理者 xoxp の chat.delete。
   ワークスペースの「メッセージの削除」設定によっては削除できず残る)
4. 回答は **あなたにだけ**届く: スレッド内エフェメラル(質問者名付き)+ Bot DM の永続コピー

制約: bot はチャンネルのメンバー一覧には表示される(完全に隠す手段はない)。
回答は `default_mode` にかかわらず常に delegated(非公開)。
```

- [ ] **Step 4: トラブルシュート表に行を追加**

既存の表に追加:

```markdown
| 自分宛メンションに反応しない | `self_mention_user_id` 未設定 / user events(`message.channels` 等)未購読 / 再インストール・ユーザー認可漏れ |
| private で回答が DM だけになる | `slack_user_token` 未設定、または user scope `groups:write` なし |
| 「参加しました」メッセージが残る | user scope `chat:write` なし / ワークスペース設定で管理者のメッセージ削除が不許可(best-effort のため残置は仕様内) |
```

- [ ] **Step 5: 最終検証とコミット**

Run:
```bash
DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/totsuka cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --check
\grep -rn "Utc::now()\|SystemTime::now()" crates/qa-service/src/channel_entry.rs crates/qa-service/src/question_filter.rs crates/qa-service/src/slack/ crates/qa-service/src/answer/
```
Expected: 全 PASS・警告ゼロ・時刻 API ヒットなし

```bash
git add docs/slack-app-setup.md
git -c commit.gpgsign=false commit -m "docs: self-mention watch — user token scopes, xoxp acquisition, troubleshooting"
```

---

## 運用作業(コード外 — PR 説明に記載)

1. Slack アプリ: Bot scope `channels:join`、User scopes `channels:history`/`groups:history`/`groups:write`/`chat:write`、user events `message.channels`/`message.groups` を追加
2. **本人のアカウントで**再インストール + 認可 → xoxp を `secrets.toml` の `slack_user_token` へ
3. `config.toml` に `self_mention_user_id = "U08T7QXPTTK"` を設定して qa-service を再起動
4. 実機検証(スペック §4 のゲート): 同僚アカウント(またはテスト用ユーザー)で private チャンネルにあなた宛メンションを投稿し、(a) bot が自動招待される (b) join メッセージが数秒で消える (c) エフェメラル+DM が届く、を確認。join メッセージが `cant_delete_message` で消えない場合は warn ログに残る — その場合は削除機能だけ諦める判断を後日行う(コードは best-effort なので変更不要)

## Self-Review 済み事項

- スペック全要件のタスク対応: 検知(user events は Slack アプリ設定のみ、コード側は既存 socket 共用)/ SelfMention トリガー = Task 5 / join-or-invite = Task 4 / BotJoined + 削除 = Task 3+7 / recipient 分離・from 句・dm_only = Task 6 / Delegated 強制 = Task 7 / config・secrets = Task 1 / docs = Task 8 / 実機検証ゲート = 運用作業 4
- 型整合: `ensure_channel_access(&dyn SlackClient, Option<&dyn SlackClient>, &str, &str) -> ChannelEntry`(Task 4 定義 = Task 7 使用)、`build_dm_text(…, author: Option<&str>)`(Task 6 内で定義・使用)、`QuestionFilter::new` 3 引数(Task 5 定義 = Task 5/7 使用)で一致確認済み
- main.rs の `bot_user_id` move/clone は既存コードとの兼ね合いがあるためコンパイラ誘導で調整する旨を Task 7 に明記(唯一の「厳密コード未確定」箇所、動作要件は明確)
- リアクション抑止コードは不要(現行回答フローに ack リアクションが存在しないため — スペックに記録済み)
