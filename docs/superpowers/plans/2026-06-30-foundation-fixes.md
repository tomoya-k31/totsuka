# Foundation Fixes (Smoke-Test Gaps) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the 5 foundational gaps surfaced during the 2026-06-29 totsukactl smoke test so that `totsukactl up` boots cleanly against the init-generated config template without manual `sed` edits or env-var workarounds (modulo external credentials).

**Architecture:** All fixes are scoped to existing crates — no new modules. Behavior changes happen in `totsuka-config::Config::load` (path tilde expansion, cross-section `${section.key}` variable expansion, `secrets.toml` merge) and the `orchestrator` / `totsukactl` bin entry points (DB URL secret discipline, drop-and-centralize `resolve_uds_path` duplicates). The `init` template is cleaned up to remove a stale `$DATE` literal. A new `Config::load` integration test pair exercises tilde-expanded + cross-section-referenced + secrets-merged configs against a temp HOME.

**Tech Stack:** Rust stable, `tokio` + `sqlx` + `serde` + `toml` + `tempfile` (already in workspace deps). No new dependencies.

## Global Constraints

- Rust workspace stable channel, `[profile.release] panic = "abort"`; lib crates expose error enums via `thiserror`, bins return `anyhow::Result<()>`.
- `#![forbid(unsafe_code)]` on every lib.rs.
- `tokio::task::block_in_place` is clippy-denied workspace-wide.
- `SystemTime::now()` / `chrono::Utc::now()` direct calls are clippy-denied — `Arc<dyn Clock>` for time.
- `Secret<String>` for tokens / passwords / webhook URLs; `.expose()` only at outbound HTTPS / DB URL construction sites; never log them.
- All Claude-driven commits use `git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "..."` (1Password Touch blocks background-signed commits).
- Schema version: `MIN_SCHEMA_VERSION = TARGET_SCHEMA_VERSION = 6` (no migrations in this plan).
- pgmq Postgres URL formula across bins: `postgres://{user}:{password.expose()}@{host}:{port}/{database}`; `DATABASE_URL` env override is the only acceptable alternative path.
- Spec §6 `secrets.toml` location: `~/.config/totsuka/secrets.toml` (chmod 0600). Per spec §6 the loader merges values from this file into the in-memory `Config`; env overrides (`TOTSUKA__*`) win over both files.
- All path-shaped string leaves in TOML (anything containing `/` and starting with `~/` or `${env:HOME}` or `${section.key}`) MUST resolve to absolute paths before any bin reads them — child bins NEVER see literal `${...}` or unexpanded `~/`.

---

### Task 1: Add `resolve_tilde` to `totsuka-config`

**Files:**
- Create: `crates/totsuka-config/src/path_expand.rs`
- Modify: `crates/totsuka-config/src/lib.rs` (`pub mod path_expand; pub use path_expand::resolve_tilde;`)
- Create: `crates/totsuka-config/tests/path_expand.rs`

**Interfaces:**
- Consumes: none.
- Produces:
  - `pub fn resolve_tilde(raw: &str, home: Option<&str>) -> String` — expands a leading `~/` (or bare `~`) using the supplied `home`. If `home` is `None` or `raw` doesn't start with a tilde token, returns `raw.to_string()` unchanged. Pure function, no env reads — caller passes `std::env::var("HOME").ok().as_deref()`.

- [ ] **Step 1: Write failing tests**

`crates/totsuka-config/tests/path_expand.rs`:
```rust
use totsuka_config::path_expand::resolve_tilde;

#[test]
fn expands_leading_tilde_slash_with_home() {
    assert_eq!(resolve_tilde("~/.config/x", Some("/home/u")), "/home/u/.config/x");
}

#[test]
fn expands_bare_tilde_with_home() {
    assert_eq!(resolve_tilde("~", Some("/home/u")), "/home/u");
}

#[test]
fn passes_through_when_home_unset() {
    assert_eq!(resolve_tilde("~/x", None), "~/x");
}

#[test]
fn passes_through_absolute_path() {
    assert_eq!(resolve_tilde("/abs/path", Some("/home/u")), "/abs/path");
}

#[test]
fn passes_through_relative_path_with_tilde_in_middle() {
    // "~foo" is some-other-user notation; we only handle "~/" and bare "~".
    assert_eq!(resolve_tilde("~foo/bar", Some("/home/u")), "~foo/bar");
    assert_eq!(resolve_tilde("dir/~/x", Some("/home/u")), "dir/~/x");
}

#[test]
fn passes_through_empty_string() {
    assert_eq!(resolve_tilde("", Some("/home/u")), "");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p totsuka-config --test path_expand
```
Expected: 6 tests fail to compile (`resolve_tilde` not in scope).

- [ ] **Step 3: Implement**

