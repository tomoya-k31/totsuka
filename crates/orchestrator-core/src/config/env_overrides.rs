//! `TOTSUKA_*` environment-variable overrides (F-66 layer 2).
//!
//! Effective values come from four layers, highest priority first:
//!
//! 1. CLI flags
//! 2. environment variables (`TOTSUKA_*`) — **this module**
//! 3. `plugins/{name}.toml` (plugin-specific file; opaque to the Orchestrator,
//!    §4.6 — not overridable from here)
//! 4. `config.toml` defaults
//!
//! Precedence is realized by *application order*, not by a merge engine:
//! [`apply_env_overrides`] mutates the parsed [`RootConfig`] right after
//! `config.toml` is read, and CLI flags are applied afterwards — so a CLI flag
//! always wins over an env var, which always wins over the file.
//!
//! The mapping is an explicit whitelist ([`OVERRIDES`]): each variable names
//! exactly one typed field. A generic "TOML overlay from env" was rejected
//! because `TOTSUKA_MAX_CONCURRENCY` cannot tell a word separator from a table
//! separator, and `RootConfig` is `deny_unknown_fields` (see ADR-0009).
//!
//! Handling is fail-loud: a whitelisted variable that does not convert aborts
//! startup, while an unrecognized `TOTSUKA_*` name only warns (it may belong to
//! another tool, and typos should be visible without being fatal).

use std::str::FromStr;

use crate::logging;

use super::schema::{ConfigError, RootConfig};

/// Prefix for environment variable overrides (F-66 layer 2).
pub const ENV_PREFIX: &str = "TOTSUKA_";

/// Reserved *outbound* env vars the Orchestrator injects into agent/hook
/// processes (`run::hooks`, `HookLaunchSpec`). These are never config
/// overrides; they are excluded from unknown-key warnings because an agent
/// session may re-invoke the `totsuka` CLI with them present.
const RESERVED: &[&str] = &[
    "TOTSUKA_JOB_ID",
    "TOTSUKA_HOOK_ENDPOINT",
    "TOTSUKA_HOOK_TOKEN",
    "TOTSUKA_HOOK_SPOOL_DIR",
    "TOTSUKA_PROMPT_CONTEXT",
];

/// Applies one variable's value to the config, or explains why it cannot.
type Applier = fn(&mut RootConfig, &str) -> Result<(), String>;

/// The whitelist: environment variable → target field (F-66 layer 2).
///
/// Selection criterion: scalars the Orchestrator itself interprets, which CI
/// and container runs want to swap without rewriting `config.toml`. Arrays and
/// dynamically keyed tables (`[[repositories]]`, `[[workflows]]`,
/// `[plugins.{name}]`) are deliberately out of scope — their keys are dynamic
/// and cannot be spelled in a static whitelist.
const OVERRIDES: &[(&str, Applier)] = &[
    ("TOTSUKA_MAX_CONCURRENCY", |cfg, v| {
        cfg.max_concurrency = Some(parse::<u32>(v, "a non-negative integer")?);
        Ok(())
    }),
    ("TOTSUKA_LOG_LEVEL", |cfg, v| {
        // Validated here (unlike the file path, which still falls back
        // silently) so a typo'd level in CI fails loudly.
        logging::parse_level(v)
            .ok_or_else(|| format!("expected one of error/warn/info/debug/trace, got `{v}`"))?;
        cfg.log.level = Some(v.to_string());
        Ok(())
    }),
    ("TOTSUKA_LOG_PROMPTS", |cfg, v| {
        cfg.log.log_prompts = parse::<bool>(v, "`true` or `false`")?;
        Ok(())
    }),
    ("TOTSUKA_LOG_MAX_FILES", |cfg, v| {
        cfg.log.max_files = Some(parse::<usize>(v, "a non-negative integer")?);
        Ok(())
    }),
    ("TOTSUKA_WORKTREE_LOCATION", |cfg, v| {
        // `~` / `${ENV}` expansion stays with the existing downstream path
        // handling (config::expand_path), so the raw template is stored.
        cfg.worktree.location = Some(v.to_string());
        Ok(())
    }),
    ("TOTSUKA_HOOKS_AUTH_TOKEN_REF", |cfg, v| {
        // A *secret reference* (`${ENV}` / `keychain:` / `op://`), not the
        // secret itself; resolution stays with SecretResolver (F-65).
        cfg.hooks.auth_token_ref = Some(v.to_string());
        Ok(())
    }),
    ("TOTSUKA_HOOKS_SOCKET_PATH", |cfg, v| {
        cfg.hooks.socket_path = Some(v.to_string());
        Ok(())
    }),
    ("TOTSUKA_HOOKS_SPOOL_DIR", |cfg, v| {
        cfg.hooks.spool_dir = Some(v.to_string());
        Ok(())
    }),
    ("TOTSUKA_HOOKS_BLOCK_RETRY_LIMIT", |cfg, v| {
        cfg.hooks.block_retry_limit = Some(parse::<u32>(v, "a non-negative integer")?);
        Ok(())
    }),
    ("TOTSUKA_LLM_BASE_URL", |cfg, v| {
        llm(cfg)?.base_url = v.to_string();
        Ok(())
    }),
    ("TOTSUKA_LLM_MODEL", |cfg, v| {
        llm(cfg)?.model = v.to_string();
        Ok(())
    }),
    ("TOTSUKA_LLM_MAX_TOKENS", |cfg, v| {
        llm(cfg)?.max_tokens = Some(parse::<u32>(v, "a non-negative integer")?);
        Ok(())
    }),
    ("TOTSUKA_LLM_TIMEOUT_SECS", |cfg, v| {
        llm(cfg)?.timeout_secs = Some(parse::<u64>(v, "a non-negative integer")?);
        Ok(())
    }),
    ("TOTSUKA_LLM_API_KEY_REF", |cfg, v| {
        llm(cfg)?.api_key_ref = Some(v.to_string());
        Ok(())
    }),
];

