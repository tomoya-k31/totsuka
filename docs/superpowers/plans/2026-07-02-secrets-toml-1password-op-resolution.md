# secrets.toml 1Password (`op://`) Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `secrets.toml` hold 1Password Secret References (`op://Vault/Item/field`) instead of plaintext, resolved via the `op` CLI during `Config::load()`.

**Architecture:** Add a new `op_resolve` module to `totsuka-config` that shells out to `op read <uri>` with a testable `resolve_with(bin, uri)` seam. Thread an `op_lookup` closure through the existing `expand_toml_value`/`expand_string_leaf` tree-walk in `expand.rs` (the same pass that already handles `${name}`/`${env:NAME}`/`~`), so any string leaf whose entire value starts with `op://` is resolved verbatim instead of var-expanded. `Config::load()`/`from_toml_str()` build this closure with an in-process cache so a repeated reference only spawns `op` once.

**Tech Stack:** Rust, `std::process::Command` (no new crate dependency), `tempfile` (already a dev-dependency) for test fixtures.

## Global Constraints

- No new crate dependency — `op` is invoked as an external binary via `std::process::Command`, matching the spec.
- Fail-closed: any `op read` failure aborts `Config::load()` via the existing `LoadError::Expand(#[from] ExpandError)` path. No fallback to a default/empty secret.
- Error messages must never include resolved secret material — only the `op://` URI itself (vault/item/field names, never secret values).
- `#![forbid(unsafe_code)]` is already enforced crate-wide in `totsuka-config` — this feature needs no `unsafe`.
- Any test that mutates `std::env::set_var("PATH", ...)` must be the sole `#[test]` function in its file, since `cargo test` runs multiple `#[test]` fns within one file as threads of one process (shared env), but each `tests/*.rs` file is its own process (no cross-file race).
- Spec reference: `docs/superpowers/specs/2026-07-02-secrets-toml-1password-op-resolution-design.md`.

---

### Task 1: `op_resolve` module — shell out to `op read` with a testable seam

**Files:**
- Modify: `crates/totsuka-config/src/expand.rs:25-33` (add 3 `ExpandError` variants)
- Modify: `crates/totsuka-config/src/lib.rs:2-6` (add `pub mod op_resolve;`)
- Create: `crates/totsuka-config/src/op_resolve.rs`

**Interfaces:**
- Consumes: `crate::expand::ExpandError` (existing enum, gains 3 variants this task).
- Produces: `pub fn resolve_with(bin: &str, uri: &str) -> Result<String, ExpandError>` (testable — binary path is a parameter) and `pub fn resolve(uri: &str) -> Result<String, ExpandError>` (production entry point, always invokes `"op"`). Task 2 calls `op_resolve::resolve`.

- [ ] **Step 1: Write the failing tests**

