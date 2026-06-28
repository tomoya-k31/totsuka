use qa_service::classifier::{AnthropicClassifier, Classifier, ClassifyRequest, RepoCandidate};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const RESP: &str = r#"{
  "content": [
    { "type": "tool_use", "name": "classify_repo",
      "input": { "top_candidates": [
        { "repo": "acme/api", "confidence": 0.91, "rationale": "auth handler lives here" },
        { "repo": "acme/web", "confidence": 0.42, "rationale": "frontend" }
      ] } }
  ]
}"#;

#[tokio::test]
async fn anthropic_forces_tool_use_and_parses_top_candidates() {
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
        let body = RESP;
        let resp = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(resp.as_bytes()).await.unwrap();
        buf
    });

    let c = AnthropicClassifier::new(
        Secret::new("sk-ant-test".into()),
        "claude-haiku-4-5-20251001".into(),
        256,
        3,
        Duration::from_secs(15),
        Some(format!("http://{addr}/v1/messages")),
    );
    let req = ClassifyRequest {
        question: "Where does login live?".into(),
        thread_context: None,
        candidates: vec![
            RepoCandidate {
                repo: "acme/api".into(),
                description: "auth backend".into(),
            },
            RepoCandidate {
                repo: "acme/web".into(),
                description: "frontend".into(),
            },
        ],
    };
    let out = c.classify(req).await.unwrap();
    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();

    // Wire body asserts: tool_choice forced, tool name present, prompt contains repos.
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], "classify_repo");
    assert_eq!(body["tools"][0]["name"], "classify_repo");
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.contains("acme/api"));
    assert!(user.contains("Where does login live?"));

    // Parsed response asserts.
    assert_eq!(out.top_candidates.len(), 2);
    assert_eq!(out.top_candidates[0].repo, "acme/api");
    assert!((out.top_candidates[0].confidence - 0.91).abs() < 1e-9);
    assert_eq!(out.provider, "anthropic");
}
