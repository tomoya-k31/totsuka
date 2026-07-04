use qa_service::slack::{HttpSlackClient, SlackClient};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

async fn one_shot_stub(payload: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut cl = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line
                .strip_prefix("content-length: ")
                .or_else(|| line.strip_prefix("Content-Length: "))
            {
                cl = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; cl];
        reader.read_exact(&mut body).await.unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            payload.len(),
            payload,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn post_message_returns_ts_on_ok() {
    let addr = one_shot_stub(r#"{"ok":true,"ts":"17500000001.000200"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let r = c.post_message("C1", None, "hi").await.unwrap();
    assert_eq!(r.ts, "17500000001.000200");
}

#[tokio::test]
async fn post_message_errors_on_not_ok() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"channel_not_found"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let err = c.post_message("C1", None, "hi").await.unwrap_err();
    let s = err.to_string();
    assert!(s.contains("channel_not_found"), "got: {s}");
}

#[tokio::test]
async fn history_returns_parsed_messages() {
    let payload = r#"{"ok":true,"messages":[
      {"user":"U1","text":"hello","ts":"17500000001.000100","thread_ts":null},
      {"user":"U2","text":"there","ts":"17500000002.000100","thread_ts":"17500000001.000100"}
    ]}"#;
    let addr = one_shot_stub(payload).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let msgs = c.conversation_history("C1", None, 10).await.unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].text, "hello");
    assert_eq!(msgs[1].thread_ts.as_deref(), Some("17500000001.000100"));
}

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

#[tokio::test]
async fn open_dm_returns_channel_id() {
    let addr = one_shot_stub(r#"{"ok":true,"channel":{"id":"D123"}}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    assert_eq!(c.open_dm("U1").await.unwrap(), "D123");
}

#[tokio::test]
async fn open_dm_errors_on_missing_scope() {
    let addr = one_shot_stub(r#"{"ok":false,"error":"missing_scope"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let s = c.open_dm("U1").await.unwrap_err().to_string();
    assert!(s.contains("missing_scope"), "got: {s}");
}

#[tokio::test]
async fn permalink_returns_url() {
    let addr =
        one_shot_stub(r#"{"ok":true,"permalink":"https://x.slack.com/archives/C1/p123"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    assert_eq!(
        c.permalink("C1", "123.45").await.unwrap(),
        "https://x.slack.com/archives/C1/p123"
    );
}

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
    let addr =
        one_shot_stub(r#"{"ok":false,"error":"method_not_supported_for_channel_type"}"#).await;
    let c = HttpSlackClient::new(
        Secret::new("xoxb-test".into()),
        Some(format!("http://{addr}/api")),
    );
    let s = c.join_channel("C1").await.unwrap_err().to_string();
    assert!(
        s.contains("method_not_supported_for_channel_type"),
        "got: {s}"
    );
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