Create `crates/totsuka-config/src/op_resolve.rs` with only the test module (no implementation yet), so the tests fail to compile against a not-yet-existing module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn resolve_success_strips_trailing_newline() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\necho resolved-secret\n");
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "resolved-secret"
        );
    }

    #[test]
    fn resolve_success_no_trailing_newline_unchanged() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\nprintf 'no-newline-value'\n");
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "no-newline-value"
        );
    }

    #[test]
    fn resolve_success_strips_trailing_crlf() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\nprintf 'value\\r\\n'\n");
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "value"
        );
    }

    #[test]
    fn resolve_passes_read_and_uri_as_args() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir.path(),
            "fake-op",
            "#!/bin/sh\nif [ \"$1\" = read ] && [ \"$2\" = \"op://Vault/Item/field\" ]; then echo ok; else echo wrong-args; exit 1; fi\n",
        );
        assert_eq!(
            resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap(),
            "ok"
        );
    }

    #[test]
    fn resolve_nonzero_exit_is_op_failed() {
        let dir = TempDir::new().unwrap();
        let script = write_script(
            &dir.path(),
            "fake-op",
            "#!/bin/sh\necho 'not signed in' >&2\nexit 1\n",
        );
        match resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap_err() {
            ExpandError::OpFailed(uri, stderr) => {
                assert_eq!(uri, "op://Vault/Item/field");
                assert!(stderr.contains("not signed in"));
            }
            e => panic!("expected OpFailed, got {:?}", e),
        }
    }

    #[test]
    fn resolve_non_utf8_output_is_op_non_utf8() {
        let dir = TempDir::new().unwrap();
        let script = write_script(&dir.path(), "fake-op", "#!/bin/sh\nprintf '\\xff\\xfe'\n");
        match resolve_with(script.to_str().unwrap(), "op://Vault/Item/field").unwrap_err() {
            ExpandError::OpNonUtf8(uri) => assert_eq!(uri, "op://Vault/Item/field"),
            e => panic!("expected OpNonUtf8, got {:?}", e),
        }
    }

    #[test]
    fn resolve_missing_binary_is_op_exec() {
        match resolve_with(
            "/nonexistent/path/to/binary-that-does-not-exist",
            "op://Vault/Item/field",
        )
        .unwrap_err()
        {
            ExpandError::OpExec(uri, _) => assert_eq!(uri, "op://Vault/Item/field"),
            e => panic!("expected OpExec, got {:?}", e),
        }
    }

}
```

`resolve()` itself (the one-line `resolve_with("op", uri)` wrapper) is not unit-tested here — testing that it invokes a binary literally named `"op"` would require `PATH` mutation, which this task's tests deliberately avoid (see Global Constraints). That coverage comes from Task 2's integration test, which exercises the real `op_resolve::resolve` end-to-end via a fake `op` script placed on `PATH`.

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test -p totsuka-config op_resolve -- --nocapture`
Expected: compile error, e.g. `cannot find function `resolve_with` in this scope` and `cannot find type `ExpandError` in this scope` (module has no `use` or implementation yet).

- [ ] **Step 3: Add the three `ExpandError` variants**

In `crates/totsuka-config/src/expand.rs`, extend the existing enum (currently lines 25-33):

```rust
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ExpandError {
    #[error("undefined variable: {0}")]
    Undefined(String),
    #[error("undefined env variable: {0}")]
    UndefinedEnv(String),
    #[error("cyclic reference involving: {0}")]
    Cycle(String),
    #[error("op CLI could not be executed for {0}: {1}")]
    OpExec(String, String),
    #[error("op read failed for {0}: {1}")]
    OpFailed(String, String),
    #[error("op read returned non-UTF8 output for {0}")]
    OpNonUtf8(String),
}
```

- [ ] **Step 4: Implement `resolve_with` and `resolve`**

At the top of `crates/totsuka-config/src/op_resolve.rs` (above the `#[cfg(test)] mod tests` block already written in Step 1):

```rust
use crate::expand::ExpandError;
use std::process::Command;

/// Resolve one `op://vault/item/field` reference by invoking `op read`.
/// `bin` is the binary to invoke — a parameter (not hardcoded `"op"`) so
/// tests can point it at a fake script instead of mutating `PATH`.
pub fn resolve_with(bin: &str, uri: &str) -> Result<String, ExpandError> {
    let output = Command::new(bin)
        .args(["read", uri])
        .output()
        .map_err(|e| ExpandError::OpExec(uri.to_string(), e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ExpandError::OpFailed(uri.to_string(), stderr));
    }

    let mut s = String::from_utf8(output.stdout)
        .map_err(|_| ExpandError::OpNonUtf8(uri.to_string()))?;
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    Ok(s)
}

