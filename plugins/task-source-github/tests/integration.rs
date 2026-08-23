//! End-to-end plugin flow over a recorded GraphQL transport (no network):
//! initialize → poll_loop → `task/submit` push (0.1.6), normalize →
//! task/update_status, plus ingest gating (F-08) and
//! invalid-token config/validate (F-59). `tasks/fetch` no longer exists as
//! of protocol 0.2.0 (#190).

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use plugin_protocol::jsonrpc::Response;
use task_source_github::error::GithubError;
use task_source_github::server::{Server, TransportFactory};
use task_source_github::transport::GithubTransport;

/// A canned GraphQL outcome for one `post_graphql` call.
#[derive(Clone)]
enum Canned {
    /// A full GraphQL response body (`{ "data": ... }`).
    Data(Value),
    /// Simulate an HTTP 401 (bad token).
    Unauthorized,
}

/// State shared between the factory, the transports it builds, and the test:
/// a queue of responses to hand out and a log of the requests received.
#[derive(Clone, Default)]
struct Shared {
    responses: Arc<Mutex<VecDeque<Canned>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Shared {
    fn push(&self, canned: Canned) {
        self.responses.lock().unwrap().push_back(canned);
    }
    fn last_request(&self) -> Value {
        self.requests.lock().unwrap().last().cloned().unwrap()
    }
    fn all_requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

struct FakeTransport {
    shared: Shared,
}

impl GithubTransport for FakeTransport {
    fn post_graphql(
        &self,
        body: Value,
        _idempotent: bool,
    ) -> impl Future<Output = Result<Value, GithubError>> + Send {
        self.shared.requests.lock().unwrap().push(body);
        let next = self.shared.responses.lock().unwrap().pop_front();
        async move {
            match next {
                Some(Canned::Data(v)) => Ok(v),
                Some(Canned::Unauthorized) => Err(GithubError::Unauthorized),
                None => Err(GithubError::InvalidResponse("no canned response".into())),
            }
        }
    }
}

struct FakeFactory {
    shared: Shared,
}

impl TransportFactory for FakeFactory {
    type Transport = FakeTransport;
    fn build(&self, _endpoint: &str, _token: &str, _max_retries: u32) -> FakeTransport {
        FakeTransport {
            shared: self.shared.clone(),
        }
    }
}

/// Receives the server's outbound `task/submit` requests and acks them —
/// the push analogue of calling `tasks/fetch` (same shape as the slack
/// plugin's SubmitHarness).
struct SubmitHarness {
    client: plugin_sdk::SubmitClient,
    rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

impl SubmitHarness {
    fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Short timeouts: a test that never acks must not stall for minutes.
        let client = plugin_sdk::SubmitClient::new(plugin_sdk::Writer::from_channel(tx))
            .with_timeouts(Duration::from_secs(5), Duration::from_millis(10));
        Self { client, rx }
    }

    /// Await the next `task/submit`, ack it `accepted`, return its task.
    async fn next_task(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.rx.recv())
            .await
            .expect("no task/submit within 5s")
            .expect("submit channel closed");
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "task/submit", "{request}");
        self.client.resolve(&json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": { "status": "accepted" }
        }));
        request["params"]["task"].clone()
    }

    /// Assert nothing is submitted within `window`.
    async fn assert_no_task(&mut self, window: Duration) {
        match tokio::time::timeout(window, self.rx.recv()).await {
            Err(_) => {}
            Ok(line) => panic!("unexpected task/submit: {line:?}"),
        }
    }
}

fn server(shared: &Shared) -> Server<FakeFactory> {
    Server::new(
        FakeFactory {
            shared: shared.clone(),
        },
        SubmitHarness::new().client,
    )
}

fn server_with_harness(shared: &Shared) -> (Server<FakeFactory>, SubmitHarness) {
    let harness = SubmitHarness::new();
    let srv = Server::new(
        FakeFactory {
            shared: shared.clone(),
        },
        harness.client.clone(),
    );
    (srv, harness)
}

/// Send one JSON-RPC request line and return the parsed response.
async fn call(srv: &mut Server<FakeFactory>, id: i64, method: &str, params: Value) -> Response {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    serde_json::from_str(&reply.line.expect("a response line")).expect("valid response")
}

fn init_config() -> Value {
    json!({
        "token": "ghp_test", "github_login": "me",
        "projects": [{ "owner": "me", "project_number": 1, "repos": ["totsuka"] }],
        "in_progress_statuses": ["実装中"],
        "status_map": { "レビュー待ち": "In Review" }
    })
}

