//! SDK behavior tests: typed dispatch, submit retry semantics, poll loop.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use plugin_protocol::Task;
use plugin_protocol::jsonrpc::{Error, error_code};
use plugin_protocol::methods::{
    ConfigValidateParams, ConfigValidateResult, InitializeParams, InitializeResult,
    ResultPublishParams, TaskUpdateStatusParams, WorkflowInfo,
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
        handle: None,
        branch_hint: None,
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
            claimed_repos: Vec::new(),
            claimed_options: Vec::new(),
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
            warnings: vec![],
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

    let outcome = client.submit_task(sample_task("r1"), "wf").await;
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

    let outcome = client.submit_task(sample_task("f1"), "wf").await;
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

    let outcome = client.submit_task(sample_task("t1"), "wf").await;
    assert_eq!(outcome, SubmitOutcome::Duplicate);
    driver.await.unwrap();
}

#[tokio::test]
async fn closed_writer_gives_up_immediately() {
    let (client, rx) = client_and_requests(Duration::from_millis(50));
    drop(rx); // host gone — permanent, no backoff
    let start = std::time::Instant::now();
    let outcome = client.submit_task(sample_task("g1"), "wf").await;
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

    let outcome = client.submit_task(sample_task("m1"), "wf").await;
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
    async fn submit(&self, task: Task, _workflow: &str) -> SubmitOutcome {
        self.submitted.lock().unwrap().push(task.id);
        SubmitOutcome::Accepted
    }
}

