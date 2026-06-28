use chrono::{TimeZone, Utc};
use github_watcher::gh_client::{GhClient, HttpGhClient, RepoSlug};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const PAYLOAD: &str = r#"[
  {"node_id":"PR_a","number":11,"head":{"ref":"totsuka/abc123def456/implv"},"body":"hello\n\nTotsuka-Task: PVTI_full_abc123def456","merged_at":"2026-06-29T05:00:00Z","updated_at":"2026-06-29T05:00:00Z"},
  {"node_id":"PR_b","number":12,"head":{"ref":"totsuka/xyz999/design"},"body":null,"updated_at":"2026-06-28T00:00:00Z"}
]"#;

#[tokio::test]
async fn prs_since_filters_old_updates() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut l = String::new();
            let n = reader.read_line(&mut l).await.unwrap();
            if n == 0 || l == "\r\n" {
                break;
            }
        }
        let body = PAYLOAD;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
    });

    let client = HttpGhClient::with_endpoints(
        Secret::new("tok".into()),
        format!("http://{addr}/graphql"),
        format!("http://{addr}"),
    );
    let repo = RepoSlug::parse("acme/widget").unwrap();
    let since = Utc.with_ymd_and_hms(2026, 6, 29, 0, 0, 0).unwrap();
    let prs = client.prs_since(&repo, since).await.unwrap();
    server.await.unwrap();

    assert_eq!(prs.len(), 1);
    let pr = &prs[0];
    assert_eq!(pr.number, 11);
    assert!(pr.merged);
    assert_eq!(pr.head_ref, "totsuka/abc123def456/implv");
    assert!(pr
        .body
        .as_ref()
        .unwrap()
        .contains("Totsuka-Task: PVTI_full_abc123def456"));
}