/// Every variable name the whitelist recognizes, in table order. Used by
/// `totsuka config show` to report which overrides are active.
pub fn override_keys() -> impl Iterator<Item = &'static str> {
    OVERRIDES.iter().map(|(name, _)| *name)
}

/// Apply `TOTSUKA_*` overrides to a parsed config (F-66 layer 2).
///
/// `vars` is a snapshot of the process environment; anything without the
/// [`ENV_PREFIX`] is ignored entirely. Returns human-readable warnings —
/// unknown names and empty values — which the caller prints to stderr (never
/// stdout: `--json` commands have a parseable-output contract).
///
/// Fails with [`ConfigError::EnvOverride`] when a whitelisted variable's value
/// does not convert, or when a `TOTSUKA_LLM_*` variable is set while
/// `config.toml` has no `[llm]` table.
pub fn apply_env_overrides<I>(cfg: &mut RootConfig, vars: I) -> Result<Vec<String>, ConfigError>
where
    I: IntoIterator<Item = (String, String)>,
{
    // Sorted so warnings and the first reported error do not depend on the
    // arbitrary iteration order of the environment.
    let mut vars: Vec<(String, String)> = vars
        .into_iter()
        .filter(|(name, _)| name.starts_with(ENV_PREFIX))
        .collect();
    vars.sort();

    let mut warnings = Vec::new();
    for (name, value) in vars {
        if RESERVED.contains(&name.as_str()) {
            continue;
        }
        let Some((_, apply)) = OVERRIDES.iter().find(|(key, _)| *key == name) else {
            warnings.push(format!(
                "unknown environment override {name} (ignored) → see the supported \
                 TOTSUKA_* list in the config docs"
            ));
            continue;
        };
        if value.is_empty() {
            // Follows the shell convention that an empty value means unset,
            // so `TOTSUKA_X=` in a CI matrix does not have to parse as a
            // number.
            warnings.push(format!("{name} is empty (ignored, treated as unset)"));
            continue;
        }
        apply(cfg, &value).map_err(|reason| ConfigError::EnvOverride { var: name, reason })?;
    }
    Ok(warnings)
}

/// Parse a value, naming the expected shape in the failure message.
fn parse<T: FromStr>(value: &str, expected: &str) -> Result<T, String> {
    value
        .parse::<T>()
        .map_err(|_| format!("expected {expected}, got `{value}`"))
}

