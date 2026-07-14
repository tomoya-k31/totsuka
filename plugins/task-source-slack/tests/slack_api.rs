//! [`SlackApi`] wrapper coverage over the recorded fake transport: each
//! method's request shape (Web API method, token kind, arguments, retry
//! class), response parsing, and the shared credential-error handling.

mod common;

use serde_json::json;

use common::{Canned, Shared, transport};
use task_source_slack::error::SlackError;
use task_source_slack::slack_api::{PostEphemeral, PostMessage, SlackApi, UpdateMessage};
use task_source_slack::transport::TokenKind;

fn api(shared: &Shared) -> SlackApi<common::FakeTransport> {
    SlackApi::new(transport(shared))
}

// ---------------------------------------------------------------------------
// auth.test / apps.connections.open
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_test_parses_identity() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "user_id": "U_ME" })));

    let identity = api(&shared).auth_test().await.unwrap();
    assert_eq!(identity.user_id, "U_ME");

    let requests = shared.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "auth.test");
    assert_eq!(requests[0].token, TokenKind::User);
    assert!(requests[0].idempotent);
    assert!(requests[0].body.is_none());
}

#[tokio::test]
async fn auth_test_without_user_id_is_invalid_response() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true })));
    let err = api(&shared).auth_test().await.unwrap_err();
    assert!(matches!(err, SlackError::InvalidResponse(_)), "{err}");
}

#[tokio::test]
async fn apps_connections_open_uses_app_token_and_returns_url() {
    let shared = Shared::default();
    shared.push(Canned::Data(
        json!({ "ok": true, "url": "wss://wss.slack.test/link/abc" }),
    ));

    let url = api(&shared).apps_connections_open().await.unwrap();
    assert_eq!(url, "wss://wss.slack.test/link/abc");

    let requests = shared.requests();
    assert_eq!(requests[0].method, "apps.connections.open");
    assert_eq!(requests[0].token, TokenKind::App);
}

#[tokio::test]
async fn apps_connections_open_credential_errors_point_at_the_xapp_token() {
    for code in ["invalid_auth", "token_revoked", "account_inactive"] {
        let shared = Shared::default();
        shared.push(Canned::Data(json!({ "ok": false, "error": code })));

        let err = api(&shared).apps_connections_open().await.unwrap_err();
        assert!(err.is_credential(), "{code}: {err}");
        let message = err.to_string();
        assert!(message.contains("App-Level Token"), "{code}: {message}");
        assert!(message.contains("xapp-"), "{code}: {message}");
        assert!(message.contains("connections:write"), "{code}: {message}");
    }
}

#[tokio::test]
async fn auth_test_treats_any_api_failure_as_credential_class() {
    // auth.test takes no arguments: whatever code comes back, the problem is
    // the token. The TokenGuard's config-vs-internal split relies on this.
    for code in ["token_expired", "not_authed", "org_login_required"] {
        let shared = Shared::default();
        shared.push(Canned::Data(json!({ "ok": false, "error": code })));
        let err = api(&shared).auth_test().await.unwrap_err();
        assert!(err.is_credential(), "{code}: {err}");
        let message = err.to_string();
        assert!(message.contains(code), "{code}: {message}");
        assert!(message.contains("plugins/slack.toml"), "{code}: {message}");
    }
}

// ---------------------------------------------------------------------------
// reads: conversations.replies / conversations.open / users.info / permalink
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversations_replies_parses_messages() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({
        "ok": true,
        "messages": [
            { "user": "U1", "text": "parent", "ts": "1.0", "thread_ts": "1.0" },
            { "user": "U2", "text": "reply", "ts": "2.0", "thread_ts": "1.0" },
            { "subtype": "bot_message", "bot_id": "B1", "text": "bot", "ts": "3.0" },
        ]
    })));

    let messages = api(&shared)
        .conversations_replies("C1", "1.0", 4)
        .await
        .unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].user.as_deref(), Some("U1"));
    assert_eq!(messages[1].text, "reply");
    assert_eq!(messages[1].thread_ts.as_deref(), Some("1.0"));
    assert_eq!(messages[2].subtype.as_deref(), Some("bot_message"));
    assert_eq!(messages[2].bot_id.as_deref(), Some("B1"));
    assert!(messages[2].user.is_none());

    let requests = shared.requests();
    assert_eq!(requests[0].method, "conversations.replies");
    assert!(requests[0].idempotent);
    let body = requests[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["ts"], "1.0");
    assert_eq!(body["limit"], 4);
}

#[tokio::test]
async fn conversations_replies_without_messages_is_invalid_response() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true })));
    let err = api(&shared)
        .conversations_replies("C1", "1.0", 4)
        .await
        .unwrap_err();
    assert!(matches!(err, SlackError::InvalidResponse(_)), "{err}");
}

#[tokio::test]
async fn conversations_open_self_returns_the_dm_channel() {
    let shared = Shared::default();
    shared.push(Canned::Data(
        json!({ "ok": true, "channel": { "id": "D_SELF" } }),
    ));

    let channel = api(&shared).conversations_open_self("U_ME").await.unwrap();
    assert_eq!(channel, "D_SELF");

    let requests = shared.requests();
    assert_eq!(requests[0].method, "conversations.open");
    assert_eq!(requests[0].body.as_ref().unwrap()["users"], "U_ME");
}

