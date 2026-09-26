//! The protocol rules every task_source keeps, checked on the real binary by the
//! shared conformance kit (#767). What is left in `integration.rs` is what
//! only this plugin does.

use serde_json::json;

#[test]
fn the_binary_conforms_to_the_protocol() {
    let init = serde_json::from_value(json!({
        "protocol_version": plugin_protocol::PROTOCOL_VERSION,
        "config": { "token": "secret_test", "notion_user_id": "u_me" },
        "projects": [{ "name": "db-1", "options": { "database_id": "DB1" } }],
        "repositories": [{ "name": "totsuka", "project": "db-1" }],
        "workflows": [{ "workflow": "design", "projects": ["db-1"], "trigger": { "status": "実装待ち" } }]
    }))
    .expect("valid initialize params");
    let violations = plugin_conformance::check(
        env!("CARGO_BIN_EXE_notion"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"),
        &init,
    );
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}
