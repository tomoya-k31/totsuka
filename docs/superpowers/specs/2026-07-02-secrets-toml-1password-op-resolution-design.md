# `secrets.toml` 1Password (`op://`) Resolution — Design

## Problem

`secrets.toml` currently only holds plaintext secret values (or `${env:NAME}` references resolved from the process environment). The user wants to store 1Password Secret References instead — the same `op://vault/item/field` convention already used in another app's `.env` file (e.g. `BRAVE_API_KEY=op://Dev/Brave/api_key`) — so no plaintext secret ever needs to sit in `~/.config/totsuka/secrets.toml` on disk.

Each of totsuka's five binaries (`agent-adapter`, `orchestrator`, `github-watcher`, `qa-service`, `totsukactl`) independently calls `totsuka_config::Config::load()` at its own startup — there is no central process that loads config once and distributes it to children. This is an existing, confirmed architectural fact, not something this feature changes.

## Goals

- A string value in `secrets.toml` (or, incidentally, `config.toml`, since both files feed the same merged tree) of the form `op://Vault/Item/field` is resolved to the actual secret value by invoking the 1Password CLI (`op read op://Vault/Item/field`) during `Config::load()`.
- Resolution failure (op CLI missing, non-zero exit, non-UTF8 output) is a hard error that aborts config loading — never a silent empty/default secret.
- A repeated `op://` reference within one `Config::load()` call is only resolved once (in-process cache), to avoid redundant `op` process spawns.
- Auth is out of scope for this feature to manage: the `op` CLI is expected to already be authenticated in whatever way the calling process's environment provides (this repo's chosen approach: `OP_SERVICE_ACCOUNT_TOKEN` in the environment of every binary that loads config — decided by the user, not re-litigated here).

## Non-goals

- Batching multiple `op://` lookups into a single `op inject` call. Considered and explicitly deferred — with a handful of secret fields per binary, the subprocess-spawn overhead of one `op read` call per field at process startup (not a hot path) is not worth the added machinery of template-building and result-splicing.
- Centralizing resolution in `totsukactl` and distributing resolved config to child processes. Rejected during design discussion — it would require a new IPC/temp-file mechanism and break the existing "every binary independently loads its own config" architecture for no benefit proportional to the cost.
- Managing `op` CLI sign-in/session lifecycle from within totsuka. `Config::load()` only ever invokes `op read`; if that fails because of an auth problem, it surfaces as a normal resolution failure.

## Architecture

### Where op:// resolution runs in the pipeline

`Config::load()` (`crates/totsuka-config/src/lib.rs:37-81`) already has this pipeline:

```
read config.toml → parse
  → merge sibling secrets.toml (if present)     [secrets.toml wins over config.toml]
  → apply TOTSUKA__SECTION__KEY env overrides   [env wins over everything]
  → strip [vars] table into a lookup map
  → expand_toml_value(): walk tree, rewrite ${name} / ${env:NAME} / ~ in string leaves
  → tree.try_into::<Config>()
  → cfg.validate()
```

`op://` resolution slots into the existing `expand_toml_value()` walk (`crates/totsuka-config/src/expand.rs`), as a new leaf rule checked *before* `${...}` expansion: if a string leaf's entire value starts with `op://`, it is resolved via an injected `op_lookup` closure and returned as-is (no further `${...}`/`~` expansion is applied to the resolved secret — the CLI's output is treated as a final literal).

Running this inside the existing `expand_toml_value` pass — which already executes *after* env overrides are applied — means a field overridden via `TOTSUKA__POSTGRES__PASSWORD=...` skips the `op` call entirely for that field, since the override has already replaced the `op://...` string before this pass runs.

### Module changes

**`crates/totsuka-config/src/expand.rs`:**
- `expand_string_leaf` gains a new parameter `op_lookup: &O where O: Fn(&str) -> Result<String, ExpandError>`. If `s.starts_with("op://")`, return `op_lookup(s)` immediately, bypassing `expand_vars_lenient`/tilde expansion.
- `expand_toml_value` gains the same `op_lookup` parameter, threaded through its recursive calls (mirrors how `env_lookup` is already threaded).
- `ExpandError` gains three variants:
  ```rust
  #[error("op CLI could not be executed for {0}: {1}")]
  OpExec(String, String),
  #[error("op read failed for {0}: {1}")]
  OpFailed(String, String),
  #[error("op read returned non-UTF8 output for {0}")]
  OpNonUtf8(String),
  ```
  The `{0}` in each is the `op://...` URI (vault/item/field path — never resolved secret material).

