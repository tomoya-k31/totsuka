//! End-to-end plugin flow over a recording fake sender (no real osascript):
//! initialize → `notify` for each event kind → delivery, plus the workflow ×
//! event filter (F-92) and fire-and-forget resilience to a failing send (F-93).

use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use notifier_macos::config::NotifierConfig;
use notifier_macos::error::NotifierError;
use notifier_macos::sender::{Notice, NotificationSender};
use notifier_macos::server::{Reply, SenderFactory, Server};
use plugin_protocol::jsonrpc::Response;

/// A sender that records every notice, and can be made to fail.
#[derive(Clone, Default)]
struct FakeSender {
    sent: Arc<Mutex<Vec<Notice>>>,
    fail: Arc<Mutex<bool>>,
}

impl FakeSender {
    fn titles(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|n| n.title.clone())
            .collect()
    }
    fn set_fail(&self, fail: bool) {
        *self.fail.lock().unwrap() = fail;
    }
}

impl NotificationSender for FakeSender {
    fn send(&self, notice: Notice) -> impl Future<Output = Result<(), NotifierError>> + Send {
        let fail = *self.fail.lock().unwrap();
        if !fail {
            self.sent.lock().unwrap().push(notice);
        }
        async move { outcome(fail) }
    }

    fn probe(&self) -> impl Future<Output = Result<(), NotifierError>> + Send {
        let fail = *self.fail.lock().unwrap();
        async move { outcome(fail) }
    }
}

/// Success unless the sender was told to fail.
fn outcome(fail: bool) -> Result<(), NotifierError> {
    if fail {
        Err(NotifierError::Failed {
            bin: "osascript".into(),
            code: 1,
            stderr: "boom".into(),
        })
    } else {
        Ok(())
    }
}

struct FakeFactory {
    sender: FakeSender,
}

impl SenderFactory for FakeFactory {
    type Sender = FakeSender;
    fn build(&self, _config: &NotifierConfig) -> FakeSender {
        self.sender.clone()
    }
}

fn server(sender: &FakeSender) -> Server<FakeFactory> {
    Server::new(FakeFactory {
        sender: sender.clone(),
    })
}

/// Send a request line and parse the response.
async fn call(srv: &mut Server<FakeFactory>, id: i64, method: &str, params: Value) -> Response {
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let reply = srv.handle_line(&line.to_string()).await;
    serde_json::from_str(&reply.line.expect("a response line")).expect("valid response")
}

/// Send a `notify` notification (no id → no reply) and let its spawned delivery
/// task run.
async fn notify(srv: &mut Server<FakeFactory>, params: Value) {
    let line = json!({ "jsonrpc": "2.0", "method": "notify", "params": params });
    let reply: Reply = srv.handle_line(&line.to_string()).await;
    assert!(reply.line.is_none(), "notify must not produce a response");
    // The send is spawned; yield so the delivery task completes before asserting.
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

fn init_params(config: Value) -> Value {
    json!({ "protocol_version": "0.1.0", "config": config })
}

#[tokio::test]
async fn delivers_all_four_event_kinds() {
    let sender = FakeSender::default();
    let mut srv = server(&sender);
    let resp = call(&mut srv, 1, "initialize", init_params(json!({}))).await;
    assert!(resp.result.is_some(), "initialize result");

    for event in ["waiting_input", "done", "failed", "pending"] {
        notify(
            &mut srv,
            json!({ "event": event, "task_id": "T1", "title": "A task" }),
        )
        .await;
    }
    let titles = sender.titles();
    assert_eq!(titles.len(), 4, "all four events delivered: {titles:?}");
    assert!(titles.iter().any(|t| t.contains("入力待ち")));
    assert!(titles.iter().any(|t| t.contains("完了")));
    assert!(titles.iter().any(|t| t.contains("失敗")));
    assert!(titles.iter().any(|t| t.contains("確認待ち")));
}

#[tokio::test]
async fn filter_suppresses_configured_events() {
    let sender = FakeSender::default();
    let mut srv = server(&sender);
    // Globally suppress `done`; re-enable it only for the `release` workflow.
    call(
        &mut srv,
        1,
        "initialize",
        init_params(json!({
            "filter": {
                "events": { "done": false },
                "workflows": { "release": { "done": true } }
            }
        })),
    )
    .await;

    // done on a normal workflow → suppressed.
    notify(
        &mut srv,
        json!({ "event": "done", "workflow": "impl", "title": "t" }),
    )
    .await;
    // done on release → delivered.
    notify(
        &mut srv,
        json!({ "event": "done", "workflow": "release", "title": "t" }),
    )
    .await;
    // failed anywhere → delivered (not suppressed).
    notify(
        &mut srv,
        json!({ "event": "failed", "workflow": "impl", "title": "t" }),
    )
    .await;

    let titles = sender.titles();
    assert_eq!(
        titles.len(),
        2,
        "only release-done and failed delivered: {titles:?}"
    );
    assert!(titles.iter().any(|t| t.contains("完了")));
    assert!(titles.iter().any(|t| t.contains("失敗")));
}

#[tokio::test]
async fn failing_send_is_swallowed_and_server_keeps_serving() {
    // Fire-and-forget (F-93): a delivery failure must not surface or break the
    // server — a subsequent request still gets a normal response.
    let sender = FakeSender::default();
    sender.set_fail(true);
    let mut srv = server(&sender);
    call(&mut srv, 1, "initialize", init_params(json!({}))).await;

    notify(&mut srv, json!({ "event": "done", "title": "t" })).await;
    assert!(
        sender.titles().is_empty(),
        "the failing send recorded nothing"
    );

    // The server is unaffected: shutdown still responds.
    let line = json!({ "jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {} });
    let reply = srv.handle_line(&line.to_string()).await;
    assert!(reply.shutdown);
    assert!(
        reply.line.is_some(),
        "shutdown still replies after a failed send"
    );
}

#[tokio::test]
async fn notify_before_initialize_is_ignored() {
    let sender = FakeSender::default();
    let mut srv = server(&sender);
    // No initialize yet: a notify is dropped, not delivered, and never panics.
    notify(&mut srv, json!({ "event": "done", "title": "t" })).await;
    assert!(sender.titles().is_empty());
}

#[tokio::test]
async fn config_validate_reports_a_failing_notifier() {
    let sender = FakeSender::default();
    sender.set_fail(true);
    let mut srv = server(&sender);
    let resp = call(&mut srv, 1, "config/validate", json!({ "config": {} })).await;
    let result = resp.result.expect("validate always replies");
    assert_eq!(result["valid"], false);
    assert!(!result["errors"].as_array().unwrap().is_empty());
}

#[test]
fn shipped_manifest_is_a_notifier() {
    let manifest = plugin_protocol::Manifest::from_toml_str(include_str!("../plugin.toml"))
        .expect("plugin.toml parses");
    assert_eq!(manifest.name, "macos");
    assert_eq!(manifest.kind, plugin_protocol::PluginKind::Notifier);
    assert!(manifest.is_compatible_with(&plugin_protocol::protocol_version()));
}
