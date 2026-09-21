//! `[tools.<name>].env_file` (#744): a `KEY=value` file whose values reach the
//! agents that tool launches, through `ToolLaunchSpec.env`.
//!
//! Split in three so each caller takes only what it may do:
//!
//! - [`parse`] — the syntax, pure. A minimal dotenv subset: `KEY=value`,
//!   `#` comment lines, blank lines, one pair of surrounding `"…"` / `'…'`
//!   stripped (no escapes, no expansion). Anything else is refused **with its
//!   line number** rather than skipped: a silently dropped line is a variable
//!   that is just missing, with nothing to say why. The values never appear in
//!   an error — they are secrets or references to them.
//! - [`load_all`] — every `[tools]` entry's file read and checked, **nothing
//!   resolved**: what `totsuka doctor` runs, which stays non-interactive.
//! - [`resolve_tool_env`] — each value through the same
//!   [`SecretResolver`] as every other `*_ref`: `op://` / `keychain:` / `cmd:` /
//!   `bw:` from their store, anything else `${VAR}`-expanded. What
//!   `totsuka run` does once at startup.
//!
//! Names starting with `TOTSUKA_` are refused: the Orchestrator owns that
//! namespace, both for the variables it injects and for the ones an agent's
//! own `totsuka` would warn about as unknown overrides (ADR-0009).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::resolve::{ResolveError, SecretResolver, expand_path};
use super::schema::ToolConfig;
use crate::ports::{SecretRef, SecretStore, SecretString, is_secret_reference};

/// The namespace an `env_file` may not write into.
const RESERVED_PREFIX: &str = "TOTSUKA_";

/// Why an `env_file` could not be used. No variant carries a value.
#[derive(Debug, thiserror::Error)]
pub enum EnvFileError {
    /// The configured path did not expand (`${VAR}` unset, no `HOME` for `~`).
    #[error("[tools.{tool}].env_file: {source}")]
    Path {
        /// The `[tools]` entry.
        tool: String,
        /// Why it did not expand.
        source: ResolveError,
    },
    /// The expanded path is relative, so what it names would depend on the
    /// directory `totsuka` happens to run from.
    #[error(
        "[tools.{tool}].env_file must be an absolute path (after `~` / `${{VAR}}` expansion), got `{path}`"
    )]
    NotAbsolute {
        /// The `[tools]` entry.
        tool: String,
        /// The expanded path.
        path: String,
    },
    /// The file could not be read.
    #[error("cannot read env_file {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: std::io::Error,
    },
    /// A line outside the supported syntax, or a refused name.
    #[error("{}:{line}: {reason}", path.display())]
    Line {
        /// The file.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
        /// What is wrong with it.
        reason: String,
    },
    /// A value failed to resolve.
    #[error("{}: `{key}` did not resolve: {source}", path.display())]
    Resolve {
        /// The file.
        path: PathBuf,
        /// The variable whose value failed.
        key: String,
        /// Why.
        source: ResolveError,
    },
}

/// One `KEY=value` line, the value as written (quotes stripped, unresolved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 1-based line number, for later errors.
    pub line: usize,
    /// The variable name.
    pub key: String,
    /// The value or reference, unresolved.
    pub value: String,
}

