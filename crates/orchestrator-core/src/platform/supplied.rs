//! Values the parent process supplies on stdin (`--secrets-stdin`, #754).
//!
//! A launcher that already holds every secret (the menu bar app) hands them
//! over as one JSON line instead of letting `totsuka` read the stores itself:
//! a GUI-launched process has no TTY for `op`, and an ad-hoc signed binary
//! re-prompts for every Keychain item on every build. Not the environment —
//! `ps -E` shows it in plaintext to the same user, and agents inherit it.
//!
//! Once [`install`]ed, these values are the **only** source the process
//! reads: [`PlatformSecretStore`](super::PlatformSecretStore) routes every
//! reference here, and [`SuppliedSecrets`] refuses the store-backed schemes
//! instead of falling through to them. That is what makes "a
//! `--secrets-stdin` process never touches Keychain or `op`" structural —
//! every resolver in the process goes through that one store, including the
//! ones inside `plugin_spec` that no call site passes a store to.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// The name → value map from `--secrets-stdin`.
#[derive(Debug, Default)]
pub struct SuppliedSecrets {
    values: HashMap<String, SecretString>,
}

/// Why a `--secrets-stdin` line was rejected. The messages never quote a
/// value — only names and positions.
#[derive(Debug, thiserror::Error)]
pub enum SuppliedError {
    /// Not JSON at all. The column, not the text: the text is the secrets.
    #[error("the line is not JSON (syntax error at column {column})")]
    NotJson {
        /// 1-based column serde_json stopped at.
        column: usize,
    },
    /// Valid JSON, but not an object.
    #[error("the line is not a JSON object → send {{\"<name>\": \"<value>\", …}}")]
    NotAnObject,
    /// A value that is not a string (a number, `null`, a nested object).
    #[error("the value of `{name}` is not a string")]
    NotAString {
        /// The offending key.
        name: String,
    },
}

impl SuppliedSecrets {
    /// Parse one `{"<name>": "<value>", …}` line.
    ///
    /// Decoded into a generic JSON value first rather than straight into
    /// `HashMap<String, String>`: serde's own type-mismatch message quotes the
    /// offending value (`invalid type: integer `5``), which here is a secret.
    pub fn from_json(line: &str) -> Result<Self, SuppliedError> {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| SuppliedError::NotJson { column: e.column() })?;
        let serde_json::Value::Object(map) = value else {
            return Err(SuppliedError::NotAnObject);
        };
        let mut values = HashMap::with_capacity(map.len());
        for (name, value) in map {
            let serde_json::Value::String(value) = value else {
                return Err(SuppliedError::NotAString { name });
            };
            values.insert(name, SecretString::new(value));
        }
        Ok(Self { values })
    }
}

/// `secret:<name>` from the map; every other scheme is refused without
/// touching its backend. Names the config does not use are simply never
/// asked for — a launcher may send everything it holds.
impl SecretStore for SuppliedSecrets {
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
        match reference {
            SecretRef::Supplied { name } => {
                self.values
                    .get(name)
                    .cloned()
                    .ok_or_else(|| SecretError::NotFound {
                        reference: reference.to_string(),
                    })
            }
            _ => Err(SecretError::StoreRefused {
                reference: reference.to_string(),
            }),
        }
    }
}

static INSTALLED: OnceLock<SuppliedSecrets> = OnceLock::new();

/// Make `secrets` the only source this process resolves references from.
///
/// Process-wide on purpose: the flag describes the process, and a value
/// threaded through every call site would leave any site that forgot it
/// free to open a store. Called once, by the CLI, before config is loaded.
///
/// # Panics
///
/// On a second call — the CLI reads stdin once.
pub fn install(secrets: SuppliedSecrets) {
    assert!(
        INSTALLED.set(secrets).is_ok(),
        "supplied secrets are installed once per process"
    );
}

/// The installed values, if this process was given any.
pub fn installed() -> Option<&'static SuppliedSecrets> {
    INSTALLED.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_supplied_names_and_ignores_the_rest() {
        let s = SuppliedSecrets::from_json(r#"{"github":"gho_x","unused":"y"}"#).unwrap();
        let v = s.get(&SecretRef::supplied("github")).unwrap();
        assert_eq!(v.expose(), "gho_x");
    }

    #[test]
    fn a_missing_name_is_not_found_and_says_which() {
        let s = SuppliedSecrets::from_json("{}").unwrap();
        let err = s.get(&SecretRef::supplied("github")).unwrap_err();
        assert!(matches!(err, SecretError::NotFound { .. }), "{err:?}");
        assert!(err.to_string().contains("secret:github"), "{err}");
    }

    /// The strict half: a store-backed reference is refused here, so the
    /// backend is never reached. There is no backend in this type to reach —
    /// which is the point; `PlatformSecretStore` delegates here wholesale.
    #[test]
    fn store_backed_schemes_are_refused() {
        let s = SuppliedSecrets::default();
        for reference in [
            SecretRef::keychain("totsuka", "hook-token"),
            SecretRef::onepassword("op://Dev/x/y"),
            SecretRef::command("echo leaked"),
            SecretRef::bitwarden("item", "password"),
        ] {
            let err = s.get(&reference).unwrap_err();
            assert!(matches!(err, SecretError::StoreRefused { .. }), "{err:?}");
            assert!(err.to_string().contains("secret:<name>"), "{err}");
        }
    }

    #[test]
    fn rejects_malformed_lines_without_quoting_values() {
        let cases = [
            ("not json at all", "not JSON"),
            (r#"["a"]"#, "not a JSON object"),
            (r#"{"n": 12345}"#, "`n` is not a string"),
            (r#"{"n": null}"#, "`n` is not a string"),
            (r#"{"n": {"x": "hunter2"}}"#, "`n` is not a string"),
        ];
        for (line, expected) in cases {
            let err = SuppliedSecrets::from_json(line).unwrap_err().to_string();
            assert!(err.contains(expected), "{line}: {err}");
            for leaked in ["12345", "hunter2", "at all"] {
                assert!(!err.contains(leaked), "{line}: {err}");
            }
        }
    }
}
