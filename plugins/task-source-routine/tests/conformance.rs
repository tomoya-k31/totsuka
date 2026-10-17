//! The protocol rules every task_source keeps, checked on the real binary by the
//! shared conformance kit (#767).

use serde_json::json;

#[test]
fn the_binary_conforms_to_the_protocol() {
    let config_dir = test_support::scratch("routine-conformance");
    let init = serde_json::from_value(json!({
        "protocol_version": plugin_protocol::PROTOCOL_VERSION,
        "config": {},
        "repositories": [{ "name": "totsuka" }],
        "workflows": [{ "workflow": "routine", "trigger": {} }],
        "config_dir": config_dir.display().to_string(),
        "host": "conformance",
    }))
    .expect("valid initialize params");
    let violations = plugin_conformance::check(
        env!("CARGO_BIN_EXE_routine"),
        concat!(env!("CARGO_MANIFEST_DIR"), "/plugin.toml"),
        &init,
    );
    assert!(violations.is_empty(), "\n{}", violations.join("\n"));
}
