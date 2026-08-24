//! End-to-end plugin flow over a recorded REST transport (no network):
//! initialize → poll_loop → `task/submit` push (0.1.6), normalize via the
//! property map → page-body fetch → task/update_status
//! (with block splitting), plus ingest gating (F-08), unknown-status
//! rejection (F-84), and config/validate against the database schema
//! (F-59). `tasks/fetch` no longer exists as of protocol 0.2.0 (#190).

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use plugin_protocol::jsonrpc::Response;
use task_source_notion::error::NotionError;
use task_source_notion::server::{Server, TransportFactory};
use task_source_notion::transport::{HttpMethod, NotionTransport, TransportSettings};

/// A canned REST outcome for one `request` call.
#[derive(Clone)]
enum Canned {
    /// A full response body.
    Data(Value),
    /// Simulate an HTTP 401 (bad token).
    Unauthorized,
}

/// One recorded request: its method, path, and body.
#[derive(Clone, Debug)]
struct Recorded {
    method: HttpMethod,
    path: String,
    body: Option<Value>,
}

/// State shared between the factory, the transports it builds, and the test.
#[derive(Clone, Default)]
struct Shared {
    responses: Arc<Mutex<VecDeque<Canned>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Shared {
    fn push(&self, canned: Canned) {
        self.responses.lock().unwrap().push_back(canned);
    }
    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
    fn last_request(&self) -> Recorded {
        self.requests.lock().unwrap().last().cloned().unwrap()
    }
}

struct FakeTransport {
    shared: Shared,
}

impl NotionTransport for FakeTransport {
    fn request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<Value>,
        _idempotent: bool,
    ) -> impl Future<Output = Result<Value, NotionError>> + Send {
        self.shared.requests.lock().unwrap().push(Recorded {
            method,
            path: path.to_string(),
            body,
        });
        let next = self.shared.responses.lock().unwrap().pop_front();
        async move {
            match next {
                Some(Canned::Data(v)) => Ok(v),
                Some(Canned::Unauthorized) => Err(NotionError::Unauthorized),
                None => Err(NotionError::InvalidResponse("no canned response".into())),
            }
        }
    }
}

struct FakeFactory {
    shared: Shared,
}

