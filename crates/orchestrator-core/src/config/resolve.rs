//! Secret reference resolution and `${ENV}` / `~` expansion (F-62, F-65).
//!
//! Secrets are never stored in plaintext in config. A string value is either:
//!
//! - a `keychain:<service>/<account>` reference, resolved via the
//!   [`SecretStore`],
//! - an `op://<vault>/<item>/<field>` 1Password reference, resolved via the
//!   same store (the composite platform store routes by scheme), or
//! - an ordinary string containing `${VAR}` placeholders, expanded from the
//!   environment.
//!
//! Resolution happens **in the Orchestrator** (F-65); resolved values are then
//! handed to plugins. Errors follow the "cause + next action" convention (§7).

use std::path::PathBuf;

use crate::ports::{SecretRef, SecretStore, SecretString};

/// Prefix marking a Keychain-backed secret reference.
const KEYCHAIN_PREFIX: &str = "keychain:";

/// Prefix marking a 1Password secret reference (`op read` native URI).
const ONEPASSWORD_PREFIX: &str = "op://";

/// Errors from resolving/expanding a configuration value.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// A `${VAR}` referenced an unset environment variable.
    #[error(
        "environment variable `{0}` is not set → export it, or use a `keychain:<service>/<account>` / `op://<vault>/<item>/<field>` reference"
    )]
    EnvNotSet(String),
    /// A `${` placeholder was not closed with `}`. The offending value is
    /// deliberately omitted so a mistyped secret cannot leak into logs.
    #[error("unterminated `${{...}}` placeholder → close it with a `}}`")]
    UnterminatedPlaceholder,
    /// The underlying secret store failed (invalid ref, not found, backend).
    #[error(transparent)]
    Secret(#[from] crate::ports::SecretError),
}

/// Expand every `${VAR}` placeholder in `input` using `env`.
///
/// `env` mirrors [`std::env::var`] (returns `Some` when set). A `${VAR}`
/// whose variable is unset is an error. `$` not followed by `{` is literal.
pub fn expand_env<E>(input: &str, env: &E) -> Result<String, ResolveError>
where
    E: Fn(&str) -> Option<String>,
{
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("${") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 2..];
        let end = after
            .find('}')
            .ok_or(ResolveError::UnterminatedPlaceholder)?;
        let var = &after[..end];
        let value = env(var).ok_or_else(|| ResolveError::EnvNotSet(var.to_string()))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Expand a filesystem path: a leading `~` becomes `$HOME`, and `${VAR}`
/// placeholders are expanded (F-61 repo paths, F-22 worktree templates).
pub fn expand_path<E>(input: &str, env: &E) -> Result<PathBuf, ResolveError>
where
    E: Fn(&str) -> Option<String>,
{
    let expanded = expand_env(input, env)?;
    // Use PathBuf::join (not manual string concat) so separators stay
    // consistent with the rest of the crate (e.g. paths.rs).
    if let Some(rest) = expanded.strip_prefix("~/") {
        let home = env("HOME").ok_or_else(|| ResolveError::EnvNotSet("HOME".to_string()))?;
        Ok(PathBuf::from(home).join(rest))
    } else if expanded == "~" {
        let home = env("HOME").ok_or_else(|| ResolveError::EnvNotSet("HOME".to_string()))?;
        Ok(PathBuf::from(home))
    } else {
        Ok(PathBuf::from(expanded))
    }
}

/// Resolves secret references against a [`SecretStore`] and an environment.
pub struct SecretResolver<S, E> {
    store: S,
    env: E,
}