`crates/totsuka-config/src/path_expand.rs`:
```rust
//! Path-shaped string helpers. Pure functions only — callers supply env lookups.

/// Expand a leading `~/` (or bare `~`) using the supplied home directory.
/// Returns `raw` unchanged if `home` is `None`, if `raw` doesn't start with a
/// tilde token (`~/` or exactly `~`), or if `raw` is empty.
pub fn resolve_tilde(raw: &str, home: Option<&str>) -> String {
    let Some(home) = home else { return raw.to_string() };
    if raw == "~" {
        return home.to_string();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let mut out = String::with_capacity(home.len() + 1 + rest.len());
        out.push_str(home);
        out.push('/');
        out.push_str(rest);
        return out;
    }
    raw.to_string()
}
```

- [ ] **Step 4: Wire the module + re-export**

Modify `crates/totsuka-config/src/lib.rs` — add the module declaration alphabetically (after `expand`):
```rust
pub mod env_override;
pub mod expand;
pub mod path_expand;
pub mod schema;
pub mod validate;
pub use env_override::apply_env_overrides;
pub use expand::{expand_toml_value, expand_vars, ExpandError};
pub use path_expand::resolve_tilde;
pub use schema::Config;
pub use validate::ValidationError;
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p totsuka-config --test path_expand
```
Expected: 6/6 passing.

- [ ] **Step 6: Commit**

```bash
git add crates/totsuka-config/src/path_expand.rs crates/totsuka-config/src/lib.rs crates/totsuka-config/tests/path_expand.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsuka-config): resolve_tilde pure helper"
```

---

### Task 2: Expand cross-section `${section.key}` references

**Files:**
- Modify: `crates/totsuka-config/src/expand.rs:25-100` (the `expand_vars_lenient` family) — add a flat-key fallback lookup against the loaded tree.
- Modify: `crates/totsuka-config/src/lib.rs` — pass the flattened tree map into the expand step.
- Create: `crates/totsuka-config/tests/expand_cross_section.rs`

**Interfaces:**
- Consumes: `toml::Value` from `Config::load`'s parse step.
- Produces:
  - `pub fn flatten_string_leaves(tree: &toml::Value) -> std::collections::HashMap<String, String>` — walks the tree and produces a flat map `section.subsection.key → string-value` for every string leaf. Used as the lookup table for `${section.key}` refs during expansion.
  - `expand_vars_lenient` and `expand_toml_value` learn a second lookup source: if `${name}` is missing from `vars`, also try the flat map. Behavior unchanged when the key is present in neither (lenient → leave literal). `${env:NAME}` semantics unchanged.

- [ ] **Step 1: Write failing tests**

`crates/totsuka-config/tests/expand_cross_section.rs`:
```rust
use totsuka_config::Config;

const MIN_TOML_WITH_CROSS_REFS: &str = r#"
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
uds_path="${totsuka.state_dir}/sock/adapter.sock"
herdr_socket="/tmp/herdr.sock"
node_capacity=8
repos_root="/tmp/repos"
auto_clone=true

[orchestrator]
uds_path="${totsuka.state_dir}/sock/orchestrator.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="${agent_adapter.uds_path}"

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
uds_path="${totsuka.state_dir}/sock/qa-service.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="${agent_adapter.uds_path}"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]

[notifications]
[retention]
[telemetry]
"#;

#[test]
fn cross_section_state_dir_expands() {
    let c = Config::from_toml_str(MIN_TOML_WITH_CROSS_REFS).expect("parse");
    assert_eq!(c.agent_adapter.uds_path, "/var/state/sock/adapter.sock");
    assert_eq!(c.orchestrator.uds_path, "/var/state/sock/orchestrator.sock");
    assert_eq!(c.qa_service.uds_path, "/var/state/sock/qa-service.sock");
}

#[test]
fn cross_section_adapter_uds_expands_transitively() {
    let c = Config::from_toml_str(MIN_TOML_WITH_CROSS_REFS).expect("parse");
    // ${agent_adapter.uds_path} itself contains ${totsuka.state_dir}; both must resolve.
    assert_eq!(c.orchestrator.adapter_uds, "/var/state/sock/adapter.sock");
    assert_eq!(c.qa_service.adapter_uds, "/var/state/sock/adapter.sock");
}

#[test]
fn vars_table_still_works_alongside_cross_section() {
    let toml_with_both = MIN_TOML_WITH_CROSS_REFS.replace(
        "[agent_adapter]",
        "[vars]\nworkdir = \"/workspace\"\n\n[agent_adapter]",
    ).replace(
        r#"herdr_socket="/tmp/herdr.sock""#,
        r#"herdr_socket="${workdir}/herdr.sock""#,
    );
    let c = Config::from_toml_str(&toml_with_both).expect("parse");
    assert_eq!(c.agent_adapter.herdr_socket, "/workspace/herdr.sock");
    // Cross-section ref still works
    assert_eq!(c.agent_adapter.uds_path, "/var/state/sock/adapter.sock");
}

#[test]
fn undefined_cross_section_ref_left_literal_lenient() {
    let bad = MIN_TOML_WITH_CROSS_REFS.replace(
        r#"adapter_uds="${agent_adapter.uds_path}""#,
        r#"adapter_uds="${nope.missing}/x""#,
    );
    let c = Config::from_toml_str(&bad).expect("parse");
    // Lenient: undefined ${name} survives as literal (consistent with current vars-table behavior).
    assert_eq!(c.orchestrator.adapter_uds, "${nope.missing}/x");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p totsuka-config --test expand_cross_section
```
Expected: tests 1-3 FAIL (cross-section refs survive as literals); test 4 should already pass.

