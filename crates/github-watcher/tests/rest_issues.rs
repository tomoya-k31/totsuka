use chrono::{TimeZone, Utc};
use github_watcher::gh_client::{GhClient, HttpGhClient, RepoSlug};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const PAYLOAD: &str = r#"[
  {"node_id":"I_a","number":1,"state":"open","updated_at":"2026-06-29T01:00:00Z"},
  {"node_id":"I_b","number":2,"state":"closed","updated_at":"2026-06-29T02:00:00Z","pull_request":{"url":"x"}},
  {"node_id":"I_c","number":3,"state":"open","updated_at":"2026-06-29T03:00:00Z"}
]"#;

#[tokio::test]
async fn issues_since_filters_out_pull_requests() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" {
                break;
            }
        }
        let body = PAYLOAD;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let client = HttpGhClient::with_endpoints(
        Secret::new("tok".into()),
        format!("http://{addr}/graphql"),
        format!("http://{addr}"),
    );
    let repo = RepoSlug::parse("acme/widget").unwrap();
    let since = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    let issues = client.issues_since(&repo, since).await.unwrap();
    server.await.unwrap();

    let ids: Vec<&str> = issues.iter().map(|i| i.node_id.as_str()).collect();
    assert_eq!(ids, vec!["I_a", "I_c"]); // I_b is a PR
    assert_eq!(issues[0].number, 1);
    assert_eq!(issues[0].state, "open");
}
