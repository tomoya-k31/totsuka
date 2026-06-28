//! Regression: malicious project_node_id / cursor must land in
//! `variables.input`, never in the `query` string. Same shape as
//! crates/orchestrator/src/gh_writeback/http.rs after PR #4.

use github_watcher::gh_client::{GhClient, HttpGhClient};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

#[tokio::test]
async fn malicious_project_id_lands_in_variables_not_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut content_length = 0usize;
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
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 31\r\n\r\n\
              {\"data\":{\"node\":{\"items\":{}}}}",
            )
            .await
            .unwrap();
        buf
    });

    let client = HttpGhClient::with_endpoints(
        Secret::new("tok".into()),
        format!("http://{addr}/graphql"),
        format!("http://{addr}"),
    );

    let evil = r#""}}, fakeField: 1, x:"#;
    // Will return GraphQl error because the fake server's response shape is incomplete,
    // but that's fine — we only care about the WIRE BODY this method emits.
    let _ = client.project_items_page(evil, Some(evil), 100).await;

    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let q = body["query"].as_str().expect("query field present");
    assert!(
        !q.contains("fakeField"),
        "query string was contaminated: {q}"
    );
    assert!(!q.contains(evil), "query string echoed evil verbatim: {q}");
    assert_eq!(body["variables"]["projectId"], evil);
    assert_eq!(body["variables"]["after"], evil);
    assert_eq!(body["variables"]["first"], 100);
}
