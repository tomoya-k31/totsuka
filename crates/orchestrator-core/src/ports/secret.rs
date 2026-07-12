//! Secret handling port: [`SecretStore`], plus the [`SecretRef`] reference
//! type and the leak-resistant [`SecretString`] newtype (F-62, F-65, §5.2).
//!
//! The Orchestrator resolves secret references and hands *resolved* values to
//! plugins; plugins never get Keychain access themselves (F-65). This module
//! defines the boundary; concrete backends live in
//! [`platform`](crate::platform).

use std::fmt;
use std::str::FromStr;

/// Prefix identifying a Keychain-backed secret reference.
const KEYCHAIN_PREFIX: &str = "keychain:";

/// A secret value that never exposes itself through `Debug`/`Display`.
///
/// Wrapping secrets in this newtype prevents accidental leakage into logs or
/// error messages (§5.2 mandates unconditional redaction). Call
/// [`SecretString::expose`] at the exact point the raw value is needed.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    /// Wrap a raw secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the raw secret. Use only where the plaintext is genuinely
    /// required (e.g. building an `Authorization` header).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Redacted representation — the value is replaced with `***`.
impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretString").field(&"***").finish()
    }
}

/// Redacted representation — the value is replaced with `***`.
impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

/// A parsed reference to a secret held in the OS Keychain.
///
/// Textual form: `keychain:<service>/<account>`. The `<service>` segment runs
/// up to the first `/`; everything after it is the `<account>` (which may
/// itself contain `/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    service: String,
    account: String,
}

impl SecretRef {
    /// Build a reference from its two components.
    pub fn keychain(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            account: account.into(),
        }
    }

    /// The Keychain service (item name).
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The Keychain account (item account).
    pub fn account(&self) -> &str {
        &self.account
    }
}

impl FromStr for SecretRef {
    type Err = SecretError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix(KEYCHAIN_PREFIX)
            .ok_or_else(|| SecretError::InvalidReference(s.to_string()))?;
        let (service, account) = rest
            .split_once('/')
            .ok_or_else(|| SecretError::InvalidReference(s.to_string()))?;
        if service.is_empty() || account.is_empty() {
            return Err(SecretError::InvalidReference(s.to_string()));
        }
        Ok(Self::keychain(service, account))
    }
}

/// Errors from resolving a secret reference.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The reference string was not a well-formed `keychain:<service>/<account>`.
    #[error("invalid secret reference: {0}")]
    InvalidReference(String),
    /// No secret exists for the reference.
    #[error("secret not found: {service}/{account}")]
    NotFound { service: String, account: String },
    /// The underlying secret backend failed.
    #[error("secret backend error: {0}")]
    Backend(String),
    /// This platform has no supported secret store.
    #[error("secret store is not supported on this platform")]
    Unsupported,
}

/// Read-only access to OS-managed secrets (Keychain on macOS).
///
/// The trait is intentionally minimal; the Orchestrator only ever *reads*
/// secrets to resolve references before handing values to plugins (F-65).
pub trait SecretStore {
    /// Fetch the secret named by `reference`.
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_string_is_redacted() {
        let s = SecretString::new("super-secret-token");
        assert_eq!(format!("{s}"), "***");
        assert_eq!(format!("{s:?}"), "SecretString(\"***\")");
        // The value is still retrievable when explicitly exposed.
        assert_eq!(s.expose(), "super-secret-token");
        // And it must not appear in either formatting.
        assert!(!format!("{s} {s:?}").contains("super-secret-token"));
    }

    #[test]
    fn parses_keychain_reference() {
        let r: SecretRef = "keychain:totsuka/github-token".parse().unwrap();
        assert_eq!(r.service(), "totsuka");
        assert_eq!(r.account(), "github-token");
    }

    #[test]
    fn account_may_contain_slashes() {
        let r: SecretRef = "keychain:svc/a/b/c".parse().unwrap();
        assert_eq!(r.service(), "svc");
        assert_eq!(r.account(), "a/b/c");
    }

    #[test]
    fn rejects_malformed_references() {
        for bad in [
            "totsuka/token",
            "keychain:noslash",
            "keychain:/account",
            "keychain:svc/",
        ] {
            assert!(
                bad.parse::<SecretRef>().is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