#[tokio::test]
async fn poll_loop_fetches_every_trigger_and_survives_fetch_errors() {
    let triggers = vec![
        WorkflowInfo {
            workflow: "ok".into(),
            projects: vec![],
            status_writebacks: vec![],
            trigger: json!({}),
            instructions_kind: None,
            task_id_prefix: None,
            options: Default::default(),
        },
        WorkflowInfo {
            workflow: "broken".into(),
            projects: vec![],
            status_writebacks: vec![],
            trigger: json!({}),
            instructions_kind: None,
            task_id_prefix: None,
            options: Default::default(),
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

// ---------------------------------------------------------------------------
// agent_ide dispatch
// ---------------------------------------------------------------------------

mod agent_ide {
    use super::*;
    use plugin_protocol::methods::{
        SessionAttachParams, SessionAttachResult, StateNotification, StateSubscribeParams,
        TaskCancelParams, TaskDispatchParams, TaskDispatchResult,
    };
    use plugin_sdk::{AgentIdeHandler, AgentIdeServer};

    /// Overrides only the required methods; `state/subscribe` hands back a
    /// stream that already holds two notifications, so the forwarder could
    /// write them the instant it starts.
    pub(super) struct Agent;

    impl AgentIdeHandler for Agent {
        async fn initialize(&mut self, _: InitializeParams) -> Result<InitializeResult, Error> {
            unreachable!("not exercised")
        }
        async fn config_validate(
            &mut self,
            _: ConfigValidateParams,
        ) -> Result<ConfigValidateResult, Error> {
            unreachable!("not exercised")
        }
        async fn task_dispatch(
            &mut self,
            _: TaskDispatchParams,
        ) -> Result<TaskDispatchResult, Error> {
            unreachable!("not exercised")
        }
        async fn session_attach(
            &mut self,
            _: SessionAttachParams,
        ) -> Result<SessionAttachResult, Error> {
            unreachable!("not exercised")
        }
        async fn task_cancel(&mut self, _: TaskCancelParams) -> Result<(), Error> {
            Ok(())
        }
        async fn state_subscribe(
            &mut self,
            params: StateSubscribeParams,
        ) -> Result<mpsc::UnboundedReceiver<StateNotification>, Error> {
            let (tx, rx) = mpsc::unbounded_channel();
            for state in ["running", "idle"] {
                let note: StateNotification = serde_json::from_value(json!({
                    "session_id": params.session_id, "state": state
                }))
                .unwrap();
                tx.send(note).unwrap();
            }
            Ok(rx)
        }
    }

    fn server() -> (AgentIdeServer<Agent>, mpsc::UnboundedReceiver<String>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (AgentIdeServer::new(Agent, Writer::from_channel(tx)), rx)
    }

    /// F-38: the host must read the ACK before any notification. The ACK is
    /// written through the shared writer by the server itself — returned as
    /// the `Reply` instead, `serve` would write it only after `handle_line`
    /// returned, behind notifications the forwarder had already sent.
    #[tokio::test]
    async fn state_subscribe_acks_before_the_first_notification() {
        let (mut server, mut out) = server();
        let reply = server
            .handle_line(&line(json!({
                "jsonrpc": "2.0", "id": 7, "method": "state/subscribe",
                "params": { "session_id": "s1" }
            })))
            .await;
        assert!(reply.line.is_none(), "the ACK must not wait for `serve`");

        let mut next = async || -> Value {
            let line = tokio::time::timeout(Duration::from_secs(5), out.recv())
                .await
                .expect("no output within 5s")
                .expect("writer closed");
            serde_json::from_str(&line).unwrap()
        };
        let ack = next().await;
        assert_eq!(ack["id"], 7, "first line is the ACK: {ack}");
        assert_eq!(ack["result"], Value::Null);
        for state in ["running", "idle"] {
            let note = next().await;
            assert_eq!(note["method"], "state/notification", "{note}");
            assert_eq!(note["params"]["state"], state);
        }
    }

    /// The capability-gated methods refuse by default, naming the capability
    /// not to declare — in a message free of source indentation (the guard
    /// `task_claim`'s default carries, for the same reason).
    #[tokio::test]
    async fn capability_gated_methods_refuse_by_default() {
        let (mut server, _out) = server();
        for (method, params, capability) in [
            (
                "session/focus",
                json!({ "session_id": "s" }),
                "pane_control",
            ),
            ("session/list", json!({}), "pane_control"),
            (
                "session/release",
                json!({ "session_id": "s", "expect_cwd": "/wt" }),
                "pane_control",
            ),
            (
                "diagnostics/snapshot",
                json!({ "session_id": "s" }),
                "diagnostics_snapshot",
            ),
        ] {
            let reply = server
                .handle_line(&line(json!({
                    "jsonrpc": "2.0", "id": 1, "method": method, "params": params
                })))
                .await;
            let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
            assert_eq!(response["error"]["code"], error_code::METHOD_NOT_FOUND);
            let message = response["error"]["message"].as_str().unwrap();
            assert!(message.contains(capability), "{method}: {message}");
            assert!(!message.contains("  "), "{method}: {message:?}");
        }
    }

    /// The rest of the line protocol is the task_source one: a unit result is
    /// `null`, junk is PARSE_ERROR, notifications are silent, `shutdown` stops.
    #[tokio::test]
    async fn the_line_protocol_matches_the_task_source_side() {
        let (mut server, _out) = server();
        let reply = server
            .handle_line(&line(json!({
                "jsonrpc": "2.0", "id": 1, "method": "task/cancel",
                "params": { "session_id": "s" }
            })))
            .await;
        let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
        assert_eq!(response["result"], Value::Null, "{response}");

        let reply = server
            .handle_line(&line(json!({
                "jsonrpc": "2.0", "id": 2, "method": "task/cancel", "params": {}
            })))
            .await;
        let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
        assert_eq!(response["error"]["code"], error_code::INVALID_PARAMS);

        let reply = server.handle_line("not json").await;
        let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
        assert_eq!(response["error"]["code"], error_code::PARSE_ERROR);
        assert!(
            server
                .handle_line(&line(json!({"jsonrpc": "2.0", "method": "notify"})))
                .await
                .line
                .is_none()
        );
        let reply = server
            .handle_line(&line(json!({"jsonrpc": "2.0", "id": 3, "method": "nope"})))
            .await;
        let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
        assert_eq!(response["error"]["code"], error_code::METHOD_NOT_FOUND);
        let reply = server
            .handle_line(&line(
                json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown"}),
            ))
            .await;
        assert!(reply.shutdown);
    }
}

// ---------------------------------------------------------------------------
// the not-initialized gate
// ---------------------------------------------------------------------------

/// A handler that has not been initialized and says so.
struct Uninitialized;

impl TaskSourceHandler for Uninitialized {
    fn initialized(&self) -> bool {
        false
    }
    async fn initialize(&mut self, _: InitializeParams) -> Result<InitializeResult, Error> {
        unreachable!("not exercised")
    }
    async fn config_validate(
        &mut self,
        _: ConfigValidateParams,
    ) -> Result<ConfigValidateResult, Error> {
        unreachable!("not exercised")
    }
    async fn update_status(&mut self, _: TaskUpdateStatusParams) -> Result<Value, Error> {
        unreachable!("not exercised")
    }
    async fn result_publish(&mut self, _: ResultPublishParams) -> Result<Value, Error> {
        unreachable!("not exercised")
    }
}

/// Before `initialize`, a request whose params do not parse is told to
/// initialize first — the code the hand-written servers answered, since they
/// checked the session before reading params (#759). `initialize` and
/// `config/validate` still report their params, and an unknown method is
/// still unknown.
#[tokio::test]
async fn malformed_params_before_initialize_say_initialize_first() {
    let mut server = TaskSourceServer(Uninitialized);
    let mut code = async |method: &str| {
        let reply = server
            .handle_line(&line(json!({
                "jsonrpc": "2.0", "id": 1, "method": method, "params": { "wrong": true }
            })))
            .await;
        let response: Value = serde_json::from_str(&reply.line.unwrap()).unwrap();
        response["error"]["code"].clone()
    };
    assert_eq!(
        code("task/update_status").await,
        error_code::INVALID_REQUEST
    );
    assert_eq!(code("result/publish").await, error_code::INVALID_REQUEST);
    assert_eq!(code("initialize").await, error_code::INVALID_PARAMS);
    assert_eq!(code("config/validate").await, error_code::INVALID_PARAMS);
    assert_eq!(code("nope").await, error_code::METHOD_NOT_FOUND);
}

/// A `state/subscribe` whose ACK cannot be written — the writer, and so the
/// host, is gone — stops the server instead of starting a stream nobody reads.
#[tokio::test]
async fn state_subscribe_with_the_writer_gone_stops_serving() {
    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let mut server = plugin_sdk::AgentIdeServer::new(agent_ide::Agent, Writer::from_channel(tx));
    let reply = server
        .handle_line(&line(json!({
            "jsonrpc": "2.0", "id": 7, "method": "state/subscribe",
            "params": { "session_id": "s1" }
        })))
        .await;
    assert!(reply.shutdown);
    assert!(reply.line.is_none());
}