- [ ] **Step 3: Add `flatten_string_leaves` to expand.rs**

Append to `crates/totsuka-config/src/expand.rs`:
```rust
use std::collections::HashMap;

/// Flatten every string leaf in the TOML tree into a `section.subsection.key → value`
/// map suitable as a fallback lookup for `${section.key}` expansion.
pub fn flatten_string_leaves(tree: &toml::Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    walk_leaves(tree, &mut Vec::new(), &mut out);
    out
}

fn walk_leaves(value: &toml::Value, path: &mut Vec<String>, out: &mut HashMap<String, String>) {
    match value {
        toml::Value::String(s) => {
            if !path.is_empty() {
                out.insert(path.join("."), s.clone());
            }
        }
        toml::Value::Table(tbl) => {
            for (k, v) in tbl.iter() {
                path.push(k.clone());
                walk_leaves(v, path, out);
                path.pop();
            }
        }
        // Arrays and scalars are not exposed as ${section.key} refs.
        _ => {}
    }
}
```

- [ ] **Step 4: Teach the expand step to consult the flat map**

Modify `crates/totsuka-config/src/expand.rs` — change `expand_inner` (the body of `expand_vars` and `expand_vars_lenient`) so the lookup falls back to a second source. The simplest change: have `expand_toml_value` take a combined `vars` map (vars-table values + flat-map values) and keep the existing single-source `expand_inner` logic.

Edit `expand_toml_value` signature (already exists) to accept the merged map. The merging happens in `Config::load` (next step). The function body doesn't change shape — only its `vars` arg now contains both layers.

In `crates/totsuka-config/src/lib.rs::Config::load` (and `from_toml_str`), after the `take_vars_table` step add:
```rust
// 2b. Merge the flat map of string leaves into the lookup. Explicit [vars]
//     entries always win over an accidental same-named cross-section value.
let flat = crate::expand::flatten_string_leaves(&tree);
let mut merged: std::collections::HashMap<String, String> = flat;
for (k, v) in vars {
    merged.insert(k, v);
}
let vars = merged;
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p totsuka-config --test expand_cross_section
cargo test -p totsuka-config
```
Expected: 4/4 pass on the new file; all existing totsuka-config tests still pass.

- [ ] **Step 6: Run workspace tests to ensure no regression**

```bash
cargo test --workspace --all-features --locked
```
Expected: 285+ passing (current main count); no failures.

- [ ] **Step 7: Commit**

```bash
git add crates/totsuka-config/src/expand.rs crates/totsuka-config/src/lib.rs crates/totsuka-config/tests/expand_cross_section.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsuka-config): expand \${section.key} cross-section references via flat-tree fallback"
```

---

### Task 3: Tilde-expand on `Config::load` input path and every string leaf

**Files:**
- Modify: `crates/totsuka-config/src/lib.rs::Config::load` (the file-reading entry point) and `from_toml_str` (no path; only leaves).
- Modify: `crates/totsuka-config/src/expand.rs::expand_toml_value` — if a string leaf starts with `~/` or equals `~`, also expand it via `resolve_tilde` using `${env:HOME}`.
- Create: `crates/totsuka-config/tests/load_tilde.rs`

**Interfaces:**
- Consumes: `path_expand::resolve_tilde` (Task 1).
- Produces: `Config::load(path)` returns a `Config` whose every path-shaped string leaf has had its leading `~/` resolved via the live `HOME` env. The input path itself is also tilde-resolved before file read.

- [ ] **Step 1: Write failing tests**