impl TransportFactory for FakeFactory {
    type Transport = FakeTransport;
    fn build(&self, _settings: TransportSettings<'_>) -> FakeTransport {
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

/// A config exercising the full property map, with the body coming from page
/// blocks so the extra block fetch is covered.
fn init_config() -> Value {
    json!({
        "token": "secret_test",
        "databases": [{ "database_id": "DB1", "repos": ["totsuka"] }],
        "notion_user_id": "u_me",
        "body_source": "page",
        "in_progress_statuses": ["実装中"],
        "status_map": { "レビュー待ち": "In Review" },
        "priority_map": { "High": 10 },
        "property_map": {
            "title": "Name",
            "status": "Status",
            "status_kind": "status",
            "assignee": "Owner",
            "priority": "Priority",
            "repo_hint": "Repo"
        }
    })
}

fn init_params() -> Value {
    json!({ "protocol_version": "0.1.0", "config": init_config() })
}

/// A database query page with four pages exercising every gating branch.
fn query_response() -> Value {
    json!({
        "has_more": false,
        "next_cursor": null,
        "results": [
            // Ingestable: right status, assigned to me (2nd), High priority.
            { "id": "P_1", "url": "https://notion.so/P_1", "properties": {
                "Name": { "title": [{ "plain_text": "Task one" }] },
                "Status": { "status": { "name": "実装待ち" } },
                "Owner": { "people": [{ "id": "u_other", "name": "Other" }, { "id": "u_me", "name": "Me" }] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [{ "plain_text": "totsuka" }] }
            } },
            // Assigned only to someone else → excluded (F-08).
            { "id": "P_2", "url": "https://notion.so/P_2", "properties": {
                "Name": { "title": [{ "plain_text": "Theirs" }] },
                "Status": { "status": { "name": "実装待ち" } },
                "Owner": { "people": [{ "id": "u_other", "name": "Other" }] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [] }
            } },
            // In-progress status. Under the `実装待ち` trigger this page is
            // rejected by the trigger match; the in-progress *gating* branch
            // (F-08) is covered separately by
            // `fetch_excludes_in_progress_on_statusless_trigger`.
            { "id": "P_3", "url": "https://notion.so/P_3", "properties": {
                "Name": { "title": [{ "plain_text": "Doing" }] },
                "Status": { "status": { "name": "実装中" } },
                "Owner": { "people": [] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [] }
            } },
            // Wrong status → filtered by the trigger.
            { "id": "P_4", "url": "https://notion.so/P_4", "properties": {
                "Name": { "title": [{ "plain_text": "Later" }] },
                "Status": { "status": { "name": "バックログ" } },
                "Owner": { "people": [] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [] }
            } }
        ]
    })
}

/// Two databases, each tracking a different repository (#542).
fn two_database_config() -> Value {
    let mut cfg = init_config();
    cfg["databases"] = json!([
        { "database_id": "DB1", "repos": ["totsuka"] },
        { "database_id": "DB2", "repos": ["web-app"] }
    ]);
    cfg
}

/// A one-page result for the second database: one page in a repository it
/// tracks, one in a repository it does not.
fn second_database_response() -> Value {
    json!({
        "has_more": false,
        "next_cursor": null,
        "results": [
            { "id": "P_20", "url": "https://notion.so/P_20", "properties": {
                "Name": { "title": [{ "plain_text": "Web task" }] },
                "Status": { "status": { "name": "実装待ち" } },
                "Owner": { "people": [] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [{ "plain_text": "web-app" }] }
            } },
            { "id": "P_21", "url": "https://notion.so/P_21", "properties": {
                "Name": { "title": [{ "plain_text": "Not ours" }] },
                "Status": { "status": { "name": "実装待ち" } },
                "Owner": { "people": [] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [{ "plain_text": "other" }] }
            } }
        ]
    })
}

/// A poll visits every configured database, and each one's `repos` gates its
/// own pages (#542).
#[tokio::test]
async fn a_poll_walks_every_database_and_each_one_gates_its_own_repos() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    shared.push(Canned::Data(query_response()));
    shared.push(Canned::Data(json!({ "results": [] }))); // P_1's page blocks
    shared.push(Canned::Data(second_database_response()));
    shared.push(Canned::Data(json!({ "results": [] }))); // P_20's page blocks
    let params = json!({
        "protocol_version": "0.5.1",
        "config": two_database_config(),
        "workflows": [
            { "workflow": "design", "trigger": { "status": "実装待ち" } }
        ],
        "poll_interval_secs": 60
    });
    let resp = call(&mut srv, 1, "initialize", params).await;
    assert!(resp.error.is_none(), "initialize failed: {:?}", resp.error);

    let first = harness.next_task().await;
    assert_eq!(first["id"], "P_1");
    let second = harness.next_task().await;
    assert_eq!(second["id"], "P_20");
    // `P_21` is on DB2 but its `Repo` names a repository DB2 does not track.
    harness.assert_no_task(Duration::from_millis(200)).await;

    let queried: Vec<String> = shared
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/query"))
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(queried, ["/databases/DB1/query", "/databases/DB2/query"]);
}

/// A page with **no** `repo_hint` passes every database's filter (#542).
///
/// Unlike a GitHub issue, a Notion page need not name a repository — the
/// property is optional. Dropping those would ingest nothing at all for anyone
/// who has not mapped `repo_hint`, so the filter only applies when there is a
/// value to filter on.
#[tokio::test]
async fn a_page_without_a_repo_hint_is_still_ingested() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    shared.push(Canned::Data(json!({
        "has_more": false, "next_cursor": null,
        "results": [
            { "id": "P_30", "url": "https://notion.so/P_30", "properties": {
                "Name": { "title": [{ "plain_text": "No repo" }] },
                "Status": { "status": { "name": "実装待ち" } },
                "Owner": { "people": [] },
                "Priority": { "select": { "name": "High" } },
                "Repo": { "rich_text": [] }
            } }
        ]
    })));
    shared.push(Canned::Data(json!({ "results": [] })));
    let params = json!({
        "protocol_version": "0.5.1",
        "config": init_config(),
        "workflows": [{ "workflow": "design", "trigger": { "status": "実装待ち" } }],
        "poll_interval_secs": 60
    });
    call(&mut srv, 1, "initialize", params).await;

    let task = harness.next_task().await;
    assert_eq!(task["id"], "P_30");
    assert!(task.get("repo_hint").is_none() || task["repo_hint"].is_null());
}