/// Two boards, each tracking a different repository (#542).
fn two_board_config() -> Value {
    json!({
        "token": "ghp_test", "github_login": "me",
        "projects": [
            { "owner": "me", "project_number": 1, "repos": ["totsuka"] },
            { "owner": "acme", "owner_type": "organization",
              "project_number": 3, "repos": ["web-app"] }
        ],
        "in_progress_statuses": ["実装中"],
        "status_map": { "レビュー待ち": "In Review" }
    })
}

/// `initialize` params wrap the plugin config alongside the protocol version.
fn init_params() -> Value {
    json!({ "protocol_version": "0.1.0", "config": init_config() })
}

/// A project-items page with four items exercising every gating branch.
fn fetch_response() -> Value {
    json!({ "data": { "user": { "projectV2": { "items": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [
            { "status": { "name": "実装待ち" }, "content": {
                "__typename": "Issue", "id": "I_1", "number": 1, "title": "Task one",
                "body": "please do it", "url": "https://github.com/me/totsuka/issues/1",
                "repository": { "name": "totsuka" },
                "assignees": { "nodes": [] },
                "labels": { "nodes": [{ "name": "bug" }] } } },
            { "status": { "name": "実装待ち" }, "content": {
                "__typename": "Issue", "id": "I_2", "number": 2, "title": "Someone else's",
                "url": "https://github.com/me/totsuka/issues/2",
                "repository": { "name": "totsuka" },
                "assignees": { "nodes": [{ "login": "another-dev" }] },
                "labels": { "nodes": [] } } },
            { "status": { "name": "実装中" }, "content": {
                "__typename": "Issue", "id": "I_3", "number": 3, "title": "In progress",
                "repository": { "name": "totsuka" },
                "assignees": { "nodes": [] }, "labels": { "nodes": [] } } },
            { "status": { "name": "実装待ち" }, "content": { "__typename": "PullRequest" } }
        ]
    } } } } })
}

#[tokio::test]
async fn initialize_then_update_status() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // initialize → declares no outputs: the agent writes the deliverable
    // itself. `task_submit` is gone in 0.5.0 too — every task_source has been
    // push-only since `tasks/fetch` was removed at 0.2.0, so the flag could
    // only ever be `true`.
    let resp = call(&mut srv, 1, "initialize", init_params()).await;
    let result = resp.result.expect("initialize result");
    assert_eq!(result["capabilities"]["outputs"], json!([]));

    // task/update_status → maps レビュー待ち → "In Review", resolves ids, mutates.
    shared.push(Canned::Data(json!({ "data": { "user": { "projectV2": {
        "id": "PROJ_1",
        "field": { "id": "FIELD_1", "options": [
            { "id": "OPT_review", "name": "In Review" },
            { "id": "OPT_todo", "name": "実装待ち" } ] },
        "items": { "nodes": [
            { "id": "ITEM_1", "content": { "id": "I_1" } },
            { "id": "ITEM_9", "content": { "id": "I_9" } } ] }
    } } } })));
    shared.push(Canned::Data(
        json!({ "data": { "updateProjectV2ItemFieldValue": {
        "projectV2Item": { "id": "ITEM_1" } } } }),
    ));
    let resp = call(
        &mut srv,
        3,
        "task/update_status",
        json!({ "task_id": "I_1", "status": "レビュー待ち" }),
    )
    .await;
    assert!(resp.error.is_none(), "update failed: {:?}", resp.error);
    let vars = &shared.last_request()["variables"];
    assert_eq!(vars["project"], "PROJ_1");
    assert_eq!(vars["item"], "ITEM_1");
    assert_eq!(vars["field"], "FIELD_1");
    assert_eq!(vars["option"], "OPT_review");

    // `result/publish` is gone: the agent writes the deliverable itself.
    let resp = call(
        &mut srv,
        4,
        "result/publish",
        json!({ "task_id": "I_1", "content": "x", "format": "markdown" }),
    )
    .await;
    let err = resp.error.expect("a removed method must be refused");
    assert_eq!(err.code, plugin_protocol::error_code::METHOD_NOT_FOUND);
    // The message has to name the fix: this is reached only after the agent
    // has done all the work, so "unknown method" would leave the operator
    // guessing at the end of a wasted run.
    assert!(err.message.contains("output"), "{}", err.message);
}

