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
            if n == 0 || line == "\r\n" { break; }
            if let Some(v) = line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")) {
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