/// Parse `text` (the contents of `path`, which only labels errors).
pub fn parse(path: &Path, text: &str) -> Result<Vec<Entry>, EnvFileError> {
    let mut entries: Vec<Entry> = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let fail = |reason: String| EnvFileError::Line {
            path: path.to_path_buf(),
            line,
            reason,
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("export ") {
            return Err(fail(
                "the `export` prefix is not supported → write `KEY=value`".into(),
            ));
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            return Err(fail("expected `KEY=value`".into()));
        };
        if !is_env_name(key) {
            return Err(fail(format!(
                "`{key}` is not a valid variable name ([A-Za-z_][A-Za-z0-9_]*)"
            )));
        }
        if key.starts_with(RESERVED_PREFIX) {
            return Err(fail(format!(
                "`{key}`: names starting with `{RESERVED_PREFIX}` are reserved for totsuka"
            )));
        }
        if let Some(first) = entries.iter().find(|e| e.key == key) {
            return Err(fail(format!(
                "`{key}` is already set on line {}",
                first.line
            )));
        }
        let value = unquote(value).map_err(fail)?;
        if value.contains("{{") {
            return Err(fail(
                "`op run` templates (`{{ … }}`) are not supported → write the reference as the \
                 whole value (`KEY=op://vault/item/field`)"
                    .into(),
            ));
        }
        entries.push(Entry {
            line,
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    Ok(entries)
}

/// Strip one pair of matching surrounding quotes. A value that opens a quote
/// it does not close is a multi-line value, which is not supported.
fn unquote(value: &str) -> Result<&str, String> {
    for quote in ['"', '\''] {
        if let Some(inner) = value.strip_prefix(quote) {
            return inner.strip_suffix(quote).ok_or_else(|| {
                format!("unterminated {quote} quote (multi-line values are not supported)")
            });
        }
    }
    Ok(value)
}

/// Whether `name` is a POSIX shell identifier (`[A-Za-z_][A-Za-z0-9_]*`).
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One `env_file` as every `[tools]` entry naming it will receive it.
#[derive(Debug, Clone)]
pub struct EnvFile {
    /// The expanded, absolute path.
    pub path: PathBuf,
    /// The `[tools]` entries that name this file.
    pub tools: Vec<String>,
    /// Its lines, checked but unresolved.
    pub entries: Vec<Entry>,
}

/// Read and check the `env_file` of every `[tools]` entry that has one,
/// resolving nothing. A file several tools name is read once. Each value
/// carrying a secret scheme must parse as that scheme's reference.
pub fn load_all<E>(
    tools: &BTreeMap<String, ToolConfig>,
    env: &E,
) -> Result<Vec<EnvFile>, EnvFileError>
where
    E: Fn(&str) -> Option<String>,
{
    let mut files: Vec<EnvFile> = Vec::new();
    for (tool, config) in tools {
        let Some(raw) = &config.env_file else {
            continue;
        };
        let path = expand_path(raw, env).map_err(|source| EnvFileError::Path {
            tool: tool.clone(),
            source,
        })?;
        if !path.is_absolute() {
            return Err(EnvFileError::NotAbsolute {
                tool: tool.clone(),
                path: path.display().to_string(),
            });
        }
        if let Some(file) = files.iter_mut().find(|f| f.path == path) {
            file.tools.push(tool.clone());
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|source| EnvFileError::Read {
            path: path.clone(),
            source,
        })?;
        let entries = parse(&path, &text)?;
        for entry in &entries {
            if is_secret_reference(&entry.value) && entry.value.parse::<SecretRef>().is_err() {
                return Err(EnvFileError::Line {
                    path: path.clone(),
                    line: entry.line,
                    reason: format!("`{}` is not a well-formed secret reference", entry.key),
                });
            }
        }
        files.push(EnvFile {
            path,
            tools: vec![tool.clone()],
            entries,
        });
    }
    Ok(files)
}

/// Resolve every `[tools]` entry's `env_file` into the env its agents launch
/// with, keyed by tool name. What `totsuka run` calls once at startup; any
/// failure stops it.
pub fn resolve_tool_env<S, E, R>(
    tools: &BTreeMap<String, ToolConfig>,
    env: &E,
    resolver: &SecretResolver<S, R>,
) -> Result<std::collections::HashMap<String, BTreeMap<String, SecretString>>, EnvFileError>
where
    S: SecretStore,
    E: Fn(&str) -> Option<String>,
    R: Fn(&str) -> Option<String>,
{
    let mut out = std::collections::HashMap::new();
    for file in load_all(tools, env)? {
        let resolved = file
            .entries
            .iter()
            .map(|entry| {
                resolver
                    .resolve(&entry.value)
                    .map(|value| (entry.key.clone(), value))
                    .map_err(|source| EnvFileError::Resolve {
                        path: file.path.clone(),
                        key: entry.key.clone(),
                        source,
                    })
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        for tool in file.tools {
            out.insert(tool, resolved.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::SecretError;
    use crate::tool::ToolKind;

    fn path() -> &'static Path {
        Path::new("/cfg/env.tpl")
    }

    fn line_error(text: &str) -> (usize, String) {
        match parse(path(), text) {
            Err(EnvFileError::Line { line, reason, .. }) => (line, reason),
            other => panic!("expected a line error, got {other:?}"),
        }
    }

    #[test]
    fn parses_the_supported_subset() {
        let text = "\
BRAVE_API_KEY=op://Dev/Brave/credential

# signing for claude
GIT_CONFIG_COUNT=1
QUOTED=\"a b\"
SINGLE='c=d'
EMPTY=
  INDENTED=x
";
        let entries = parse(path(), text).unwrap();
        let pairs: Vec<(&str, &str, usize)> = entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str(), e.line))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("BRAVE_API_KEY", "op://Dev/Brave/credential", 1),
                ("GIT_CONFIG_COUNT", "1", 4),
                ("QUOTED", "a b", 5),
                ("SINGLE", "c=d", 6),
                ("EMPTY", "", 7),
                ("INDENTED", "x", 8),
            ]
        );
    }

    #[test]
    fn refuses_what_it_does_not_support_by_line_without_the_value() {
        let cases = [
            ("A=1\nexport B=secret-value\n", 2, "`export`"),
            ("A=1\nnot a pair secret-value\n", 2, "KEY=value"),
            ("1A=secret-value\n", 1, "not a valid variable name"),
            ("A B=secret-value\n", 1, "not a valid variable name"),
            ("A=1\nA=secret-value\n", 2, "already set on line 1"),
            ("A=\"secret-value\n", 1, "unterminated"),
            ("A={{ op://v/i/secret-value }}\n", 1, "templates"),
            ("TOTSUKA_HOOK_TOKEN=secret-value\n", 1, "reserved"),
            ("TOTSUKA_ANYTHING=secret-value\n", 1, "reserved"),
        ];
        for (text, line, needle) in cases {
            let (got_line, reason) = line_error(text);
            assert_eq!(got_line, line, "{text:?}: {reason}");
            assert!(reason.contains(needle), "{text:?}: {reason}");
            let shown = EnvFileError::Line {
                path: path().to_path_buf(),
                line,
                reason,
            }
            .to_string();
            assert!(!shown.contains("secret-value"), "{shown}");
            assert!(shown.starts_with("/cfg/env.tpl:"), "{shown}");
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("totsuka-env-file-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tool(env_file: Option<&str>) -> ToolConfig {
        ToolConfig {
            kind: ToolKind::Claude,
            command: None,
            mode_args: None,
            plan_args: None,
            env_file: env_file.map(str::to_string),
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn load_all_reads_a_shared_file_once_and_expands_the_path() {
        let dir = scratch("shared");
        std::fs::write(dir.join("env.tpl"), "A=1\n").unwrap();
        let home = dir.display().to_string();
        let env = move |k: &str| (k == "HOME").then(|| home.clone());
        let tools = BTreeMap::from([
            ("claude-fast".to_string(), tool(Some("~/env.tpl"))),
            ("claude-opus".to_string(), tool(Some("~/env.tpl"))),
            ("codex".to_string(), tool(None)),
        ]);
        let files = load_all(&tools, &env).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, dir.join("env.tpl"));
        assert_eq!(files[0].tools, vec!["claude-fast", "claude-opus"]);
    }

    #[test]
    fn load_all_refuses_a_relative_path_a_missing_file_and_a_malformed_reference() {
        let tools = BTreeMap::from([("c".to_string(), tool(Some("env.tpl")))]);
        assert!(matches!(
            load_all(&tools, &no_env),
            Err(EnvFileError::NotAbsolute { .. })
        ));

        let dir = scratch("errors");
        let missing = dir.join("missing").display().to_string();
        let tools = BTreeMap::from([("c".to_string(), tool(Some(&missing)))]);
        assert!(matches!(
            load_all(&tools, &no_env),
            Err(EnvFileError::Read { .. })
        ));

        std::fs::write(dir.join("bad"), "A=1\nB=op://only-two/parts\n").unwrap();
        let bad = dir.join("bad").display().to_string();
        let tools = BTreeMap::from([("c".to_string(), tool(Some(&bad)))]);
        match load_all(&tools, &no_env) {
            Err(EnvFileError::Line {
                line: 2, reason, ..
            }) => {
                assert!(reason.contains("secret reference"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    /// Fake store: one known reference per scheme.
    struct FakeStore;
    impl SecretStore for FakeStore {
        fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
            match reference {
                SecretRef::OnePassword { uri } if uri == "op://Dev/Brave/credential" => {
                    Ok(SecretString::new("brave-key"))
                }
                SecretRef::Command { command } if command == "gh auth token" => {
                    Ok(SecretString::new("gho_token"))
                }
                _ => Err(SecretError::NotFound {
                    reference: "fake".into(),
                }),
            }
        }
    }

    #[test]
    fn resolve_tool_env_routes_every_scheme_and_expands_literals() {
        let dir = scratch("resolve");
        std::fs::write(
            dir.join("env.tpl"),
            "BRAVE=op://Dev/Brave/credential\nGH=cmd:gh auth token\nPLAIN=v-${USER}\n",
        )
        .unwrap();
        let file = dir.join("env.tpl").display().to_string();
        let env = |k: &str| (k == "USER").then(|| "alice".to_string());
        let tools = BTreeMap::from([
            ("a".to_string(), tool(Some(&file))),
            ("b".to_string(), tool(Some(&file))),
        ]);
        let resolver = SecretResolver::new(FakeStore, env);
        let out = resolve_tool_env(&tools, &env, &resolver).unwrap();
        let exposed: BTreeMap<&str, &str> = out["a"]
            .iter()
            .map(|(k, v)| (k.as_str(), v.expose()))
            .collect();
        assert_eq!(
            exposed,
            BTreeMap::from([
                ("BRAVE", "brave-key"),
                ("GH", "gho_token"),
                ("PLAIN", "v-alice")
            ])
        );
        assert_eq!(out["b"].len(), 3, "both tools get the file");
    }

    #[test]
    fn a_value_that_does_not_resolve_names_the_key() {
        let dir = scratch("unresolved");
        std::fs::write(dir.join("env.tpl"), "MISSING=op://Dev/Nope/field\n").unwrap();
        let file = dir.join("env.tpl").display().to_string();
        let tools = BTreeMap::from([("a".to_string(), tool(Some(&file)))]);
        let resolver = SecretResolver::new(FakeStore, no_env);
        let err = resolve_tool_env(&tools, &no_env, &resolver).unwrap_err();
        assert!(
            matches!(err, EnvFileError::Resolve { ref key, .. } if key == "MISSING"),
            "{err}"
        );
    }
}
