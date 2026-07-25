//! SDK behavior tests: typed dispatch, submit retry semantics, poll loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_protocol::Task;
use plugin_protocol::jsonrpc::{Error, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, TriggerInfo,
};
use plugin_sdk::{
    LineHandler, Lookup, LookupClient, SubmitClient, SubmitOutcome, Submitter, TaskSourceHandler,
    TaskSourceServer, Writer, poll_loop,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

fn sample_task(id: &str) -> Task {
    Task {
        id: id.into(),
        source: "test".into(),
        title: format!("task {id}"),
        body: None,
        repo_hint: None,
        labels: vec![],
        priority: 0,
        status: None,
        url: None,
        assignee: None,
        message_key: None,
        instructions: None,
    }
}

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

/// Records which typed methods were called.
#[derive(Default)]
struct Recording {
    calls: Vec<&'static str>,
}

impl TaskSourceHandler for Recording {
    async fn initialize(&mut self, _params: InitializeParams) -> Result<InitializeResult, Error> {
        self.calls.push("initialize");
        Ok(InitializeResult {
            plugin_version: semver::Version::new(0, 1, 0),
            capabilities: Default::default(),
        })
    }

    async fn config_validate(
        &mut self,
        _params: ConfigValidateParams,
    ) -> Result<ConfigValidateResult, Error> {
        self.calls.push("config_validate");
        Ok(ConfigValidateResult {
            valid: true,
            errors: vec![],
        })
    }

    async fn update_status(&mut self, _params: TaskUpdateStatusParams) -> Result<Value, Error> {
        self.calls.push("update_status");
        Ok(Value::Null)
    }

    async fn result_publish(&mut self, _params: ResultPublishParams) -> Result<Value, Error> {
        self.calls.push("result_publish");
        Err(Error::new(error_code::INTERNAL_ERROR, "publish broke"))
    }
}

fn line(v: Value) -> String {
    serde_json::to_string(&v).unwrap()
}

#[tokio::test]
async fn typed_dispatch_covers_the_wire_protocol() {
    let mut server = TaskSourceServer(Recording::default());

    // initialize → typed handler → result with capabilities.
    let reply = server
        .handle_line(&line(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocol_version": "0.1.6", "config": {} }
        })))
        .await;
    let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(response["result"]["plugin_version"], "0.1.0");

    // A handler error becomes the JSON-RPC error response.
    let reply = server
        .handle_line(&line(json!({
            "jsonrpc": "2.0", "id": 2, "method": "result/publish",
            "params": { "task_id": "1", "content": "x" }
        })))
        .await;
    let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], error_code::INTERNAL_ERROR);

    // Invalid params never reach the handler.
    let reply = server
        .handle_line(&line(json!({
            "jsonrpc": "2.0", "id": 3, "method": "task/update_status",
            "params": { "wrong": true }
        })))
        .await;
    let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], error_code::INVALID_PARAMS);

    // Notifications and blank lines are silent; junk is PARSE_ERROR.
    assert!(
        server
            .handle_line(&line(json!({"jsonrpc": "2.0", "method": "notify"})))
            .await
            .line
            .is_none()
    );
    assert!(server.handle_line("   ").await.line.is_none());
    let reply = server.handle_line("not json").await;
    let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], error_code::PARSE_ERROR);

    // Unknown method → METHOD_NOT_FOUND; shutdown → ack + stop flag.
    let reply = server
        .handle_line(&line(json!({"jsonrpc": "2.0", "id": 5, "method": "nope"})))
        .await;
    let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
    assert_eq!(response["error"]["code"], error_code::METHOD_NOT_FOUND);
    let reply = server
        .handle_line(&line(
            json!({"jsonrpc": "2.0", "id": 6, "method": "shutdown"}),
        ))
        .await;
    assert!(reply.shutdown);

    // Invalid params never reached the handler, so `update_status` is absent.
    assert_eq!(server.0.calls, vec!["initialize", "result_publish"]);
}

// ---------------------------------------------------------------------------
// submit
// ---------------------------------------------------------------------------

/// A test harness: the client writes requests into a channel the test reads,
/// and the test injects responses via `resolve` (exactly what `serve` does).
fn client_and_requests(ack_timeout: Duration) -> (SubmitClient, mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let client = SubmitClient::new(Writer::from_channel(tx))
        .with_timeouts(ack_timeout, Duration::from_millis(5));
    (client, rx)
}

/// Read the next request line and return `(id, parsed request)`.
async fn next_request(rx: &mut mpsc::UnboundedReceiver<String>) -> (Value, Value) {
    let request: Value = serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("request not sent")
            .expect("writer channel closed"),
    )
    .unwrap();
    (request["id"].clone(), request)
}