impl<S, E> SecretResolver<S, E>
where
    S: SecretStore,
    E: Fn(&str) -> Option<String>,
{
    /// Build a resolver from a secret store and an environment lookup.
    pub fn new(store: S, env: E) -> Self {
        Self { store, env }
    }

    /// Resolve one configuration value into a [`SecretString`].
    ///
    /// A `keychain:` or `op://` value is fetched from the store (which routes
    /// by scheme); anything else has its `${VAR}` placeholders expanded. The
    /// result is wrapped so it cannot leak via `Debug`/`Display` (§5.2).
    pub fn resolve(&self, value: &str) -> Result<SecretString, ResolveError> {
        if value.starts_with(KEYCHAIN_PREFIX) || value.starts_with(ONEPASSWORD_PREFIX) {
            let reference: SecretRef = value.parse()?;
            Ok(self.store.get(&reference)?)
        } else {
            Ok(SecretString::new(expand_env(value, &self.env)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{SecretError, SecretRef};
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    /// Fake store returning a fixed secret for one known reference per scheme.
    struct FakeStore;
    impl SecretStore for FakeStore {
        fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
            match reference {
                SecretRef::Keychain { service, account }
                    if service == "totsuka" && account == "gh" =>
                {
                    Ok(SecretString::new("ghp_secret"))
                }
                SecretRef::OnePassword { uri } if uri == "op://Dev/Openrouter/api_key" => {
                    Ok(SecretString::new("sk-or-from-op"))
                }
                _ => Err(SecretError::NotFound {
                    reference: reference.to_string(),
                }),
            }
        }
    }

    #[test]
    fn expands_env_placeholders() {
        let env = env_from(&[("XDG_RUNTIME_DIR", "/run/user/501")]);
        assert_eq!(
            expand_env("${XDG_RUNTIME_DIR}/herdr.sock", &env).unwrap(),
            "/run/user/501/herdr.sock"
        );
        assert_eq!(
            expand_env("no placeholders", &env).unwrap(),
            "no placeholders"
        );
    }

    #[test]
    fn missing_env_is_actionable_error() {
        let env = env_from(&[]);
        let err = expand_env("${NOTION_TOKEN}", &env).unwrap_err();
        assert!(matches!(err, ResolveError::EnvNotSet(ref v) if v == "NOTION_TOKEN"));
        // The message names the variable and the next action.
        let msg = err.to_string();
        assert!(msg.contains("NOTION_TOKEN"));
        assert!(msg.contains("keychain:"));
    }

    #[test]
    fn unterminated_placeholder_errors() {
        let env = env_from(&[]);
        let err = expand_env("${OPEN", &env).unwrap_err();
        assert!(matches!(err, ResolveError::UnterminatedPlaceholder));
        // The offending value must not appear in the message (secret safety).
        assert!(!err.to_string().contains("OPEN"));
    }

    #[test]
    fn expands_tilde_and_env_in_paths() {
        let env = env_from(&[("HOME", "/home/alice"), ("SUB", "proj")]);
        assert_eq!(
            expand_path("~/code/${SUB}", &env).unwrap(),
            PathBuf::from("/home/alice/code/proj")
        );
        // A bare `~` and absolute paths are handled too.
        assert_eq!(
            expand_path("~", &env).unwrap(),
            PathBuf::from("/home/alice")
        );
        assert_eq!(
            expand_path("/abs/path", &env).unwrap(),
            PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn resolves_keychain_reference_via_store() {
        let resolver = SecretResolver::new(FakeStore, env_from(&[]));
        let secret = resolver.resolve("keychain:totsuka/gh").unwrap();
        assert_eq!(secret.expose(), "ghp_secret");
    }

    #[test]
    fn resolves_env_reference() {
        let resolver = SecretResolver::new(FakeStore, env_from(&[("TOK", "abc123")]));
        let secret = resolver.resolve("${TOK}").unwrap();
        assert_eq!(secret.expose(), "abc123");
    }

    #[test]
    fn keychain_miss_surfaces_not_found() {
        let resolver = SecretResolver::new(FakeStore, env_from(&[]));
        let err = resolver.resolve("keychain:totsuka/missing").unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Secret(SecretError::NotFound { .. })
        ));
    }

    #[test]
    fn resolves_onepassword_reference_via_store() {
        // `op://` is the third reference scheme: routed to the store (which
        // dispatches on the SecretRef variant), never env-expanded.
        let resolver = SecretResolver::new(FakeStore, env_from(&[]));
        let secret = resolver.resolve("op://Dev/Openrouter/api_key").unwrap();
        assert_eq!(secret.expose(), "sk-or-from-op");
    }

    #[test]
    fn malformed_onepassword_reference_is_rejected() {
        // A recognized scheme with a bad shape is an error, not a passthrough
        // string (it would otherwise reach a plugin as a bogus "secret").
        let resolver = SecretResolver::new(FakeStore, env_from(&[]));
        let err = resolver.resolve("op://only-vault").unwrap_err();
        assert!(matches!(
            err,
            ResolveError::Secret(SecretError::InvalidReference(_))
        ));
    }
}
