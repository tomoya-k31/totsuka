//! The protocol rules every task_source keeps, checked on the real binary by the
//! shared conformance kit (#767). What is left in `integration.rs` is what
//! only this plugin does.

use serde_json::json;

#[test]
fn the_binary_conforms_to_the_protocol() {
    let init = serde_json::from_value(json!({
        "protocol_version": plugin_protocol::PROTOCOL_VERSION,
        "config": { "bot_token": "bot-token", "operator_user_id": "111111111111111111" },
        "repositories": [{ "name": "my-docs" }],
        "workflows": [{
            "workflow": "clip",
            "trigger": { "channel": "222222222222222222", "channel_name": "clip", "repo": "my-docs" }
        }]
    }))
    .expect("valid initialize params");
    let violations = plugin_conformance::check(
        env!("CARGO_BIN_EXE_discord"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"),
        &init,
    );
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}