#[tokio::test]
async fn retryable_error_is_retried_then_final_ack_wins() {
    let (client, mut rx) = client_and_requests(Duration::from_secs(5));
    let responder = client.clone();
    let driver = tokio::spawn(async move {
        // Attempt 1: retryable SUBMIT_OVERLOADED.
        let (id, request) = next_request(&mut rx).await;
        assert_eq!(request["method"], "task/submit");
        assert_eq!(request["params"]["task"]["id"], "r1");
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": error_code::SUBMIT_OVERLOADED, "message": "busy" }
        }));
        // Attempt 2: final accepted.
        let (id, _) = next_request(&mut rx).await;
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id, "result": { "status": "accepted" }
        }));
    });

    let outcome = client.submit_task(sample_task("r1")).await;
    assert_eq!(outcome, SubmitOutcome::Accepted);
    driver.await.unwrap();
}

#[tokio::test]
async fn final_statuses_are_never_retried() {
    let (client, mut rx) = client_and_requests(Duration::from_secs(5));
    let responder = client.clone();
    tokio::spawn(async move {
        let (id, _) = next_request(&mut rx).await;
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "status": "rejected", "reason": "no workflow" }
        }));
        // A second request would hang the test's 5s timeout below.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "rejected must not be re-submitted"
        );
    });

    let outcome = client.submit_task(sample_task("f1")).await;
    assert_eq!(
        outcome,
        SubmitOutcome::Rejected {
            reason: Some("no workflow".into())
        }
    );
}

#[tokio::test]
async fn ack_timeout_retries_and_duplicate_resolves() {
    // Attempt 1 gets no answer (times out at 50ms); attempt 2 is answered
    // `duplicate` — the Orchestrator's dedup absorbing the re-submit.
    let (client, mut rx) = client_and_requests(Duration::from_millis(50));
    let responder = client.clone();
    let driver = tokio::spawn(async move {
        let (_ignored, _) = next_request(&mut rx).await; // never answered
        let (id, _) = next_request(&mut rx).await;
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id, "result": { "status": "duplicate" }
        }));
    });

    let outcome = client.submit_task(sample_task("t1")).await;
    assert_eq!(outcome, SubmitOutcome::Duplicate);
    driver.await.unwrap();
}

#[tokio::test]
async fn closed_writer_gives_up_immediately() {
    let (client, rx) = client_and_requests(Duration::from_millis(50));
    drop(rx); // host gone — permanent, no backoff
    let start = std::time::Instant::now();
    let outcome = client.submit_task(sample_task("g1")).await;
    assert!(
        matches!(outcome, SubmitOutcome::GaveUp { .. }),
        "{outcome:?}"
    );
    assert!(
        start.elapsed() < Duration::from_millis(40),
        "a permanent failure must not sit through backoff"
    );
}

#[tokio::test]
async fn non_contract_error_code_gives_up_without_retry() {
    // METHOD_NOT_FOUND is a protocol violation, not load: retrying cannot
    // help, so the client gives up after the single attempt.
    let (client, mut rx) = client_and_requests(Duration::from_secs(5));
    let responder = client.clone();
    tokio::spawn(async move {
        let (id, _) = next_request(&mut rx).await;
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": error_code::METHOD_NOT_FOUND, "message": "unknown method" }
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "a protocol violation must not be re-submitted"
        );
    });

    let outcome = client.submit_task(sample_task("m1")).await;
    assert!(
        matches!(outcome, SubmitOutcome::GaveUp { .. }),
        "{outcome:?}"
    );
}

// ---------------------------------------------------------------------------
// poll
// ---------------------------------------------------------------------------

/// Counts submissions; always accepts.
#[derive(Clone, Default)]
struct CountingSubmitter {
    submitted: Arc<Mutex<Vec<String>>>,
}

impl Submitter for CountingSubmitter {
    async fn submit(&self, task: Task) -> SubmitOutcome {
        self.submitted.lock().unwrap().push(task.id);
        SubmitOutcome::Accepted
    }
}

#[tokio::test]
async fn poll_loop_fetches_every_trigger_and_survives_fetch_errors() {
    let triggers = vec![
        TriggerInfo {
            workflow: "ok".into(),
            trigger: json!({}),
        },
        TriggerInfo {
            workflow: "broken".into(),
            trigger: json!({}),
        },
    ];
    let submitter = CountingSubmitter::default();
    let submitted = submitter.submitted.clone();
    let ticks = Arc::new(Mutex::new(0u32));
    let tick_probe = ticks.clone();

    let loop_fut = poll_loop(
        triggers,
        Duration::from_millis(5),
        submitter,
        move |trigger| {
            let ticks = tick_probe.clone();
            let workflow = trigger.workflow.clone();
            async move {
                if workflow == "broken" {
                    return Err("api down".to_string());
                }
                let mut ticks = ticks.lock().unwrap();
                *ticks += 1;
                Ok(vec![sample_task(&format!("p{ticks}"))])
            }
        },
    );
    // The loop never returns; run it for a bounded slice of time.
    let _ = tokio::time::timeout(Duration::from_millis(100), loop_fut).await;

    let submitted = submitted.lock().unwrap();
    assert!(
        submitted.len() >= 2,
        "expected multiple non-overlapping ticks, got {submitted:?}"
    );
    // The broken trigger never produced a submission and never killed the
    // healthy one.
    assert!(submitted.iter().all(|id| id.starts_with('p')));
}

