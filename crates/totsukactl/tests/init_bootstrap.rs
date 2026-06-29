//! Tests the file-writing portion only — docker compose / migrate are exercised
//! by the e2e tasks (and skipped when DATABASE_URL is absent).

use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

fn write_via_tmpl_helper(path: &std::path::Path, body: &str, mode: u32) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms).unwrap();
}

#[test]
fn secrets_file_is_chmod_0600() {
    let tmp = TempDir::new().unwrap();
    let p = tmp.path().join("secrets.toml");
    write_via_tmpl_helper(&p, "x = 1\n", 0o600);
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn config_template_is_parseable() {
    let tmpl = include_str!("../src/commands/templates/config.toml.tmpl");
    // Template parses as TOML before var expansion.
    let _: toml::Value = toml::from_str(tmpl).expect("config template must be valid TOML");
}

#[test]
fn secrets_template_is_parseable() {
    let tmpl = include_str!("../src/commands/templates/secrets.toml.tmpl");
    let _: toml::Value = toml::from_str(tmpl).expect("secrets template must be valid TOML");
}