/// Production entry point — always invokes the real `op` CLI on `PATH`.
pub fn resolve(uri: &str) -> Result<String, ExpandError> {
    resolve_with("op", uri)
}
```

Add the module declaration in `crates/totsuka-config/src/lib.rs`. Current lines 2-6:

```rust
pub mod env_override;
pub mod expand;
pub mod path_expand;
pub mod schema;
pub mod validate;
```

Change to (alphabetical, `op_resolve` between `expand` and `path_expand`):

```rust
pub mod env_override;
pub mod expand;
pub mod op_resolve;
pub mod path_expand;
pub mod schema;
pub mod validate;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p totsuka-config op_resolve -- --nocapture`
Expected: all 7 tests in `op_resolve::tests` pass (`resolve_success_strips_trailing_newline`, `resolve_success_no_trailing_newline_unchanged`, `resolve_success_strips_trailing_crlf`, `resolve_passes_read_and_uri_as_args`, `resolve_nonzero_exit_is_op_failed`, `resolve_non_utf8_output_is_op_non_utf8`, `resolve_missing_binary_is_op_exec`).

- [ ] **Step 6: Run the whole crate's existing tests to confirm no regression**

Run: `cargo test -p totsuka-config`
Expected: all pre-existing tests in `expand.rs`, `lib.rs`, and `tests/load_secrets_merge.rs` still pass unchanged (this task only added an enum variant set and a new module — no existing signature changed yet).

- [ ] **Step 7: Commit**

```bash
git add crates/totsuka-config/src/expand.rs crates/totsuka-config/src/lib.rs crates/totsuka-config/src/op_resolve.rs
git commit -m "feat(totsuka-config): add op_resolve module for 1Password op:// lookups"
```

---

### Task 2: Wire `op://` resolution into the expansion pipeline

**Files:**
- Modify: `crates/totsuka-config/src/expand.rs` (`expand_string_leaf`, `expand_toml_value`, plus their doc comments and existing test-module additions)
- Modify: `crates/totsuka-config/src/lib.rs` (`Config::load`, `Config::from_toml_str` — both call sites of `expand_toml_value`)
- Test: `crates/totsuka-config/src/expand.rs` (unit tests, fake closures — no subprocess)
- Create: `crates/totsuka-config/tests/load_secrets_op_resolve.rs` (integration test, real `op_resolve::resolve` via a fake `op` script on `PATH` — the one `PATH`-mutating test in the suite)

**Interfaces:**
- Consumes: `op_resolve::resolve(uri: &str) -> Result<String, ExpandError>` and the `ExpandError::{OpExec, OpFailed, OpNonUtf8}` variants from Task 1.
- Produces: `expand_string_leaf<F, O>(s: &str, vars: &HashMap<String, String>, env_lookup: &F, op_lookup: &O) -> Result<String, ExpandError>` and `pub fn expand_toml_value<F, O>(value: &mut toml::Value, vars: &HashMap<String, String>, env_lookup: &F, op_lookup: &O) -> Result<(), ExpandError>` where `O: Fn(&str) -> Result<String, ExpandError>`. No other task depends on these beyond this one (Task 3 is docs-only).

- [ ] **Step 1: Write the failing unit tests in `expand.rs`**

Add to the `#[cfg(test)] mod tests` block in `crates/totsuka-config/src/expand.rs` (after the existing `cycle_errors` test):

```rust
    fn ok_op_lookup() -> impl Fn(&str) -> Result<String, ExpandError> {
        |uri: &str| Ok(format!("resolved:{uri}"))
    }

    fn failing_op_lookup() -> impl Fn(&str) -> Result<String, ExpandError> {
        |uri: &str| Err(ExpandError::OpFailed(uri.to_string(), "boom".into()))
    }

    #[test]
    fn op_ref_resolved_via_lookup() {
        let vars = HashMap::new();
        assert_eq!(
            expand_string_leaf(
                "op://Vault/Item/field",
                &vars,
                &empty_env(),
                &ok_op_lookup()
            )
            .unwrap(),
            "resolved:op://Vault/Item/field"
        );
    }

    #[test]
    fn op_ref_failure_propagates() {
        let vars = HashMap::new();
        assert_eq!(
            expand_string_leaf(
                "op://Vault/Item/field",
                &vars,
                &empty_env(),
                &failing_op_lookup()
            )
            .unwrap_err(),
            ExpandError::OpFailed("op://Vault/Item/field".into(), "boom".into())
        );
    }

    #[test]
    fn op_ref_is_not_var_expanded() {
        let mut vars = HashMap::new();
        vars.insert("x".into(), "should-not-appear".into());
        assert_eq!(
            expand_string_leaf(
                "op://Vault/${x}/field",
                &vars,
                &empty_env(),
                &ok_op_lookup()
            )
            .unwrap(),
            "resolved:op://Vault/${x}/field"
        );
    }

    #[test]
    fn non_op_string_unaffected_by_op_lookup_param() {
        let vars = HashMap::new();
        assert_eq!(
            expand_string_leaf("/plain/path", &vars, &empty_env(), &failing_op_lookup()).unwrap(),
            "/plain/path"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test -p totsuka-config expand:: -- --nocapture`