`crates/totsuka-config/tests/load_tilde.rs`:
```rust
use std::fs;
use tempfile::TempDir;
use totsuka_config::Config;

const TOML_WITH_TILDES: &str = r#"
[totsuka]
state_dir = "~/.local/state/totsuka"
data_dir  = "~/.local/share/totsuka"

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
uds_path="~/sock/adapter.sock"
herdr_socket="~/.config/herdr/herdr.sock"
node_capacity=8
repos_root="~/work/repos"
auto_clone=true

[orchestrator]
uds_path="~/sock/orchestrator.sock"
wip_global=3
phase_timeout_default_secs=1800
retry_max=1
stuck_threshold_secs=600
adapter_uds="~/sock/adapter.sock"

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
uds_path="~/sock/qa-service.sock"
allowed_user_ids=["U1"]
catchup_channels=["C1"]
reaction_trigger="memo"
default_mode="delegated"
adapter_uds="~/sock/adapter.sock"

[qa_service.classifier]
provider="anthropic"
model="claude-haiku-4-5-20251001"

[qa_service.answer]
[notifications]
[retention]
[telemetry]
"#;

#[test]
fn from_toml_str_expands_tildes_using_live_home() {
    // HOME comes from the test process env.
    let home = std::env::var("HOME").expect("HOME unset in test env");
    let c = Config::from_toml_str(TOML_WITH_TILDES).expect("parse");
    assert_eq!(c.totsuka.state_dir, format!("{home}/.local/state/totsuka"));
    assert_eq!(c.totsuka.data_dir, format!("{home}/.local/share/totsuka"));
    assert_eq!(c.agent_adapter.uds_path, format!("{home}/sock/adapter.sock"));
    assert_eq!(c.agent_adapter.herdr_socket, format!("{home}/.config/herdr/herdr.sock"));
    assert_eq!(c.agent_adapter.repos_root, format!("{home}/work/repos"));
}

#[test]
fn load_expands_tilde_in_input_path() {
    // Write a config under a temp dir, then load via a tilde'd path.
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("config.toml");
    fs::write(&cfg_path, TOML_WITH_TILDES).unwrap();

    // Point HOME at the temp dir so "~/config.toml" resolves to cfg_path.
    let original = std::env::var("HOME").ok();
    std::env::set_var("HOME", tmp.path());

    // Move the file to the new HOME's root so "~/config.toml" finds it.
    let target = tmp.path().join("config.toml");
    if cfg_path != target {
        fs::copy(&cfg_path, &target).unwrap();
    }

    let c = Config::load("~/config.toml").expect("load via tilde'd path");
    assert_eq!(c.totsuka.state_dir, format!("{}/.local/state/totsuka", tmp.path().display()));

    match original {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
```

Note: HOME mutation is process-global — if other integration tests in this crate ever start touching HOME, add a `static ENV_LOCK: Mutex<()>` like `crates/totsukactl/tests/migrate_dryrun.rs`. For now, this test file is the only HOME mutator in totsuka-config, so a guard is unnecessary.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p totsuka-config --test load_tilde
```
Expected: 2/2 FAIL — `state_dir` is still literal `~/.local/state/totsuka`.

- [ ] **Step 3: Tilde-expand string leaves during the expand pass**

Modify `crates/totsuka-config/src/expand.rs::expand_toml_value` so each string leaf is also fed through `resolve_tilde` AFTER `expand_vars_lenient`. The HOME lookup uses the same `env_lookup` closure the caller already supplies, falling back to a direct `std::env::var("HOME")` read if not present (we keep the same closure signature).

Add this small wrapper at the top of `expand.rs`:
```rust
fn expand_string_leaf<F>(
    s: &str,
    vars: &HashMap<String, String>,
    env_lookup: &F,
) -> Result<String, ExpandError>
where
    F: Fn(&str) -> Option<String>,
{
    // First: var/env expansion (lenient — unknown ${name} survives as literal).
    let expanded = expand_vars_lenient(s, vars, env_lookup)?;
    // Then: leading-tilde expansion using HOME from the env_lookup.
    let home = env_lookup("HOME");
    Ok(crate::path_expand::resolve_tilde(&expanded, home.as_deref()))
}
```

Then in `expand_toml_value`, replace the `Value::String(s) => { *s = expand_vars_lenient(s, vars, env_lookup)?; }` line with:
```rust
toml::Value::String(s) => {
    *s = expand_string_leaf(s, vars, env_lookup)?;
}
```

- [ ] **Step 4: Tilde-expand the input path in `Config::load`**

In `crates/totsuka-config/src/lib.rs::Config::load`, replace the first line of the function body with:
```rust
pub fn load(path: impl AsRef<Path>) -> Result<Config, LoadError> {
    let raw_path = path.as_ref().to_string_lossy();
    let resolved = crate::path_expand::resolve_tilde(&raw_path, std::env::var("HOME").ok().as_deref());
    let raw = std::fs::read_to_string(&resolved)?;
    // ... rest unchanged
}
```

The rest of `load` is unchanged.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p totsuka-config --test load_tilde
cargo test -p totsuka-config
cargo test --workspace --all-features --locked
```
Expected: new tests 2/2 pass; existing totsuka-config tests still pass; workspace 287+ green.

- [ ] **Step 6: Commit**

```bash
git add crates/totsuka-config/src/expand.rs crates/totsuka-config/src/lib.rs crates/totsuka-config/tests/load_tilde.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsuka-config): tilde-expand input path and every string leaf via HOME"
```

---

### Task 4: Merge `secrets.toml` into `Config::load`

**Files:**
- Modify: `crates/totsuka-config/src/lib.rs::Config::load` — after reading the main config, look for a sibling `secrets.toml` (same directory as the resolved config path) and deep-merge it under the existing tree before env-override.
- Create: `crates/totsuka-config/tests/load_secrets_merge.rs`

**Interfaces:**
- Produces: `Config::load(path)` merges a sibling `secrets.toml` if it exists. Precedence (low → high): config.toml → secrets.toml → env. `from_toml_str` is unchanged (no file I/O).