**New `crates/totsuka-config/src/op_resolve.rs`:**
```rust
/// Testable seam: the binary to invoke is a parameter, not hardcoded, so
/// tests can point it at a fake script instead of manipulating PATH/env.
pub fn resolve_with(bin: &str, uri: &str) -> Result<String, ExpandError> { ... }

/// Production entry point — always invokes the real `op` CLI.
pub fn resolve(uri: &str) -> Result<String, ExpandError> {
    resolve_with("op", uri)
}
```
`resolve_with` runs `std::process::Command::new(bin).args(["read", uri]).output()`:
- `Command::output()` returning `Err` (binary not found, exec failure) → `ExpandError::OpExec(uri, err.to_string())`.
- Non-zero exit status → `ExpandError::OpFailed(uri, stderr trimmed to a string)`.
- `String::from_utf8` failure on stdout → `ExpandError::OpNonUtf8(uri)`.
- Success: strip exactly one trailing `\n` (and a preceding `\r` if present) from stdout — matches typical CLI line-output convention — and return the rest verbatim (no broader `.trim()`, to avoid stripping meaningful whitespace from a multiline secret).

**`crates/totsuka-config/src/lib.rs`:**
- `Config::load` (and `from_toml_str`, for parity) builds a `RefCell<HashMap<String, String>>` cache and an `op_lookup` closure before calling `expand_toml_value`:
  ```rust
  let op_cache = std::cell::RefCell::new(std::collections::HashMap::<String, String>::new());
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
- `LoadError::Expand(#[from] ExpandError)` already exists and needs no change — the new `ExpandError` variants surface through it automatically.

## Testing

- **`expand.rs` unit tests** (no subprocess, no filesystem): pass a fake `op_lookup` closure directly (e.g. `|uri: &str| Ok(format!("resolved:{uri}"))`, or one that returns an error) to `expand_string_leaf`/`expand_toml_value`. Covers: successful resolution, error propagation, that `${...}` inside an `op://...` string is *not* expanded (the whole value is treated as an opaque URI), and cache behavior is exercised at the `lib.rs` integration level (below) rather than here, since the cache lives in the closure built by `Config::load`, not in `expand.rs` itself.
- **`op_resolve.rs` unit tests**: use `tempfile` (already a dev-dependency of `totsuka-config`) to write a fake `op` shell script to a `TempDir` at test-run time — one script body that echoes a fixed string and exits 0 (success case), another that writes to stderr and exits 1 (failure case). Tests call `resolve_with("<tempdir-path>/op", "op://Vault/Item/field")` directly — no `PATH` mutation, no shared-process-env races, safe under parallel `cargo test`.
- **`totsuka-config` integration test** (new file `tests/load_secrets_op_resolve.rs`, mirrors `tests/load_secrets_merge.rs`): a `secrets.toml` fixture containing an `op://...` value, loaded through the full `Config::load()` pipeline. Since `Config::load()`'s production path always calls `op_resolve::resolve` (hardcoded `"op"`), this test writes the same kind of fake script to a `TempDir` via `tempfile`, names it exactly `op`, and prepends that `TempDir`'s path to `PATH` for the duration of this one test (`std::env::set_var("PATH", ...)`, restored via a guard/drop at the end of the test). This is the only test in the suite that touches `PATH`. Since each file under `tests/` compiles to its own test binary (its own OS process), mutating `PATH` here cannot race `load_secrets_merge.rs` or the `op_resolve.rs` unit tests regardless of parallel execution — but *within* `tests/load_secrets_op_resolve.rs` itself, `PATH` mutation would race a second `#[test]` function in the same file if one were added later (same-process threads share env). This file must contain exactly one `#[test]` function for that reason; if a second `op`-resolution scenario needs its own test later, give it its own new file rather than adding a second test function here.
- No real `op` CLI or live 1Password account is exercised anywhere in the test suite or CI.

## Docs & templates

- `crates/totsukactl/src/commands/templates/secrets.toml.tmpl`: add a comment under one or two fields showing the `op://` form as an alternative to the plaintext default, e.g.:
  ```toml
  [github_watcher]
  # Either a plaintext value, or a 1Password Secret Reference:
  # github_token = "op://Dev/GitHub/token"
  github_token = ""
  ```
- `CLAUDE.md`: one line under the config-loading description noting that `secrets.toml` values may be `op://vault/item/field` references, resolved via the `op` CLI at load time, and that every binary loading config needs `OP_SERVICE_ACCOUNT_TOKEN` (or equivalent `op` auth) in its own process environment.

## Global constraints

- No new crate dependency — `op` is invoked as an external binary via `std::process::Command`, consistent with the existing `tokio::process::Command` usage for `docker` in `crates/totsukactl/src/compose.rs` (that one is async; this one stays sync since `Config::load` is sync end-to-end today).
- Fail-closed: any `op read` failure aborts `Config::load()` via `LoadError::Expand`. No fallback to a default/empty secret.
- Error messages must never include resolved secret material — only the `op://` URI itself (which contains no secret value, only vault/item/field names).
- `#![forbid(unsafe_code)]` (already enforced crate-wide) — no unsafe needed for this feature regardless.