Expected: compile error — `expand_string_leaf` takes 3 arguments in existing code, these calls pass 4 (`this function takes 3 arguments but 4 arguments were supplied`).

- [ ] **Step 3: Update `expand_string_leaf` and `expand_toml_value` to accept and thread `op_lookup`**

Replace the current `expand_string_leaf` (lines 5-23 of `expand.rs`):

```rust
/// Expand a single string leaf. A whole-value `op://...` reference is
/// resolved verbatim via `op_lookup` (no further `${...}`/`~` expansion is
/// applied to the secret it resolves to). Otherwise: first var/env expansion
/// (lenient), then leading-tilde expansion using HOME from `env_lookup`.
fn expand_string_leaf<F, O>(
    s: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    op_lookup: &O,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
    O: Fn(&str) -> Result<String, ExpandError>,
{
    if s.starts_with("op://") {
        return op_lookup(s);
    }
    let expanded = expand_vars_lenient(s, vars, env_lookup)?;
    let home = env_lookup("HOME");
    Ok(crate::path_expand::resolve_tilde(
        &expanded,
        home.as_deref(),
    ))
}
```

Replace the current `expand_toml_value` (lines 99-124 of `expand.rs`):

```rust
/// Walk a `toml::Value` tree and expand `${name}` / `${env:NAME}` / `op://...`
/// references in every string leaf. `vars` should be collected from a
/// top-level `[vars]` table; `env_lookup` provides env-var fallback;
/// `op_lookup` resolves whole-value `op://vault/item/field` references (see
/// `expand_string_leaf`).
///
/// **Lenient mode for unknown `${name}`**: when a `${name}` reference cannot be
/// resolved from `vars`, the original token is left in place rather than
/// raising `ExpandError::Undefined`. This keeps backward compatibility with
/// configs whose variables live in non-top-level sections (e.g.
/// `[agent_adapter.vars]`) — the unresolved leaves still hit `validate()` and
/// the orchestrator can decide what to do with them. Cycles, `${env:NAME}`
/// misses, and `op://` resolution failures remain hard errors.
pub fn expand_toml_value<F, O>(
    value: &mut toml::Value,
    vars: &HashMap<String, String>,
    env_lookup: &F,
    op_lookup: &O,
) -> Result<(), ExpandError>
where
    F: Fn(&str) -> Option<String>,
    O: Fn(&str) -> Result<String, ExpandError>,
{
    match value {
        toml::Value::String(s) => {
            *s = expand_string_leaf(s, vars, env_lookup, op_lookup)?;
        }
        toml::Value::Array(arr) => {
            for v in arr.iter_mut() {
                expand_toml_value(v, vars, env_lookup, op_lookup)?;
            }
        }
        toml::Value::Table(tbl) => {
            for (_, v) in tbl.iter_mut() {
                expand_toml_value(v, vars, env_lookup, op_lookup)?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

- [ ] **Step 4: Update the two `expand_toml_value` call sites in `lib.rs`**

In `crates/totsuka-config/src/lib.rs`, add this import near the top (with the other `use` lines):

```rust
use std::cell::RefCell;
```

In `Config::load` (currently around line 72-73):

```rust
        // 3. Expand every string leaf in place. `op://` refs are resolved via
        //    `op read`, cached per-call so a repeated reference only spawns
        //    `op` once.
        let op_cache = RefCell::new(HashMap::<String, String>::new());
        let op_lookup = |uri: &str| -> Result<String, ExpandError> {
            if let Some(v) = op_cache.borrow().get(uri) {
                return Ok(v.clone());
            }
            let resolved = crate::op_resolve::resolve(uri)?;
            op_cache.borrow_mut().insert(uri.to_string(), resolved.clone());
            Ok(resolved)
        };
        expand_toml_value(&mut tree, &vars, &|name| std::env::var(name).ok(), &op_lookup)?;
```

Apply the identical change to the equivalent block in `Config::from_toml_str` (currently around line 106-107) — same five lines, same variable names, since it's the same pipeline step duplicated for the in-process/string-literal loading path.

- [ ] **Step 5: Run tests to verify the unit tests pass and nothing else broke**

Run: `cargo test -p totsuka-config`
Expected: all tests pass, including the 4 new tests from Step 1 and every pre-existing test in `expand.rs`, `lib.rs`, and `tests/load_secrets_merge.rs`.

- [ ] **Step 6: Write the failing integration test**

Create `crates/totsuka-config/tests/load_secrets_op_resolve.rs`:

```rust
//! This is the only test in this file, and the only test in the whole
//! `totsuka-config` test suite that mutates `PATH`. Each `tests/*.rs` file
//! compiles to its own process, so this cannot race other test files; a
//! second `#[test]` fn added to *this* file would race it via shared
//! process env, so keep this file single-test (see plan Task 2 constraint
//! in docs/superpowers/plans/2026-07-02-secrets-toml-1password-op-resolution.md).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use totsuka_config::Config;

const CONFIG_TOML: &str = r#"
[totsuka]
state_dir = "/var/state"
data_dir  = "/var/data"

[supervisor]
[supervisor.heartbeat]

[postgres]
image="ghcr.io/pgmq/pg18-pgmq:v1.11.1"
container="totsuka-pgmq"
host="127.0.0.1"
port=5432
database="totsuka"
user="postgres"
volume="totsuka_pgmq_data"
compose_file="deploy/docker-compose.yml"

[bus]
queue_name="totsuka_events"

[agent_adapter]
uds_path="/sock/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true

[orchestrator]
uds_path="/sock/orchestrator.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="/sock/adapter.sock"

[github]
project_owner="o"
project_number=1
[github.columns]
inbox="📥"
ready="📋"
design="🤖"
design_review="🚧"
impl_verify="🤖"
final_review="🚧"
awaiting_release="🚀"
released="🏁"

[github_watcher]
bind="127.0.0.1:7802"

[qa_service]
uds_path="/sock/qa-service.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="/sock/adapter.sock"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]
[notifications]
[retention]
[telemetry]
"#;

const SECRETS_TOML: &str = r#"
[postgres]
password = "op://Vault/Item/field"

[github_watcher]
github_token = "op://Vault/Item/field"
"#;

#[test]
fn op_refs_resolved_through_full_pipeline_and_cached() {
    let bin_dir = TempDir::new().unwrap();
    let calls_log = bin_dir.path().join("calls.log");
    let op_script = bin_dir.path().join("op");
    fs::write(
        &op_script,
        format!(
            "#!/bin/sh\necho \"$2\" >> {}\necho resolved-secret\n",
            calls_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&op_script, fs::Permissions::from_mode(0o755)).unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", bin_dir.path().display(), original_path),
    );

    let cfg_dir = TempDir::new().unwrap();
    let cfg_path = cfg_dir.path().join("config.toml");
    fs::write(&cfg_path, CONFIG_TOML).unwrap();
    fs::write(cfg_dir.path().join("secrets.toml"), SECRETS_TOML).unwrap();

    let result = Config::load(&cfg_path);

    std::env::set_var("PATH", original_path);

    let c = result.expect("load");
    assert_eq!(c.postgres.password.expose(), "resolved-secret");
    assert_eq!(c.github_watcher.github_token.expose(), "resolved-secret");

    // Same op:// URI ("op://Vault/Item/field") appears twice above but must
    // only invoke the fake `op` script once, due to the in-process cache.
    let calls = fs::read_to_string(&calls_log).unwrap_or_default();
    assert_eq!(
        calls.lines().count(),
        1,
        "expected exactly one op invocation, got: {calls:?}"
    );
}
```

- [ ] **Step 7: Run the test to verify it fails for the right reason before the fix, then passes**

Run: `cargo test -p totsuka-config --test load_secrets_op_resolve`
Expected: PASS (Steps 3-4 already implemented the resolution logic this test exercises — this step is verifying the integration point, not driving new implementation). If it fails, check: is `bin_dir.path()` prepended (not appended) to `PATH` so the fake `op` shadows any real `op` on the machine? Is the script executable (`0o755`)?

- [ ] **Step 8: Run the full crate test suite**

Run: `cargo test -p totsuka-config`
Expected: all tests pass — `expand.rs` unit tests, `op_resolve.rs` unit tests, `tests/load_secrets_merge.rs`, `tests/load_secrets_op_resolve.rs`.

- [ ] **Step 9: Run workspace-wide checks**

Run: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo fmt --check`
Expected: no warnings, no formatting diffs. (`crates/totsukactl/tests/init_template_roundtrip.rs` and `crates/totsuka-config` consumers elsewhere are unaffected by this task's signature changes since only `expand_toml_value`'s two call sites, both in `lib.rs`, exist — confirmed by the plan's pre-work grep.)

- [ ] **Step 10: Commit**

```bash
git add crates/totsuka-config/src/expand.rs crates/totsuka-config/src/lib.rs crates/totsuka-config/tests/load_secrets_op_resolve.rs
git commit -m "feat(totsuka-config): resolve op:// secret references during Config::load"
```

---

### Task 3: Document `op://` support in the secrets template and CLAUDE.md

**Files:**
- Modify: `crates/totsukactl/src/commands/templates/secrets.toml.tmpl`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: nothing from Task 1/2 code — this is a documentation-only task. No interfaces produced (terminal task).

- [ ] **Step 1: Add the `op://` example to the secrets template**

Replace the full current contents of `crates/totsukactl/src/commands/templates/secrets.toml.tmpl`:

```toml
# totsuka secrets — chmod 0600
# Each value is also overridable via env (e.g. POSTGRES_PASSWORD).
# Any value below may also be a 1Password Secret Reference instead of
# plaintext, e.g. github_token = "op://Dev/GitHub/token" — resolved via the
# `op` CLI at config-load time. Every totsuka binary loads config
# independently, so `op` must be authenticated in each binary's own process
# environment (e.g. via OP_SERVICE_ACCOUNT_TOKEN).
[postgres]
password = "postgres"

[github_watcher]
# github_token = "op://Dev/GitHub/token"
github_token = ""

[qa_service]
slack_app_token = ""
slack_bot_token = ""

[qa_service.classifier]
api_key = ""

[notifications.slack]
webhook_url = ""
```

- [ ] **Step 2: Run the template round-trip test to confirm the comment doesn't break parsing**

Run: `cargo test -p totsukactl --test init_template_roundtrip`
Expected: `init_template_loads_with_fully_resolved_paths` still passes — `github_token = ""` is still the active (uncommented) value, so `cfg.github_watcher.github_token.expose()` is still `""`.

- [ ] **Step 3: Update CLAUDE.md**

In `CLAUDE.md`, change the shared-libs bullet (currently):

```markdown
- **Shared libs**: `totsuka-core` (domain types), `totsuka-bus` (pgmq wrapper), `totsuka-config` (TOML schema + `${section.key}`/`~` expansion), `totsuka-telemetry` (tracing/healthz/readyz).
```

to:

```markdown
- **Shared libs**: `totsuka-core` (domain types), `totsuka-bus` (pgmq wrapper), `totsuka-config` (TOML schema + `${section.key}`/`~`/`op://` expansion), `totsuka-telemetry` (tracing/healthz/readyz).
```

Add one new bullet immediately after it:

```markdown
- `secrets.toml` values may be `op://vault/item/field` (1Password Secret Reference) instead of plaintext, resolved via the `op` CLI during `Config::load()`. Since every binary loads config independently at its own startup, each one needs `op` authenticated in its own process environment (e.g. `OP_SERVICE_ACCOUNT_TOKEN`) — there's no central process that resolves secrets once and distributes them.
```

- [ ] **Step 4: Run full workspace checks**

Run: `cargo test --workspace --all-features && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo fmt --check`
Expected: everything passes — this task changes no `.rs` files, only `.tmpl` and `.md`.

- [ ] **Step 5: Commit**

```bash
git add crates/totsukactl/src/commands/templates/secrets.toml.tmpl CLAUDE.md
git commit -m "docs: document op:// secret reference support in secrets.toml"
```