- [ ] **Step 1: Write failing tests**

`crates/totsuka-config/tests/load_secrets_merge.rs`:
```rust
use std::fs;
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
password = "supersecret"

[github_watcher]
github_token = "ghp_abcdef"

[qa_service]
slack_app_token = "xapp-1"
slack_bot_token = "xoxb-1"

[qa_service.classifier]
api_key = "sk-ant-1"
"#;

fn write_pair(dir: &std::path::Path, secrets: Option<&str>) -> std::path::PathBuf {
    let cfg = dir.join("config.toml");
    fs::write(&cfg, CONFIG_TOML).unwrap();
    if let Some(s) = secrets {
        fs::write(dir.join("secrets.toml"), s).unwrap();
    }
    cfg
}

#[test]
fn secrets_toml_values_merged_into_config() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = write_pair(tmp.path(), Some(SECRETS_TOML));
    let c = Config::load(&cfg_path).expect("load");
    assert_eq!(c.postgres.password.expose(), "supersecret");
    assert_eq!(c.github_watcher.github_token.expose(), "ghp_abcdef");
    assert_eq!(c.qa_service.slack_app_token.expose(), "xapp-1");
    assert_eq!(c.qa_service.slack_bot_token.expose(), "xoxb-1");
    assert_eq!(c.qa_service.classifier.api_key.expose(), "sk-ant-1");
}

#[test]
fn secrets_toml_optional_loader_works_without_file() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = write_pair(tmp.path(), None);
    let c = Config::load(&cfg_path).expect("load");
    // Default Secret<String> is the empty string.
    assert_eq!(c.postgres.password.expose(), "");
}

#[test]
fn env_override_wins_over_secrets_toml() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = write_pair(tmp.path(), Some(SECRETS_TOML));
    std::env::set_var("TOTSUKA__POSTGRES__PASSWORD", "envwins");
    let c = Config::load(&cfg_path).expect("load");
    assert_eq!(c.postgres.password.expose(), "envwins");
    std::env::remove_var("TOTSUKA__POSTGRES__PASSWORD");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p totsuka-config --test load_secrets_merge
```
Expected: tests 1 + 3 FAIL — password is empty because secrets.toml isn't loaded; test 2 passes vacuously.

- [ ] **Step 3: Implement secrets.toml merge**

In `crates/totsuka-config/src/lib.rs::Config::load`, after `let raw = std::fs::read_to_string(&resolved)?;` and before the existing `let parsed: toml::Value = toml::from_str(&raw)?;` line, add the merge:

```rust
let parsed: toml::Value = toml::from_str(&raw)?;

// Optional sibling secrets.toml — merged BELOW env override but ABOVE config.toml.
let secrets_path = std::path::Path::new(&resolved)
    .parent()
    .map(|p| p.join("secrets.toml"));
let parsed = if let Some(p) = secrets_path.filter(|p| p.exists()) {
    let raw_sec = std::fs::read_to_string(&p)?;
    let parsed_sec: toml::Value = toml::from_str(&raw_sec)?;
    merge_toml(parsed, parsed_sec)
} else {
    parsed
};
```

Add `merge_toml` as a private helper at the bottom of `lib.rs`:
```rust
/// Recursive deep merge: keys present in `overlay` win over `base`. Tables are
/// merged element-wise; non-table values from `overlay` replace `base`'s.
fn merge_toml(base: toml::Value, overlay: toml::Value) -> toml::Value {
    use toml::Value;
    match (base, overlay) {
        (Value::Table(mut b), Value::Table(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(bv) => merge_toml(bv, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Table(b)
        }
        (_, overlay) => overlay,
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p totsuka-config --test load_secrets_merge
cargo test -p totsuka-config
cargo test --workspace --all-features --locked
```
Expected: 3/3 secrets_merge tests pass; existing tests still pass; workspace green.

- [ ] **Step 5: Commit**

```bash
git add crates/totsuka-config/src/lib.rs crates/totsuka-config/tests/load_secrets_merge.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "feat(totsuka-config): merge sibling secrets.toml under env-override precedence"
```

---

### Task 5: Fix orchestrator DB URL hardcode (`:totsuka@` → `password.expose()`)

**Files:**
- Modify: `crates/orchestrator/src/main.rs:32-42` — replace the hardcoded password literal with the same `build_db_url`-style formula every other bin uses.

**Interfaces:**
- Produces: `orchestrator/main.rs` reads `cfg.postgres.password.expose()` exactly once, at DB URL construction.

- [ ] **Step 1: Write failing test**

`crates/orchestrator/tests/db_url.rs`:
```rust
//! Guards against regressions in DB URL construction. The `:totsuka@` literal
//! must never reappear in main.rs.

#[test]
fn main_rs_has_no_hardcoded_totsuka_password() {
    let src = include_str!("../src/main.rs");
    assert!(
        !src.contains(":totsuka@"),
        "orchestrator main.rs has a hardcoded ':totsuka@' password — \
         this regresses spec §11.7 Secret discipline. Use config.postgres.password.expose() instead."
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p orchestrator --test db_url
```
Expected: FAIL with the assert message.