#[tokio::test]
async fn users_info_prefers_display_name_then_falls_back() {
    for (profile, expected) in [
        (
            json!({ "display_name": "とも", "real_name": "Tomoya" }),
            "とも",
        ),
        (
            json!({ "display_name": "", "real_name": "Tomoya" }),
            "Tomoya",
        ),
        (
            json!({ "display_name": "", "real_name": "" }),
            "tomoya-account",
        ),
    ] {
        let shared = Shared::default();
        shared.push(Canned::Data(json!({
            "ok": true,
            "user": { "name": "tomoya-account", "profile": profile }
        })));
        let name = api(&shared).users_info("U1").await.unwrap();
        assert_eq!(name, expected);
    }
}

#[tokio::test]
async fn chat_get_permalink_returns_the_link() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({
        "ok": true,
        "permalink": "https://ws.slack.test/archives/C1/p10"
    })));

    let link = api(&shared).chat_get_permalink("C1", "1.0").await.unwrap();
    assert_eq!(link, "https://ws.slack.test/archives/C1/p10");

    let body = shared.requests()[0].body.clone().unwrap();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["message_ts"], "1.0");
}

// ---------------------------------------------------------------------------
// writes: postMessage / postEphemeral / update / response_url
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chat_post_message_is_not_idempotent_and_returns_ts() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "ts": "9.0" })));

    let ts = api(&shared)
        .chat_post_message(&PostMessage {
            channel: "C1",
            text: "reply body",
            thread_ts: Some("1.0"),
            unfurl_links: Some(false),
            blocks: None,
        })
        .await
        .unwrap();
    assert_eq!(ts, "9.0");

    let requests = shared.requests();
    assert_eq!(requests[0].method, "chat.postMessage");
    assert!(!requests[0].idempotent, "posting must never auto-retry");
    let body = requests[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "C1");
    assert_eq!(body["text"], "reply body");
    assert_eq!(body["thread_ts"], "1.0");
    assert_eq!(body["unfurl_links"], false);
}

#[tokio::test]
async fn chat_post_ephemeral_targets_one_user_with_blocks() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "message_ts": "9.1" })));

    let blocks = json!([{ "type": "section", "text": { "type": "mrkdwn", "text": "draft" } }]);
    api(&shared)
        .chat_post_ephemeral(&PostEphemeral {
            channel: "C1",
            user: "U_ME",
            text: "draft fallback",
            thread_ts: Some("1.0"),
            blocks: Some(blocks.clone()),
        })
        .await
        .unwrap();

    let requests = shared.requests();
    assert_eq!(requests[0].method, "chat.postEphemeral");
    assert!(!requests[0].idempotent);
    let body = requests[0].body.as_ref().unwrap();
    assert_eq!(body["user"], "U_ME");
    assert_eq!(body["blocks"], blocks);
}

#[tokio::test]
async fn chat_update_is_idempotent() {
    let shared = Shared::default();
    shared.push(Canned::Data(json!({ "ok": true, "ts": "9.0" })));

    api(&shared)
        .chat_update(&UpdateMessage {
            channel: "D_SELF",
            ts: "9.0",
            text: "✅ sent",
            blocks: None,
        })
        .await
        .unwrap();

    let requests = shared.requests();
    assert_eq!(requests[0].method, "chat.update");
    assert!(requests[0].idempotent);
    let body = requests[0].body.as_ref().unwrap();
    assert_eq!(body["channel"], "D_SELF");
    assert_eq!(body["ts"], "9.0");
}

#[tokio::test]
async fn post_response_url_goes_through_the_url_channel() {
    let shared = Shared::default();
    let payload = json!({ "replace_original": true, "text": "✅ 送信済み" });
    api(&shared)
        .post_response_url("https://hooks.slack.test/r/1", payload.clone())
        .await
        .unwrap();

    let posted = shared.posted_urls();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].url, "https://hooks.slack.test/r/1");
    assert_eq!(posted[0].body, payload);
    // Nothing went through the Web API path.
    assert!(shared.requests().is_empty());
}

// ---------------------------------------------------------------------------
// shared error handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn credential_errors_carry_recovery_guidance_on_any_method() {
    for (code, expect) in [
        ("invalid_auth", "re-issue"),
        ("token_revoked", "re-install"),
        ("account_inactive", "deactivated"),
    ] {
        let shared = Shared::default();
        shared.push(Canned::Data(json!({ "ok": false, "error": code })));
        let err = api(&shared)
            .conversations_replies("C1", "1.0", 4)
            .await
            .unwrap_err();
        assert!(err.is_credential(), "{code}: {err}");
        let message = err.to_string();
        assert!(message.contains(code), "{code}: {message}");
        assert!(message.contains(expect), "{code}: {message}");
    }
}

#[tokio::test]
async fn non_credential_api_errors_pass_through() {
    let shared = Shared::default();
    shared.push(Canned::Data(
        json!({ "ok": false, "error": "channel_not_found" }),
    ));
    let err = api(&shared)
        .chat_post_message(&PostMessage {
            channel: "C_GONE",
            text: "x",
            thread_ts: None,
            unfurl_links: None,
            blocks: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, SlackError::Api { ref error, .. } if error == "channel_not_found"),
        "{err}"
    );
    assert!(!err.is_credential());
}