/// The `[llm]` table, or an error. Never synthesized from env alone: `[llm]`
/// has required fields (`base_url`, `model`), so a partial table built from
/// whatever happens to be exported would be invalid. Erroring beats silently
/// dropping the variable — that silence is exactly what this module fixes.
fn llm(cfg: &mut RootConfig) -> Result<&mut super::schema::LlmConfig, String> {
    cfg.llm
        .as_mut()
        .ok_or_else(|| "config.toml has no [llm] table to override".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn apply(toml: &str, pairs: &[(&str, &str)]) -> Result<(RootConfig, Vec<String>), ConfigError> {
        let mut cfg = RootConfig::from_toml_str(toml).unwrap();
        let warnings = apply_env_overrides(&mut cfg, env(pairs))?;
        Ok((cfg, warnings))
    }

    const WITH_LLM: &str = r#"
[llm]
base_url = "https://file.example/v1"
model = "file-model"
"#;

    #[test]
    fn env_beats_the_config_file() {
        // F-66: layer 2 (env) > layer 4 (config.toml).
        let (cfg, warnings) = apply(
            r#"
max_concurrency = 4

[log]
level = "info"
log_prompts = true
max_files = 7

[worktree]
location = "/from/file/{branch}"

[hooks]
auth_token_ref = "keychain:file"
socket_path = "/from/file.sock"
spool_dir = "/from/file/spool"
block_retry_limit = 3
"#,
            &[
                ("TOTSUKA_MAX_CONCURRENCY", "5"),
                ("TOTSUKA_LOG_LEVEL", "debug"),
                ("TOTSUKA_LOG_PROMPTS", "false"),
                ("TOTSUKA_LOG_MAX_FILES", "14"),
                ("TOTSUKA_WORKTREE_LOCATION", "/from/env/{branch}"),
                ("TOTSUKA_HOOKS_AUTH_TOKEN_REF", "keychain:env"),
                ("TOTSUKA_HOOKS_SOCKET_PATH", "/from/env.sock"),
                ("TOTSUKA_HOOKS_SPOOL_DIR", "/from/env/spool"),
                ("TOTSUKA_HOOKS_BLOCK_RETRY_LIMIT", "9"),
            ],
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg.max_concurrency, Some(5));
        assert_eq!(cfg.log.level.as_deref(), Some("debug"));
        assert!(!cfg.log.log_prompts);
        assert_eq!(cfg.log.max_files, Some(14));
        assert_eq!(cfg.worktree.location.as_deref(), Some("/from/env/{branch}"));
        assert_eq!(cfg.hooks.auth_token_ref.as_deref(), Some("keychain:env"));
        assert_eq!(cfg.hooks.socket_path.as_deref(), Some("/from/env.sock"));
        assert_eq!(cfg.hooks.spool_dir.as_deref(), Some("/from/env/spool"));
        assert_eq!(cfg.hooks.block_retry_limit, Some(9));
    }

    #[test]
    fn llm_overrides_apply_when_the_table_exists() {
        let (cfg, warnings) = apply(
            WITH_LLM,
            &[
                ("TOTSUKA_LLM_BASE_URL", "https://env.example/v1"),
                ("TOTSUKA_LLM_MODEL", "env-model"),
                ("TOTSUKA_LLM_MAX_TOKENS", "512"),
                ("TOTSUKA_LLM_TIMEOUT_SECS", "30"),
                ("TOTSUKA_LLM_API_KEY_REF", "${ENV_KEY}"),
            ],
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let llm = cfg.llm.unwrap();
        assert_eq!(llm.base_url, "https://env.example/v1");
        assert_eq!(llm.model, "env-model");
        assert_eq!(llm.max_tokens, Some(512));
        assert_eq!(llm.timeout_secs, Some(30));
        assert_eq!(llm.api_key_ref.as_deref(), Some("${ENV_KEY}"));
    }

    #[test]
    fn llm_override_without_the_table_is_an_error() {
        // Not silently ignored: `[llm]` cannot be synthesized from env alone
        // because base_url/model are required.
        let err = apply("", &[("TOTSUKA_LLM_MODEL", "env-model")]).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("TOTSUKA_LLM_MODEL"), "{message}");
        assert!(message.contains("[llm]"), "{message}");
    }

    #[test]
    fn invalid_values_name_the_variable_and_the_expected_type() {
        for (var, value, expected) in [
            ("TOTSUKA_MAX_CONCURRENCY", "abc", "integer"),
            ("TOTSUKA_LOG_MAX_FILES", "-1", "integer"),
            ("TOTSUKA_LOG_PROMPTS", "yes", "`true` or `false`"),
            (
                "TOTSUKA_LOG_LEVEL",
                "verbose",
                "error/warn/info/debug/trace",
            ),
            ("TOTSUKA_HOOKS_BLOCK_RETRY_LIMIT", "many", "integer"),
        ] {
            let err = apply("", &[(var, value)]).unwrap_err();
            let message = err.to_string();
            assert!(matches!(err, ConfigError::EnvOverride { .. }), "{message}");
            assert!(message.contains(var), "{message}");
            assert!(message.contains(value), "{message}");
            assert!(message.contains(expected), "{message}");
        }
    }

    #[test]
    fn an_unknown_totsuka_var_warns_but_does_not_abort() {
        // A typo (MAX_CONCURENCY) must be visible, not silently dropped.
        let (cfg, warnings) = apply(
            "max_concurrency = 4",
            &[
                ("TOTSUKA_TYPO", "1"),
                ("TOTSUKA_MAX_CONCURENCY", "5"),
                ("PATH", "/usr/bin"),
                ("HOME", "/home/x"),
            ],
        )
        .unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("TOTSUKA_TYPO")));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("TOTSUKA_MAX_CONCURENCY"))
        );
        // Non-prefixed vars are not looked at at all.
        assert!(!warnings.iter().any(|w| w.contains("PATH")));
        assert_eq!(cfg.max_concurrency, Some(4), "config value is untouched");
    }

    #[test]
    fn reserved_hook_injection_vars_are_ignored_silently() {
        // An agent session re-invoking the CLI has these exported; they are a
        // different mechanism (outbound to hook scripts), not overrides.
        let (_, warnings) = apply(
            "",
            &[
                ("TOTSUKA_JOB_ID", "job-1-2"),
                ("TOTSUKA_HOOK_ENDPOINT", "http://localhost/events"),
                ("TOTSUKA_HOOK_TOKEN", "secret"),
                ("TOTSUKA_HOOK_SPOOL_DIR", "/spool"),
                ("TOTSUKA_PROMPT_CONTEXT", "instructions"),
            ],
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn the_singular_hook_spool_dir_is_reserved_not_an_override() {
        // One character apart: TOTSUKA_HOOK_SPOOL_DIR (injection) vs
        // TOTSUKA_HOOKS_SPOOL_DIR ([hooks].spool_dir override).
        let (cfg, warnings) = apply(
            "",
            &[
                ("TOTSUKA_HOOK_SPOOL_DIR", "/injected"),
                ("TOTSUKA_HOOKS_SPOOL_DIR", "/override"),
            ],
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg.hooks.spool_dir.as_deref(), Some("/override"));
    }

    #[test]
    fn an_empty_value_warns_and_leaves_the_config_value() {
        let (cfg, warnings) = apply(
            "max_concurrency = 4",
            &[("TOTSUKA_MAX_CONCURRENCY", ""), ("TOTSUKA_LOG_LEVEL", "")],
        )
        .unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().all(|w| w.contains("empty")));
        assert_eq!(cfg.max_concurrency, Some(4));
        assert!(cfg.log.level.is_none());
    }

    #[test]
    fn no_env_is_a_no_op() {
        let (cfg, warnings) = apply("max_concurrency = 4\n[log]\nlevel = \"warn\"", &[]).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(cfg.max_concurrency, Some(4));
        assert_eq!(cfg.log.level.as_deref(), Some("warn"));
    }

    #[test]
    fn override_keys_covers_the_whole_table_and_excludes_reserved_names() {
        let keys: Vec<&str> = override_keys().collect();
        assert_eq!(keys.len(), OVERRIDES.len());
        assert!(keys.iter().all(|k| k.starts_with(ENV_PREFIX)));
        assert!(keys.iter().all(|k| !RESERVED.contains(k)));
    }
}