- [ ] **Step 3: Replace the hardcoded password in main.rs**

In `crates/orchestrator/src/main.rs`, replace the existing `let db_url = ...` block (around line 32):
```rust
let db_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
    format!(
        "postgres://{}:{}@{}:{}/{}",
        config.postgres.user,
        config.postgres.password.expose(),
        config.postgres.host,
        config.postgres.port,
        config.postgres.database,
    )
});
```

This matches the qa-service and github-watcher formulas exactly.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p orchestrator --test db_url
cargo test -p orchestrator
cargo test --workspace --all-features --locked
cargo clippy -p orchestrator --all-targets --all-features --locked -- -D warnings
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/orchestrator/src/main.rs crates/orchestrator/tests/db_url.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "fix(orchestrator): use config.postgres.password.expose() for DB URL (drop :totsuka@ hardcode)"
```

---

### Task 6: Centralize `resolve_uds_path` — drop the 3 duplicates

**Files:**
- Modify: `crates/orchestrator/src/listener.rs:40-47` — delete `resolve_uds_path` and update the 2 call sites in `crates/orchestrator/src/main.rs` to use `totsuka_config::resolve_tilde(&path, std::env::var("HOME").ok().as_deref())`.
- Modify: `crates/agent-adapter/src/listener.rs:46-53` — same treatment, update callers in `crates/agent-adapter/src/main.rs`.
- Modify: `crates/qa-service/src/listener.rs:48-55` — same, update callers in `crates/qa-service/src/main.rs`.

**Interfaces:**
- Consumes: `totsuka_config::resolve_tilde` (Task 1).
- Produces: no new public symbols. Behavior unchanged — paths post-config-load are already absolute (Task 3), so this call becomes a defensive pass-through. Keeping it explicit at the call site makes the contract visible and the test from Task 3 covers the expansion semantics.

Rationale for keeping the wrapper call rather than blindly trusting Task 3: a caller could one day instantiate `Config` via a non-`load` path (e.g., a test that builds the struct directly with hardcoded `~/...` strings). `resolve_tilde` is cheap and idempotent.

- [ ] **Step 1: Delete `resolve_uds_path` from `crates/orchestrator/src/listener.rs`**

Open the file; remove the 8-line `pub fn resolve_uds_path(raw: &str) -> PathBuf { ... }` block at the bottom and the now-unused `PathBuf` import if any.

- [ ] **Step 2: Update orchestrator/main.rs call sites**

`crates/orchestrator/src/main.rs`:
- Remove `resolve_uds_path` from the `listener::` import line.
- Add `use totsuka_config::resolve_tilde;` to the top-level imports.
- For each `resolve_uds_path(&x)` call (search the file with the Read tool), replace with:
  ```rust
  std::path::PathBuf::from(resolve_tilde(&x, std::env::var("HOME").ok().as_deref()))
  ```

- [ ] **Step 3: Run orchestrator tests**

```bash
cargo test -p orchestrator
cargo clippy -p orchestrator --all-targets --all-features --locked -- -D warnings
```
Expected: all green.

- [ ] **Step 4: Delete `resolve_uds_path` from `crates/agent-adapter/src/listener.rs`**

Same shape: delete the 8-line helper.

- [ ] **Step 5: Update agent-adapter/main.rs call sites**

`crates/agent-adapter/src/main.rs`:
- Remove `resolve_uds_path` from the `listener::` import.
- Add `use totsuka_config::resolve_tilde;`.
- Replace each `resolve_uds_path(&x)` with `std::path::PathBuf::from(resolve_tilde(&x, std::env::var("HOME").ok().as_deref()))`.

- [ ] **Step 6: Run agent-adapter tests**

```bash
cargo test -p agent-adapter
cargo clippy -p agent-adapter --all-targets --all-features --locked -- -D warnings
```

- [ ] **Step 7: Delete `resolve_uds_path` from `crates/qa-service/src/listener.rs`**

Same.

- [ ] **Step 8: Update qa-service/main.rs call sites**

`crates/qa-service/src/main.rs`:
- Remove `resolve_uds_path` from the `listener::` import.
- Add `use totsuka_config::resolve_tilde;`.
- Replace each `resolve_uds_path(&x)` with `std::path::PathBuf::from(resolve_tilde(&x, std::env::var("HOME").ok().as_deref()))`.

- [ ] **Step 9: Run workspace tests**

```bash
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```
Expected: all green.

- [ ] **Step 10: Commit**

```bash
git add crates/orchestrator/src/listener.rs crates/orchestrator/src/main.rs \
        crates/agent-adapter/src/listener.rs crates/agent-adapter/src/main.rs \
        crates/qa-service/src/listener.rs crates/qa-service/src/main.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "refactor(workspace): delete 3 resolve_uds_path duplicates; use totsuka_config::resolve_tilde"
