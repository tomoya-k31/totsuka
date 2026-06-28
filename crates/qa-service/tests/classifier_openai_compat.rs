use qa_service::classifier::{ClassifyRequest, Classifier, OpenAiCompatClassifier, RepoCandidate};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use totsuka_core::Secret;

const RESP: &str = r#"{
  "choices": [
    { "message": { "role": "assistant",
                   "content": "{\"top_candidates\":[{\"repo\":\"acme/api\",\"confidence\":0.83,\"rationale\":\"auth\"},{\"repo\":\"acme/web\",\"confidence\":0.21,\"rationale\":\"ui\"}]}" } }
  ]
}"#;

#[tokio::test]
async fn openai_compat_forces_json_schema_and_parses_content() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await.unwrap();
            if n == 0 || line == "\r\n" { break; }
            if let Some(v) = line.strip_prefix("content-length: ").or_else(|| line.strip_prefix("Content-Length: ")) {
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

    let c = OpenAiCompatClassifier::new(
        "openrouter".into(),
        format!("http://{addr}/v1/chat/completions"),
        Secret::new("sk-or-test".into()),
        "anthropic/claude-3-5-haiku".into(),
        256, 3, Duration::from_secs(15),
    );
    let req = ClassifyRequest {
        question: "auth flow?".into(),
        thread_context: None,
        candidates: vec![
            RepoCandidate { repo: "acme/api".into(), description: "backend".into() },
            RepoCandidate { repo: "acme/web".into(), description: "frontend".into() },
        ],
    };
    let out = c.classify(req).await.unwrap();
    let raw = server.await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["name"], "classify_repo");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);

    assert_eq!(out.top_candidates.len(), 2);
    assert_eq!(out.top_candidates[0].repo, "acme/api");
    assert!((out.top_candidates[0].confidence - 0.83).abs() < 1e-9);
    assert_eq!(out.provider, "openrouter");
}
