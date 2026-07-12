//! End-to-end plugin flow over a recorded REST transport (no network):
//! initialize → tasks/fetch → normalize via the property map → page-body fetch
//! → task/update_status → result/publish (with block splitting), plus ingest
//! gating (F-08), unknown-status rejection (F-84), and config/validate against
//! the database schema (F-59).

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

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

fn server(shared: &Shared) -> Server<FakeFactory> {
    Server::new(FakeFactory {
        shared: shared.clone(),
    })
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
        "database_id": "DB1",
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
            // In-progress status → excluded (F-08).
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

#[tokio::test]
async fn full_flow_initialize_fetch_update_publish() {
    let shared = Shared::default();
    let mut srv = server(&shared);

    // initialize → declares outputs = ["source"].
    let resp = call(&mut srv, 1, "initialize", init_params()).await;
    let result = resp.result.expect("initialize result");
    assert_eq!(result["capabilities"]["outputs"], json!(["source"]));

    // tasks/fetch → only the ingestable page survives gating (F-08); its body
    // is the converted page blocks (a second, per-task request).
    shared.push(Canned::Data(query_response()));
    shared.push(Canned::Data(json!({
        "has_more": false,
        "results": [
            { "type": "heading_2", "heading_2": { "rich_text": [{ "plain_text": "背景" }] } },
            { "type": "paragraph", "paragraph": { "rich_text": [{ "plain_text": "やること" }] } }
        ]
    })));
    let resp = call(
        &mut srv,
        2,
        "tasks/fetch",
        json!({ "trigger": { "status": "実装待ち" } }),
    )
    .await;
    let tasks = resp.result.expect("fetch result")["tasks"].clone();
    let tasks = tasks.as_array().unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "other-assignee, in-progress, wrong-status excluded"
    );
    let t = &tasks[0];
    assert_eq!(t["id"], "P_1");
    assert_eq!(t["source"], "notion");
    assert_eq!(t["title"], "Task one");
    assert_eq!(t["body"], "## 背景\nやること"); // page blocks → Markdown
    assert_eq!(t["repo_hint"], "totsuka"); // enables F-10
    assert_eq!(t["status"], "実装待ち");
    assert_eq!(t["priority"], 10); // High → 10 via priority_map
    assert_eq!(t["assignee"], "Me"); // my name surfaced even as 2nd assignee

    // The server-side query carried the status filter (efficiency).
    let query = shared.requests()[0].clone();
    assert_eq!(query.method, HttpMethod::Post);
    assert_eq!(query.path, "/databases/DB1/query");
    assert_eq!(
        query.body.unwrap()["filter"],
        json!({ "property": "Status", "status": { "equals": "実装待ち" } })
    );

    // task/update_status → maps レビュー待ち → "In Review", verifies the option
    // exists (DB fetch), then PATCHes the page property.
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

    // result/publish → appends converted blocks to the page (F-07).
    shared.push(Canned::Data(json!({ "results": [] })));
    let resp = call(
        &mut srv,
        4,
        "result/publish",
        json!({ "task_id": "P_1", "content": "# Design\n- point", "format": "markdown" }),
    )
    .await;
    assert!(resp.error.is_none(), "publish failed: {:?}", resp.error);
    let append = shared.last_request();
    assert_eq!(append.method, HttpMethod::Patch);
    assert_eq!(append.path, "/blocks/P_1/children");
    let children = append.body.unwrap()["children"].clone();
    assert_eq!(children[0]["type"], "heading_1");
    assert_eq!(children[1]["type"], "bulleted_list_item");
}

#[tokio::test]
async fn publish_splits_long_content_over_2000_chars() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;

    // One 2500-char paragraph → two blocks, appended in a single request.
    shared.push(Canned::Data(json!({ "results": [] })));
    let long = "あ".repeat(2500);
    let resp = call(
        &mut srv,
        2,
        "result/publish",
        json!({ "task_id": "P_1", "content": long }),
    )
    .await;
    assert!(resp.error.is_none(), "publish failed: {:?}", resp.error);
    let children = shared.last_request().body.unwrap()["children"].clone();
    let children = children.as_array().unwrap();
    assert_eq!(children.len(), 2, "2500 chars split at the 2000 limit");
    let first = children[0]["paragraph"]["rich_text"][0]["text"]["content"]
        .as_str()
        .unwrap();
    assert_eq!(first.chars().count(), 2000);
}

#[tokio::test]
async fn update_status_rejects_unknown_option() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    call(&mut srv, 1, "initialize", init_params()).await;

    // The property has no option matching the (mapped) target status.
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
    // Only the DB fetch happened — no PATCH was attempted.
    assert_eq!(shared.requests().len(), 1);
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
    bad["database_id"] = json!("");
    let resp = call(&mut srv, 1, "config/validate", json!({ "config": bad })).await;
    let result = resp.result.unwrap();
    assert_eq!(result["valid"], false);
    assert!(shared.requests().is_empty(), "no network on static failure");
}

#[test]
fn shipped_manifest_is_valid_and_declares_source_output() {
    // The on-disk plugin.toml must parse and declare kind=task_source with the
    // `source` output capability (F-83) so the orchestrator accepts it.
    let manifest = plugin_protocol::Manifest::from_toml_str(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(manifest.name, "notion");
    assert_eq!(manifest.kind, plugin_protocol::PluginKind::TaskSource);
    assert_eq!(
        manifest.capabilities.outputs,
        vec![plugin_protocol::OutputCapability::Source]
    );
    assert!(
        manifest.is_compatible_with(&plugin_protocol::protocol_version()),
        "manifest must accept the current protocol version"
    );
}

#[tokio::test]
async fn methods_before_initialize_are_rejected() {
    let shared = Shared::default();
    let mut srv = server(&shared);
    let resp = call(&mut srv, 1, "tasks/fetch", json!({ "trigger": {} })).await;
    assert!(
        resp.error
            .expect("must error")
            .message
            .contains("initialize")
    );
}