```

---

### Task 7: Clean up the `init` template

**Files:**
- Modify: `crates/totsukactl/src/commands/templates/config.toml.tmpl` — drop the dead `$DATE` literal and add a top-of-file comment explaining that `${section.key}` cross-section refs now work and tildes auto-expand at load time.

**Interfaces:** none.

- [ ] **Step 1: Inspect the current template**

Read `crates/totsukactl/src/commands/templates/config.toml.tmpl`. The first two lines are:
```
# totsuka — generated by `totsukactl init` ($DATE)
# Edit before running `totsukactl up`. Secrets go in secrets.toml (chmod 0600).
```

- [ ] **Step 2: Replace the header comment**

Replace the first two lines verbatim with:
```
# totsuka — generated by `totsukactl init`.
# Edit before running `totsukactl up`. Secrets go in secrets.toml (chmod 0600).
# Paths may start with `~/` — they are tilde-expanded at config load time.
# Values may reference other keys via `${section.key}` (e.g. `${totsuka.state_dir}`).
```

- [ ] **Step 3: Verify the template still parses + the existing tests still pass**

```bash
cargo test -p totsukactl --test init_bootstrap
```
Expected: 3/3 pass (the existing `config_template_is_parseable` test re-validates the file as TOML).

- [ ] **Step 4: Commit**

```bash
git add crates/totsukactl/src/commands/templates/config.toml.tmpl
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "docs(totsukactl): drop \$DATE literal from init template; document path expansion"
```

---

### Task 8: Smoke harness — `Config::load` round-trip with the init template

**Files:**
- Create: `crates/totsukactl/tests/init_template_roundtrip.rs`

**Interfaces:**
- Produces: an integration test that writes the init template to a temp HOME, then `Config::load`s it, then asserts every path-shaped field is absolute and every `${...}` ref is resolved. This catches regressions in Tasks 1–4 from the template's POV.

- [ ] **Step 1: Write the test**

`crates/totsukactl/tests/init_template_roundtrip.rs`:
```rust
//! Round-trip: write the init template into a fake HOME, load it via
//! totsuka_config::Config::load, and verify the resulting Config has no
//! literal `~/` or `${...}` left in any path-shaped field.

use std::fs;
use std::sync::Mutex;
use tempfile::TempDir;
use totsuka_config::Config;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const TEMPLATE: &str = include_str!("../src/commands/templates/config.toml.tmpl");
const SECRETS_TEMPLATE: &str = include_str!("../src/commands/templates/secrets.toml.tmpl");