#[tokio::test]
async fn update_status_rejects_unknown_option() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;

    // The project has no option matching the (mapped) target status.
    shared.push(Canned::Data(json!({ "data": { "user": { "projectV2": {
        "id": "PROJ_1",
        "field": { "id": "FIELD_1", "options": [{ "id": "OPT_todo", "name": "実装待ち" }] },
        "items": { "nodes": [{ "id": "ITEM_1", "content": { "id": "I_1" } }] }
    } } } })));
    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "I_1", "status": "Nonexistent" }),
    )
    .await;
    let err = resp.error.expect("unknown status must error");
    assert!(
        err.message.contains("unknown status"),
        "got {}",
        err.message
    );
}

#[tokio::test]
async fn config_validate_reports_invalid_token() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // The viewer ping is rejected → config/validate returns invalid + guidance.
    shared.push(Canned::Unauthorized);
    let resp = call(
        &mut srv,
        1,
        "config/validate",
        json!({ "config": init_config() }),
    )
    .await;
    let result = resp
        .result
        .expect("config/validate always succeeds at the RPC level");
    assert_eq!(result["valid"], false);
    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors.iter().any(|e| e.as_str().unwrap().contains("401")),
        "expected a 401/next-action message, got {errors:?}"
    );
}

#[tokio::test]
async fn config_validate_flags_static_problem_without_network() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // project_number = 0 is caught statically; no transport call is made.
    let mut bad = init_config();
    bad["projects"][0]["project_number"] = json!(0);
    let resp = call(&mut srv, 1, "config/validate", json!({ "config": bad })).await;
    let result = resp.result.unwrap();
    assert_eq!(result["valid"], false);
    assert!(
        shared.requests.lock().unwrap().is_empty(),
        "no network on static failure"
    );
}

#[tokio::test]
async fn ingests_task_assigned_to_me_among_multiple_assignees() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    // I ("me") am the *second* assignee — ingest must not depend on ordering.
    shared.push(Canned::Data(
        json!({ "data": { "user": { "projectV2": { "items": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [ { "status": { "name": "実装待ち" }, "content": {
            "__typename": "Issue", "id": "I_7", "number": 7, "title": "Shared",
            "repository": { "name": "totsuka" },
            "assignees": { "nodes": [{ "login": "reviewer" }, { "login": "me" }] },
            "labels": { "nodes": [] } } } ]
    } } } } }),
    ));
    let params = json!({
        "protocol_version": "0.1.6",
        "config": init_config(),
        "triggers": [
            { "workflow": "design", "trigger": { "project_status": "実装待ち" } }
        ],
        "poll_interval_secs": 60
    });
    call(&mut srv, 1, "initialize", params).await;

    let task = harness.next_task().await;
    assert_eq!(task["id"], "I_7", "a task I co-own must be ingested");
    assert_eq!(task["assignee"], "me");
    harness.assert_no_task(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn update_status_finds_item_on_a_later_page() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;

    // Page 1: field + options present, but the target item is not here.
    shared.push(Canned::Data(json!({ "data": { "user": { "projectV2": {
        "id": "PROJ_1",
        "field": { "id": "FIELD_1", "options": [{ "id": "OPT_review", "name": "In Review" }] },
        "items": { "pageInfo": { "hasNextPage": true, "endCursor": "C1" },
            "nodes": [{ "id": "ITEM_A", "content": { "id": "I_other" } }] }
    } } } })));
    // Page 2: the item appears.
    shared.push(Canned::Data(json!({ "data": { "user": { "projectV2": {
        "id": "PROJ_1",
        "field": { "id": "FIELD_1", "options": [{ "id": "OPT_review", "name": "In Review" }] },
        "items": { "pageInfo": { "hasNextPage": false, "endCursor": null },
            "nodes": [{ "id": "ITEM_TARGET", "content": { "id": "I_1" } }] }
    } } } })));
    shared.push(Canned::Data(
        json!({ "data": { "updateProjectV2ItemFieldValue": {
        "projectV2Item": { "id": "ITEM_TARGET" } } } }),
    ));

    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "I_1", "status": "レビュー待ち" }),
    )
    .await;
    assert!(
        resp.error.is_none(),
        "paged item lookup failed: {:?}",
        resp.error
    );
    let vars = &shared.last_request()["variables"];
    assert_eq!(vars["item"], "ITEM_TARGET");
    assert_eq!(vars["option"], "OPT_review");
}