/// `initialize` answers with the repository → database mapping (protocol
/// 0.5.1), naming the properties an agent has to fill.
#[tokio::test]
async fn initialize_publishes_the_repository_to_database_mapping() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    let params = json!({ "protocol_version": "0.5.1", "config": two_database_config() });
    let resp = call(&mut srv, 1, "initialize", params).await;
    let claims = resp.result.expect("initialize result")["claimed_repos"].clone();
    assert_eq!(claims.as_array().map(Vec::len), Some(2));
    assert_eq!(claims[0]["repo"], "totsuka");
    let first = claims[0]["destination"].as_str().unwrap();
    assert!(first.contains("DB1"), "{first}");
    // The property names are the operator's, and an agent creating the page
    // cannot guess them.
    assert!(
        first.contains("`Name`") && first.contains("`Repo`"),
        "{first}"
    );
    assert_eq!(claims[1]["repo"], "web-app");
    assert!(
        claims[1]["destination"].as_str().unwrap().contains("DB2"),
        "{claims}"
    );
}

/// `task/update_status` asks Notion which database the page belongs to rather
/// than guessing, and checks the options on **that** database (#542).
#[tokio::test]
async fn update_status_verifies_options_on_the_page_own_database() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(
        &mut srv,
        1,
        "initialize",
        json!({ "protocol_version": "0.5.1", "config": two_database_config() }),
    )
    .await;

    // The page lives in DB2.
    shared.push(Canned::Data(json!({
        "id": "P_20", "parent": { "type": "database_id", "database_id": "DB2" }
    })));
    shared.push(Canned::Data(
        json!({ "properties": { "Status": { "status": {
        "options": [{ "name": "In Review" }] } } } }),
    ));
    shared.push(Canned::Data(json!({ "id": "P_20" })));

    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "P_20", "status": "レビュー待ち" }),
    )
    .await;
    assert!(resp.error.is_none(), "update failed: {:?}", resp.error);
    let paths: Vec<String> = shared.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, ["/pages/P_20", "/databases/DB2", "/pages/P_20"]);
}

/// Notion accepts database ids with or without hyphens and echoes back the
/// hyphenated form, so matching the page's parent against the configured id
/// must ignore them — otherwise a perfectly good config never matches and
/// every status transition fails with "not in `[[databases]]`".
#[tokio::test]
async fn a_hyphenated_parent_id_matches_a_config_written_without_hyphens() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    let mut config = init_config();
    config["databases"] = json!([
        { "database_id": "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d", "repos": ["totsuka"] }
    ]);
    call(
        &mut srv,
        1,
        "initialize",
        json!({ "protocol_version": "0.5.1", "config": config }),
    )
    .await;

    shared.push(Canned::Data(json!({
        "id": "P_1",
        "parent": {
            "type": "database_id",
            "database_id": "1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d"
        }
    })));
    shared.push(Canned::Data(
        json!({ "properties": { "Status": { "status": {
        "options": [{ "name": "In Review" }] } } } }),
    ));
    shared.push(Canned::Data(json!({ "id": "P_1" })));

    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "P_1", "status": "レビュー待ち" }),
    )
    .await;
    assert!(resp.error.is_none(), "update failed: {:?}", resp.error);
    // The GET went to the id **as configured**, not the hyphenated echo.
    assert_eq!(
        shared.requests()[1].path,
        "/databases/1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d"
    );
}