#[test]
fn init_template_loads_with_fully_resolved_paths() {
    let _lock = ENV_LOCK.lock().unwrap();

    let tmp = TempDir::new().unwrap();
    let cfg_dir = tmp.path().join(".config").join("totsuka");
    fs::create_dir_all(&cfg_dir).unwrap();
    fs::write(cfg_dir.join("config.toml"), TEMPLATE).unwrap();
    fs::write(cfg_dir.join("secrets.toml"), SECRETS_TEMPLATE).unwrap();

    let restore_home = std::env::var("HOME").ok();
    std::env::set_var("HOME", tmp.path());

    let cfg = Config::load(cfg_dir.join("config.toml")).expect("load template");

    // Tilde paths resolved.
    assert_eq!(
        cfg.totsuka.state_dir,
        format!("{}/.local/state/totsuka", tmp.path().display())
    );
    assert_eq!(
        cfg.totsuka.data_dir,
        format!("{}/.local/share/totsuka", tmp.path().display())
    );
    // Cross-section refs resolved.
    assert_eq!(
        cfg.agent_adapter.uds_path,
        format!("{}/.local/state/totsuka/sock/adapter.sock", tmp.path().display())
    );
    assert_eq!(
        cfg.orchestrator.uds_path,
        format!("{}/.local/state/totsuka/sock/orchestrator.sock", tmp.path().display())
    );
    assert_eq!(
        cfg.orchestrator.adapter_uds,
        format!("{}/.local/state/totsuka/sock/adapter.sock", tmp.path().display())
    );
    assert_eq!(
        cfg.qa_service.uds_path,
        format!("{}/.local/state/totsuka/sock/qa-service.sock", tmp.path().display())
    );
    assert_eq!(
        cfg.qa_service.adapter_uds,
        format!("{}/.local/state/totsuka/sock/adapter.sock", tmp.path().display())
    );
    // env: ref resolved.
    assert_eq!(
        cfg.agent_adapter.repos_root,
        format!("{}/work/repos", tmp.path().display())
    );
    // Secrets merged from the secrets template (default values are empty strings).
    assert_eq!(cfg.postgres.password.expose(), "postgres");
    assert_eq!(cfg.github_watcher.github_token.expose(), "");

    match restore_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p totsukactl --test init_template_roundtrip
```
Expected: 1/1 passing (only after Tasks 1–4 land; this task assumes those are merged).

- [ ] **Step 3: Run full workspace validation**

```bash
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```
Expected: all green.

- [ ] **Step 4: Commit**

```bash
git add crates/totsukactl/tests/init_template_roundtrip.rs
git -c commit.gpgsign=false -c tag.gpgsign=false commit -m "test(totsukactl): init template round-trip — load + resolve + secrets merge"
```

---

### Task 9: Push, PR, and merge

**Files:** none (housekeeping only).

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feat/foundation-fixes
```

- [ ] **Step 2: Create the PR**

```bash
gh pr create --title "fix: foundation gaps surfaced by totsukactl smoke test" --body "$(cat <<'EOF'
## Summary

Closes 5 foundation gaps surfaced during the 2026-06-29 totsukactl smoke test, where `totsukactl init` + `totsukactl up` against the default-generated config failed before reaching `Ready` for any child.

- **totsuka-config**:
  - `resolve_tilde` pure helper (single source of truth for `~/` expansion)
  - `${section.key}` cross-section variable expansion via flat-tree fallback (transitive)
  - Tilde expansion applied at `Config::load` to the input path AND every string leaf
  - Sibling `secrets.toml` merged into the loaded config under env-override precedence
- **orchestrator**: DB URL no longer hardcodes `:totsuka@`; uses `cfg.postgres.password.expose()` like every other bin (covered by a regression test pinning `:totsuka@` out of `main.rs`)
- **workspace**: deleted 3 copies of `resolve_uds_path` (orchestrator / agent-adapter / qa-service `listener.rs`); call sites now use `totsuka_config::resolve_tilde` directly
- **totsukactl**: dropped the dead `$DATE` literal from the init template; added an integration test that writes the template into a fake HOME and asserts every path-shaped field comes back absolute

## Test plan
- [x] `cargo test --workspace --all-features --locked` — 290+ passing
- [x] `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` — clean
- [x] `cargo fmt --all -- --check` — clean
- [ ] Manual smoke (post-merge): `totsukactl init` against an empty HOME → `totsukactl up` reaches Ready for `agent-adapter` + `orchestrator` without sed edits or env workarounds (watcher + qa still need real GitHub credentials to pass their readiness probes — out of scope here).
EOF
)"
```

- [ ] **Step 3: Wait for CI; merge fast-forward**

```bash
# After CI is 5/5 green:
gh pr merge --merge --delete-branch
git checkout main && git pull --ff-only
```

---

## Self-review notes (controller-side)

**Spec coverage:**
- §6 secrets.toml merge — Task 4 ✓
- §6 tilde-resolved paths — Tasks 1 + 3 ✓
- §6 `${section.key}` cross-section expansion — Task 2 ✓
- §11.7 Secret discipline (orchestrator regression) — Task 5 ✓
- §11.11 init template hygiene — Task 7 ✓
- Cross-bin redundancy cleanup — Task 6 (not strictly a spec item; just YAGNI)

**Type consistency:**
- `resolve_tilde(raw: &str, home: Option<&str>) -> String` — same signature throughout (Task 1 defines it; Tasks 3, 6, 8 consume it).
- `expand_string_leaf` (Task 3 internal helper) takes the same `vars` HashMap + `env_lookup` closure that `expand_toml_value` already passes around. No type drift.
- `merge_toml` (Task 4) is a local helper in `lib.rs`, returns `toml::Value`. Not exposed.
- `flatten_string_leaves` (Task 2) is public in `expand` module, returns `HashMap<String, String>` matching the same value type as the `[vars]` table.

**Path-shaped coverage:** Tasks 1+3 only tilde-expand string leaves. Numeric/boolean/array values are untouched (correct — they aren't paths). Arrays of strings (e.g., `qa_service.allowed_user_ids`) are walked element-wise by `expand_toml_value` (existing behavior); each string element gets tilde + var expansion. That's a benign no-op for IDs like `"U12345"`.

**Concurrency in tests:** The `ENV_LOCK` Mutex pattern from `migrate_dryrun.rs` is reused in `init_template_roundtrip.rs`. The `load_tilde.rs` and `load_secrets_merge.rs` tests in totsuka-config are the only HOME / TOTSUKA__POSTGRES__PASSWORD mutators in that crate; if more land later, retrofit the same Mutex pattern.

**Out of scope (deferred follow-ups):**
- Conventional env names (`POSTGRES_PASSWORD` instead of `TOTSUKA__POSTGRES__PASSWORD`) per spec §6 — a nice-to-have requiring a per-section env-name map; track as a follow-up issue.
- "Disable specific bins" config knob so smoke can run without GitHub/Slack tokens — design discussion, not a fix.
- agent-adapter `GET /v1/agents` HTTP route (the long-standing qa-service follow-up) — unrelated to these foundation gaps.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-30-foundation-fixes.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — fresh implementer + reviewer per task; matches the rhythm that delivered PRs #1–#7.

**2. Inline Execution** — batch with checkpoints (`superpowers:executing-plans`).

Which approach?
