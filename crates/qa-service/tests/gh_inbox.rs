use qa_service::gh_inbox::GhInboxClient;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

#[tokio::test]
async fn malicious_inputs_land_in_variables_not_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body_resp = r#"{"data":{"addProjectV2DraftIssue":{"projectItem":{"id":"PVTI_OK"}}}}"#;
    let server = tokio::spawn(async move {
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
        let mut buf = vec![0u8; cl];
        reader.read_exact(&mut buf).await.unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body_resp.len(),
            body_resp,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
        buf
    });

    let client = GhInboxClient::new(
        Secret::new("tok".into()),
        Some(format!("http://{addr}/graphql")),
    );
    let evil_id    = r#""}}}) { __typename } mutation Pwn { __typename "#;
    let evil_title = r#"</title><script>alert(1)</script>"#;
    let id = client.create_draft(evil_id, evil_title, "body").await.unwrap();
    assert_eq!(id, "PVTI_OK");

    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

    let q = body["query"].as_str().expect("query string present");
    assert!(!q.contains("__typename"), "query contaminated: {q}");
    assert!(!q.contains("script"),     "query contaminated: {q}");
    assert_eq!(body["variables"]["input"]["projectId"], evil_id);
    assert_eq!(body["variables"]["input"]["title"],     evil_title);
    assert_eq!(body["variables"]["input"]["body"],      "body");
}