/// The resident poll loop (0.1.6): `initialize` with triggers fetches each
/// trigger immediately and pushes the surviving tasks via `task/submit`.
#[tokio::test]
async fn initialize_with_triggers_polls_and_submits() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    // The first tick runs before any sleep, consuming this canned page.
    shared.push(Canned::Data(fetch_response()));
    let params = json!({
        "protocol_version": "0.1.6",
        "config": init_config(),
        "triggers": [
            { "workflow": "design", "trigger": { "project_status": "実装待ち" } }
        ],
        "poll_interval_secs": 60
    });
    let resp = call(&mut srv, 1, "initialize", params).await;
    assert!(resp.error.is_none(), "initialize failed: {:?}", resp.error);

    // Ingest gating (F-08): only the ingestable issue survives (assignee-other,
    // in-progress, and the PR are excluded).
    let task = harness.next_task().await;
    assert_eq!(task["id"], "I_1");
    assert_eq!(task["source"], "github");
    assert_eq!(task["title"], "Task one");
    assert_eq!(task["body"], "please do it");
    assert_eq!(task["repo_hint"], "totsuka"); // enables F-10
    assert_eq!(task["status"], "実装待ち");
    assert_eq!(task["labels"], json!(["bug"]));
    assert!(task.get("assignee").is_none() || task["assignee"].is_null());
    harness.assert_no_task(Duration::from_millis(200)).await;
}

/// One page for a board that tracks `web-app`, holding one ingestable issue.
fn web_app_page() -> Value {
    json!({ "data": { "organization": { "projectV2": { "items": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [
            { "status": { "name": "実装待ち" }, "content": {
                "__typename": "Issue", "id": "I_20", "number": 20, "title": "Web task",
                "url": "https://github.com/acme/web-app/issues/20",
                "repository": { "name": "web-app" },
                "assignees": { "nodes": [] }, "labels": { "nodes": [] } } },
            // Same board, a repository it does not track: the per-board `repos`
            // filter must drop this even though the board holds it.
            { "status": { "name": "実装待ち" }, "content": {
                "__typename": "Issue", "id": "I_21", "number": 21, "title": "Not ours",
                "url": "https://github.com/acme/other/issues/21",
                "repository": { "name": "other" },
                "assignees": { "nodes": [] }, "labels": { "nodes": [] } } }
        ]
    } } } } })
}

/// A poll visits **every** configured board, and each board's `repos` gates
/// its own items (#542).
#[tokio::test]
async fn a_poll_walks_every_board_and_each_board_gates_its_own_repos() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    // Board order is config order, so the queue order is fixed.
    shared.push(Canned::Data(fetch_response()));
    shared.push(Canned::Data(web_app_page()));
    let params = json!({
        "protocol_version": "0.5.1",
        "config": two_board_config(),
        "triggers": [
            { "workflow": "design", "trigger": { "project_status": "実装待ち" } }
        ],
        "poll_interval_secs": 60
    });
    let resp = call(&mut srv, 1, "initialize", params).await;
    assert!(resp.error.is_none(), "initialize failed: {:?}", resp.error);

    let first = harness.next_task().await;
    assert_eq!(first["id"], "I_1");
    assert_eq!(first["repo_hint"], "totsuka");
    let second = harness.next_task().await;
    assert_eq!(second["id"], "I_20");
    assert_eq!(second["repo_hint"], "web-app");
    // `I_21` lives on the second board but in an untracked repository.
    harness.assert_no_task(Duration::from_millis(200)).await;

    // Each board was queried with its own owner/number/root — a single shared
    // owner would silently poll the same board twice.
    let requests = shared.all_requests();
    assert_eq!(requests.len(), 2, "one request per board");
    assert_eq!(requests[0]["variables"]["owner"], "me");
    assert_eq!(requests[0]["variables"]["number"], 1);
    assert!(
        requests[0]["query"]
            .as_str()
            .unwrap()
            .contains("user(login:")
    );
    assert_eq!(requests[1]["variables"]["owner"], "acme");
    assert_eq!(requests[1]["variables"]["number"], 3);
    assert!(
        requests[1]["query"]
            .as_str()
            .unwrap()
            .contains("organization(login:")
    );
}

/// `initialize` answers with the repository → board mapping (protocol 0.5.1),
/// which is the only way the Orchestrator can learn it.
#[tokio::test]
async fn initialize_publishes_the_repository_to_board_mapping() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    let params = json!({ "protocol_version": "0.5.1", "config": two_board_config() });
    let resp = call(&mut srv, 1, "initialize", params).await;
    let claims = resp.result.expect("initialize result")["claimed_repos"].clone();
    assert_eq!(claims.as_array().map(Vec::len), Some(2));
    assert_eq!(claims[0]["repo"], "totsuka");
    assert!(
        claims[0]["destination"].as_str().unwrap().contains("#1"),
        "{claims}"
    );
    assert_eq!(claims[1]["repo"], "web-app");
    assert!(
        claims[1]["destination"].as_str().unwrap().contains("#3"),
        "{claims}"
    );
}