// ---------------------------------------------------------------------------
// lookup (0.2.4, #242)
// ---------------------------------------------------------------------------

fn lookup_client(timeout: Duration) -> (LookupClient, mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let client = LookupClient::new(Writer::from_channel(tx)).with_timeout(timeout);
    (client, rx)
}

#[tokio::test]
async fn lookup_reports_a_known_conversation_and_its_repository() {
    let (client, mut rx) = lookup_client(Duration::from_secs(5));
    let responder = client.clone();
    let driver = tokio::spawn(async move {
        let (id, request) = next_request(&mut rx).await;
        assert_eq!(request["method"], "task/lookup");
        assert_eq!(request["params"]["source"], "slack");
        assert_eq!(request["params"]["task_id"], "C1:100");
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id,
            "result": { "known": true, "repo": "totsuka" }
        }));
    });

    let answer = client.lookup("slack", "C1:100").await;
    assert_eq!(
        answer,
        Lookup::Known {
            repo: Some("totsuka".into())
        }
    );
    assert!(answer.skips_resolution(), "a reply needs no repo hint");
    driver.await.unwrap();
}

#[tokio::test]
async fn lookup_reports_an_unknown_conversation_as_new() {
    let (client, mut rx) = lookup_client(Duration::from_secs(5));
    let responder = client.clone();
    let driver = tokio::spawn(async move {
        let (id, _) = next_request(&mut rx).await;
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id, "result": { "known": false }
        }));
    });

    let answer = client.lookup("slack", "C9:999").await;
    assert_eq!(answer, Lookup::New);
    assert!(
        !answer.skips_resolution(),
        "a new conversation still needs resolving"
    );
    driver.await.unwrap();
}

/// The degradation contract: an unanswerable lookup must resolve to a value
/// the caller can act on, in bounded time, and must never look "known".
#[tokio::test]
async fn an_unanswered_lookup_degrades_instead_of_hanging() {
    // Nobody answers: the timeout fires and the caller falls back.
    let (client, _rx) = lookup_client(Duration::from_millis(30));
    let answer = client.lookup("slack", "C1:100").await;
    assert!(matches!(answer, Lookup::Unknown { .. }), "{answer:?}");
    assert!(
        !answer.skips_resolution(),
        "a timeout must never be read as `known` — the conversation would \
         dispatch with no repository at all"
    );

    // An error answer degrades the same way, without retrying: a second
    // request would mean waiting again for the same fallback.
    let (client, mut rx) = lookup_client(Duration::from_secs(5));
    let responder = client.clone();
    let driver = tokio::spawn(async move {
        let (id, _) = next_request(&mut rx).await;
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": error_code::INTERNAL_ERROR, "message": "db locked" }
        }));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "a lookup is never retried"
        );
    });
    let answer = client.lookup("slack", "C1:100").await;
    assert!(matches!(answer, Lookup::Unknown { .. }), "{answer:?}");
    driver.await.unwrap();
}

/// The two clients share the response stream, so each must ignore the other's
/// ids rather than swallowing them.
#[tokio::test]
async fn lookup_and_submit_clients_do_not_steal_each_other_s_answers() {
    let (lookup, mut lrx) = lookup_client(Duration::from_secs(5));
    let (submit, _srx) = client_and_requests(Duration::from_secs(5));
    let responder = lookup.clone();
    let stealer = submit.clone();
    let driver = tokio::spawn(async move {
        let (id, _) = next_request(&mut lrx).await;
        // `serve` hands every response to both clients; the submit client
        // must leave this one alone.
        stealer.resolve(&json!({
            "jsonrpc": "2.0", "id": id.clone(), "result": { "known": true }
        }));
        responder.resolve(&json!({
            "jsonrpc": "2.0", "id": id, "result": { "known": true, "repo": "totsuka" }
        }));
    });

    assert_eq!(
        lookup.lookup("slack", "C1:100").await,
        Lookup::Known {
            repo: Some("totsuka".into())
        }
    );
    driver.await.unwrap();
}
