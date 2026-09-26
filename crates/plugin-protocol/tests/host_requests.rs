//! Every method constant has a declared place (#767).
//!
//! [`HOST_REQUESTS`] is what the conformance kit probes before `initialize`.
//! A method added to [`method`] but to neither that table nor one of the
//! exclusions below would silently escape the probe — the very "forgot to add
//! it to one of the copies" failure #767 removed. So this reads the constants
//! out of the source, the only list of them there is, and demands a decision
//! for each.

use std::collections::BTreeSet;

use plugin_protocol::method;
use plugin_protocol::methods::HOST_REQUESTS;

/// Requests every kind serves; not kind-specific, so not in the table.
const COMMON: &[&str] = &[
    method::INITIALIZE,
    method::CONFIG_VALIDATE,
    method::SHUTDOWN,
];
/// P→O requests: the plugin sends them, the host answers.
const PLUGIN_TO_HOST: &[&str] = &[method::TASK_SUBMIT, method::TASK_LOOKUP];
/// Notifications: never answered, so there is nothing to refuse.
const NOTIFICATIONS: &[&str] = &[method::STATE_NOTIFICATION, method::NOTIFY];

/// The string values of every `pub const` inside `pub mod method { … }`.
fn declared_methods() -> BTreeSet<String> {
    let source = include_str!("../src/methods.rs");
    let body = source
        .split_once("pub mod method {")
        .expect("`pub mod method` is in methods.rs")
        .1
        .split_once("\n}")
        .expect("`pub mod method` is closed")
        .0;
    let found: BTreeSet<String> = body
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub const "))
        .map(|decl| {
            decl.split('"')
                .nth(1)
                .unwrap_or_else(|| panic!("not a string constant: {decl}"))
                .to_string()
        })
        .collect();
    // An empty set would pass the comparison below vacuously.
    assert!(found.len() >= 18, "parsed only {found:?}");
    found
}

#[test]
fn every_method_is_placed_exactly_once() {
    let mut placed = Vec::new();
    placed.extend(HOST_REQUESTS.iter().map(|r| r.method));
    placed.extend(COMMON);
    placed.extend(PLUGIN_TO_HOST);
    placed.extend(NOTIFICATIONS);

    let unique: BTreeSet<String> = placed.iter().map(|m| m.to_string()).collect();
    assert_eq!(
        unique.len(),
        placed.len(),
        "a method is placed twice: {placed:?}"
    );
    assert_eq!(
        unique,
        declared_methods(),
        "every `method` constant must be in `HOST_REQUESTS` or in one of this \
         test's exclusion lists"
    );
}