/// With several boards, `task/update_status` has nothing in the request naming
/// the board, so it scans — and the scan must not stop at the first board.
#[tokio::test]
async fn update_status_scans_past_a_board_that_does_not_hold_the_item() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(
        &mut srv,
        1,
        "initialize",
        json!({ "protocol_version": "0.5.1", "config": two_board_config() }),
    )
    .await;

    // Board #1 resolves fine but holds a different item.
    shared.push(Canned::Data(json!({ "data": { "user": { "projectV2": {
        "id": "PROJ_1",
        "field": { "id": "FIELD_1", "options": [{ "id": "OPT_review", "name": "In Review" }] },
        "items": { "nodes": [{ "id": "ITEM_9", "content": { "id": "I_9" } }] }
    } } } })));
    // Board #3 holds it.
    shared.push(Canned::Data(
        json!({ "data": { "organization": { "projectV2": {
        "id": "PROJ_3",
        "field": { "id": "FIELD_3", "options": [{ "id": "OPT_review3", "name": "In Review" }] },
        "items": { "nodes": [{ "id": "ITEM_20", "content": { "id": "I_20" } }] }
    } } } }),
    ));
    shared.push(Canned::Data(
        json!({ "data": { "updateProjectV2ItemFieldValue": {
        "projectV2Item": { "id": "ITEM_20" } } } }),
    ));

    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "I_20", "status": "レビュー待ち" }),
    )
    .await;
    assert!(resp.error.is_none(), "update failed: {:?}", resp.error);
    let vars = &shared.last_request()["variables"];
    assert_eq!(vars["project"], "PROJ_3");
    assert_eq!(vars["item"], "ITEM_20");
    assert_eq!(vars["option"], "OPT_review3");
}

/// An item on none of the boards is an error naming every board tried — not a
/// silent no-op, and not a message naming only the first board.
#[tokio::test]
async fn update_status_for_an_item_on_no_board_names_every_board() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(
        &mut srv,
        1,
        "initialize",
        json!({ "protocol_version": "0.5.1", "config": two_board_config() }),
    )
    .await;

    for (root, project_id, option) in [
        ("user", "PROJ_1", "OPT_1"),
        ("organization", "PROJ_3", "OPT_3"),
    ] {
        shared.push(Canned::Data(json!({ "data": { root: { "projectV2": {
            "id": project_id,
            "field": { "id": "F", "options": [{ "id": option, "name": "In Review" }] },
            "items": { "nodes": [] }
        } } } })));
    }

    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "I_404", "status": "レビュー待ち" }),
    )
    .await;
    let err = resp.error.expect("an item on no board must fail");
    assert!(
        err.message.contains("#1") && err.message.contains("#3"),
        "{}",
        err.message
    );
}

/// Without triggers there is nothing to watch: no poll loop, no submissions,
/// and no canned responses consumed by a background task.
#[tokio::test]
async fn initialize_without_triggers_never_submits() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;
    harness.assert_no_task(Duration::from_millis(200)).await;
    assert!(
        shared.requests.lock().unwrap().is_empty(),
        "no fetch without triggers"
    );
}

#[test]
fn shipped_manifest_is_valid_and_declares_push_source() {
    // The on-disk plugin.toml must parse and declare kind=task_source with
    // `task_submit` (push ingestion, 0.1.6) and the `source` output
    // capability (F-83) so the orchestrator accepts it and never polls it.
    let manifest = plugin_protocol::Manifest::from_toml_str(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(manifest.name, "github");
    assert_eq!(manifest.kind, plugin_protocol::PluginKind::TaskSource);
    // Nothing is published by this plugin any more: the agent writes the
    // deliverable itself, so declaring `source` would advertise an RPC that
    // no longer exists.
    assert!(manifest.capabilities.outputs.is_empty());
    assert!(
        manifest.is_compatible_with(&plugin_protocol::protocol_version()),
        "manifest must accept the current protocol version"
    );
}

#[tokio::test]
async fn methods_before_initialize_are_rejected() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    let resp = call(
        &mut srv,
        1,
        "task/update_status",
        json!({ "task_id": "I_1", "status": "実装待ち" }),
    )
    .await;
    assert!(
        resp.error
            .expect("must error")
            .message
            .contains("initialize")
    );
}