/// A page whose database is not configured is named, not silently patched
/// against some other database's options (#542).
#[tokio::test]
async fn update_status_refuses_a_page_from_an_unconfigured_database() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(
        &mut srv,
        1,
        "initialize",
        json!({ "protocol_version": "0.5.1", "config": two_database_config() }),
    )
    .await;

    shared.push(Canned::Data(json!({
        "id": "P_99", "parent": { "type": "database_id", "database_id": "DB_ELSEWHERE" }
    })));
    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "P_99", "status": "レビュー待ち" }),
    )
    .await;
    let err = resp.error.expect("an unconfigured database must error");
    assert!(err.message.contains("DB_ELSEWHERE"), "{}", err.message);
    // Nothing was written.
    let methods: Vec<HttpMethod> = shared.requests().iter().map(|r| r.method).collect();
    assert_eq!(methods, [HttpMethod::Get], "{methods:?}");
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

    // task/update_status → resolves which database the page lives in (#542:
    // the memo is empty here, so Notion is asked), maps レビュー待ち → "In
    // Review", verifies the option exists (DB fetch), then PATCHes the page.
    shared.push(Canned::Data(
        json!({ "id": "P_1", "parent": { "type": "database_id", "database_id": "DB1" } }),
    ));
    shared.push(Canned::Data(
        json!({ "properties": { "Status": { "status": {
        "options": [{ "name": "In Review" }, { "name": "実装待ち" }] } } } }),
    ));
    shared.push(Canned::Data(json!({ "id": "P_1" })));
    let resp = call(
        &mut srv,
        3,
        "task/update_status",
        json!({ "task_id": "P_1", "status": "レビュー待ち" }),
    )
    .await;
    assert!(resp.error.is_none(), "update failed: {:?}", resp.error);
    let patch = shared.last_request();
    assert_eq!(patch.method, HttpMethod::Patch);
    assert_eq!(patch.path, "/pages/P_1");
    assert_eq!(
        patch.body.unwrap()["properties"]["Status"]["status"]["name"],
        "In Review"
    );

    // `result/publish` is gone: the agent writes the deliverable itself.
    let resp = call(
        &mut srv,
        4,
        "result/publish",
        json!({ "task_id": "PAGE_1", "content": "x", "format": "markdown" }),
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
async fn fetch_excludes_in_progress_on_statusless_trigger() {
    // With a status-less trigger `{}` there is no server-side status filter, so
    // gating relies on `in_progress_statuses` (F-08). This is the production
    // path that exercises the `is_in_progress` branch directly.
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    shared.push(Canned::Data(json!({
        "has_more": false, "next_cursor": null,
        "results": [
            { "id": "P_todo", "url": "https://notion.so/P_todo", "properties": {
                "Name": { "title": [{ "plain_text": "Todo" }] },
                "Status": { "status": { "name": "実装待ち" } },
                "Owner": { "people": [] } } },
            { "id": "P_doing", "url": "https://notion.so/P_doing", "properties": {
                "Name": { "title": [{ "plain_text": "Doing" }] },
                "Status": { "status": { "name": "実装中" } },
                "Owner": { "people": [] } } }
        ]
    })));
    // body_source = page, so each surviving task fetches its blocks; only the
    // todo page survives, so exactly one block fetch follows.
    shared.push(Canned::Data(json!({ "has_more": false, "results": [] })));

    let params = json!({
        "protocol_version": "0.1.6",
        "config": init_config(),
        "workflows": [
            { "workflow": "design", "trigger": {} }
        ],
        "poll_interval_secs": 60
    });
    call(&mut srv, 1, "initialize", params).await;

    let task = harness.next_task().await;
    assert_eq!(
        task["id"], "P_todo",
        "the in-progress page is gated out (F-08)"
    );
    harness.assert_no_task(Duration::from_millis(200)).await;
    // No status filter was sent on a status-less trigger.
    assert!(
        shared.requests()[0]
            .body
            .as_ref()
            .unwrap()
            .get("filter")
            .is_none(),
        "no server-side filter without a trigger status"
    );
}

#[tokio::test]
async fn update_status_rejects_unknown_option() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;

    // The page's database is resolved first (#542), and that database's
    // property has no option matching the (mapped) target status.
    shared.push(Canned::Data(
        json!({ "id": "P_1", "parent": { "type": "database_id", "database_id": "DB1" } }),
    ));
    shared.push(Canned::Data(
        json!({ "properties": { "Status": { "status": {
        "options": [{ "name": "実装待ち" }] } } } }),
    ));
    let resp = call(
        &mut srv,
        2,
        "task/update_status",
        json!({ "task_id": "P_1", "status": "Nonexistent" }),
    )
    .await;
    let err = resp.error.expect("unknown status must error");
    assert!(
        err.message.contains("unknown status"),
        "got {}",
        err.message
    );
    // Only the parent lookup and the DB fetch happened — no PATCH was
    // attempted. Asserted on the methods, not just the count: a PATCH slipping
    // through is what this test exists to catch, and a bare count would also
    // pass if the two reads were replaced by a read and a write.
    let methods: Vec<HttpMethod> = shared.requests().iter().map(|r| r.method).collect();
    assert_eq!(methods, [HttpMethod::Get, HttpMethod::Get], "{methods:?}");
}

