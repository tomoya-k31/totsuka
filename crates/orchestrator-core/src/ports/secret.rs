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

/// Prefix identifying a 1Password secret reference (`op read` native URI).
const ONEPASSWORD_PREFIX: &str = "op://";

/// Prefix identifying a command-backed secret reference (#444).
const COMMAND_PREFIX: &str = "cmd:";

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

/// A parsed reference to an externally-held secret.
///
/// Three schemes exist:
///
/// - `keychain:<service>/<account>` — the OS Keychain (macOS). The
///   `<service>` segment runs up to the first `/`; everything after it is the
///   `<account>` (which may itself contain `/`).
/// - `op://<vault>/<item>/<field>` — 1Password, resolved by shelling out to
///   `op read`. The URI is kept verbatim (`op read` accepts it natively);
///   parsing only requires the `vault/item/field` shape, existence is the
///   CLI's job.
/// - `cmd:<command>` — a shell command whose stdout is the secret (#444).
///   For credentials another tool already manages and rotates
///   (`cmd:gh auth token`): resolving re-runs the command, so no copy exists
///   to go stale. The `keychain:`-style prefix is deliberate — `op://`'s `//`
///   comes from `op`'s native URI, which has no counterpart here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRef {
    /// An OS-Keychain item (`keychain:<service>/<account>`).
    Keychain {
        /// The Keychain service (item name).
        service: String,
        /// The Keychain account (item account).
        account: String,
    },
    /// A 1Password item field (`op://<vault>/<item>/<field>`), kept verbatim.
    OnePassword {
        /// The full `op://…` URI as written in config.
        uri: String,
    },
    /// A shell command whose stdout is the secret (`cmd:<command>`).
    Command {
        /// The command string, run via `/bin/sh -c`.
        command: String,
    },
}

impl SecretRef {
    /// Build a Keychain reference from its two components.
    pub fn keychain(service: impl Into<String>, account: impl Into<String>) -> Self {
        Self::Keychain {
            service: service.into(),
            account: account.into(),
        }
    }

    /// Build a 1Password reference from its `op://…` URI.
    pub fn onepassword(uri: impl Into<String>) -> Self {
        Self::OnePassword { uri: uri.into() }
    }

    /// Build a command reference from its shell command string.
    pub fn command(command: impl Into<String>) -> Self {
        Self::Command {
            command: command.into(),
        }
    }
}

/// The textual form the reference was written in (`keychain:…` / `op://…` /
/// `cmd:…`). The reference names *where* a secret lives, never the secret
/// itself, so displaying it is safe (error messages, doctor output).
impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keychain { service, account } => {
                write!(f, "{KEYCHAIN_PREFIX}{service}/{account}")
            }
            Self::OnePassword { uri } => f.write_str(uri),
            Self::Command { command } => write!(f, "{COMMAND_PREFIX}{command}"),
        }
    }
}

impl FromStr for SecretRef {
    type Err = SecretError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix(KEYCHAIN_PREFIX) {
            let (service, account) = rest
                .split_once('/')
                .ok_or_else(|| SecretError::InvalidReference(s.to_string()))?;
            if service.is_empty() || account.is_empty() {
                return Err(SecretError::InvalidReference(s.to_string()));
            }
            return Ok(Self::keychain(service, account));
        }
        if let Some(rest) = s.strip_prefix(ONEPASSWORD_PREFIX) {
            // `op read` needs at least `vault/item/field`; deeper validation
            // (existence, extra segments like `?attribute=…`) is `op`'s job.
            let segments: Vec<&str> = rest.split('/').collect();
            if segments.len() < 3 || segments.iter().take(3).any(|s| s.is_empty()) {
                return Err(SecretError::InvalidReference(s.to_string()));
            }
            return Ok(Self::onepassword(s));
        }
        if let Some(rest) = s.strip_prefix(COMMAND_PREFIX) {
            // Anything after the prefix is the command, verbatim. Only an
            // empty/blank command is rejected — the command's own validity is
            // the shell's job at resolve time.
            if rest.trim().is_empty() {
                return Err(SecretError::InvalidReference(s.to_string()));
            }
            return Ok(Self::command(rest));
        }
        Err(SecretError::InvalidReference(s.to_string()))
    }
}

/// Errors from resolving a secret reference.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The reference string was not a well-formed `keychain:<service>/<account>`,
    /// `op://<vault>/<item>/<field>`, or `cmd:<command>`.
    #[error("invalid secret reference: {0}")]
    InvalidReference(String),
    /// No secret exists for the reference.
    #[error("secret not found: {reference}")]
    NotFound { reference: String },
    /// The underlying secret backend failed.
    #[error("secret backend error: {0}")]
    Backend(String),
    /// The backend tool for this reference scheme is not installed.
    #[error(
        "secret backend `{backend}` is not available → install it (macOS: `brew install 1password-cli`, other platforms: https://developer.1password.com/docs/cli)"
    )]
    BackendUnavailable { backend: String },
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
        assert_eq!(r, SecretRef::keychain("totsuka", "github-token"));
        assert_eq!(r.to_string(), "keychain:totsuka/github-token");
    }

    #[test]
    fn account_may_contain_slashes() {
        let r: SecretRef = "keychain:svc/a/b/c".parse().unwrap();
        assert_eq!(r, SecretRef::keychain("svc", "a/b/c"));
    }

    #[test]
    fn parses_onepassword_reference_verbatim() {
        // The `op read` native URI is kept whole — `op` interprets it.
        let r: SecretRef = "op://Dev/Openrouter/api_key".parse().unwrap();
        assert_eq!(r, SecretRef::onepassword("op://Dev/Openrouter/api_key"));
        assert_eq!(r.to_string(), "op://Dev/Openrouter/api_key");
        // Extra segments (e.g. a section) stay the CLI's business.
        assert!("op://Dev/Item/section/field".parse::<SecretRef>().is_ok());
    }

    #[test]
    fn parses_command_reference_verbatim() {
        let r: SecretRef = "cmd:gh auth token".parse().unwrap();
        assert_eq!(r, SecretRef::command("gh auth token"));
        assert_eq!(r.to_string(), "cmd:gh auth token");
        // Shell syntax rides through untouched — validity is the shell's job.
        assert!(
            "cmd:op read 'op://Dev/X/y' | tr -d '\\n'"
                .parse::<SecretRef>()
                .is_ok()
        );
    }

    #[test]
    fn rejects_malformed_references() {
        for bad in [
            "totsuka/token",
            "keychain:noslash",
            "keychain:/account",
            "keychain:svc/",
            // op:// needs at least vault/item/field, all non-empty.
            "op://",
            "op://only-vault",
            "op://Dev/item-only",
            "op://Dev//field",
            // cmd: needs a non-blank command.
            "cmd:",
            "cmd:   ",
        ] {
            assert!(
                bad.parse::<SecretRef>().is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
