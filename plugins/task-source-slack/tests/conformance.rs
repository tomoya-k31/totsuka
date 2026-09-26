//! The protocol rules every task_source keeps, checked on the real binary by
//! the shared conformance kit (#767). What is left in `integration.rs` is what
//! only this plugin does.

use serde_json::json;

#[test]
fn the_binary_conforms_to_the_protocol() {
    let init = serde_json::from_value(json!({
        "protocol_version": plugin_protocol::PROTOCOL_VERSION,
        "config": {
            "state_dir": std::env::temp_dir().join("totsuka-slack-conformance"),
            "app_token": "xapp-1-A1-test",
            "user_token": "xoxp-user-test",
            "target_user_id": "U_ME"
        },
        // One candidate: with none, initialize refuses for that reason alone.
        "repositories": [{ "name": "web-app" }],
        "workflows": [{ "workflow": "slack-reply", "trigger": { "mention": true } }]
    }))
    .expect("valid initialize params");
    let violations = plugin_conformance::check(
        env!("CARGO_BIN_EXE_slack"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"),
        &init,
    );
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}