#[tokio::test]
async fn config_validate_reports_invalid_token() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // The users/me ping is rejected → config/validate returns invalid + guidance.
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
async fn config_validate_flags_missing_mapped_property() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // Token OK, but the database lacks the mapped `Priority` property (F-03/F-59).
    shared.push(Canned::Data(json!({ "type": "bot" })));
    shared.push(Canned::Data(json!({ "properties": {
        "Name": { "type": "title" },
        "Status": { "type": "status" },
        "Owner": { "type": "people" },
        "Repo": { "type": "rich_text" }
    } })));
    let resp = call(
        &mut srv,
        1,
        "config/validate",
        json!({ "config": init_config() }),
    )
    .await;
    let result = resp.result.unwrap();
    assert_eq!(result["valid"], false);
    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("Priority")),
        "expected the missing property named, got {errors:?}"
    );
}

#[tokio::test]
async fn config_validate_flags_static_problem_without_network() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // An empty database_id is caught statically; no transport call is made.
    let mut bad = init_config();
    bad["databases"] = json!([]);
    let resp = call(&mut srv, 1, "config/validate", json!({ "config": bad })).await;
    let result = resp.result.unwrap();
    assert_eq!(result["valid"], false);
    assert!(shared.requests().is_empty(), "no network on static failure");
}

/// The resident poll loop (0.1.6): `initialize` with triggers fetches each
/// trigger immediately and pushes the surviving tasks via `task/submit`.
#[tokio::test]
async fn initialize_with_triggers_polls_and_submits() {
    let shared = Shared::default();
    let (mut srv, mut harness) = server_with_harness(&shared);

    // The first tick runs before any sleep: the DB query page plus the
    // page-block fetch for the single surviving task (body_source = page).
    shared.push(Canned::Data(query_response()));
    shared.push(Canned::Data(json!({
        "has_more": false,
        "results": [
            { "type": "heading_2", "heading_2": { "rich_text": [{ "plain_text": "背景" }] } },
            { "type": "paragraph", "paragraph": { "rich_text": [{ "plain_text": "やること" }] } }
        ]
    })));
    let params = json!({
        "protocol_version": "0.1.6",
        "config": init_config(),
        "workflows": [
            { "workflow": "design", "trigger": { "status": "実装待ち" } }
        ],
        "poll_interval_secs": 60
    });
    let resp = call(&mut srv, 1, "initialize", params).await;
    assert!(resp.error.is_none(), "initialize failed: {:?}", resp.error);

    // Ingest gating (F-08): only the ingestable page survives (other-assignee,
    // in-progress, wrong-status excluded).
    let task = harness.next_task().await;
    assert_eq!(task["id"], "P_1");
    assert_eq!(task["source"], "notion");
    assert_eq!(task["title"], "Task one");
    assert_eq!(task["body"], "## 背景\nやること"); // page blocks → Markdown
    assert_eq!(task["repo_hint"], "totsuka"); // enables F-10
    assert_eq!(task["status"], "実装待ち");
    assert_eq!(task["priority"], 10); // High → 10 via priority_map
    assert_eq!(task["assignee"], "Me"); // my name surfaced even as 2nd assignee
    harness.assert_no_task(Duration::from_millis(200)).await;

    // The server-side query carried the status filter (efficiency).
    let query = shared.requests()[0].clone();
    assert_eq!(query.method, HttpMethod::Post);
    assert_eq!(query.path, "/databases/DB1/query");
    assert_eq!(
        query.body.unwrap()["filter"],
        json!({ "property": "Status", "status": { "equals": "実装待ち" } })
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
    assert!(shared.requests().is_empty(), "no fetch without triggers");
}

#[test]
fn shipped_manifest_is_valid_and_declares_push_source() {
    // The on-disk plugin.toml must parse and declare kind=task_source with
    // `task_submit` (push ingestion, 0.1.6) and the `source` output
    // capability (F-83) so the orchestrator accepts it and never polls it.
    let manifest = plugin_protocol::Manifest::from_toml_str(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(manifest.name, "notion");
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
        json!({ "task_id": "P_1", "status": "実装待ち" }),
    )
    .await;
    assert!(
        resp.error
            .expect("must error")
            .message
            .contains("initialize")
    );
}
