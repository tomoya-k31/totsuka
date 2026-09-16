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

/// Prefix identifying a Bitwarden secret reference (#699).
const BITWARDEN_PREFIX: &str = "bw:";

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
/// Four schemes exist:
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
/// - `bw:<item>/<field>` — Bitwarden, resolved by shelling out to
///   `bw get <field> <item>` (#699). The `//` is dropped for the same reason
///   as `cmd:`: Bitwarden has **no native `bw://` URI**, so writing one would
///   be inventing a URI that does not exist. `<field>` is a `bw get` object
///   name, so the reference reads the way the official CLI is invoked; the
///   split is at the **last** `/` because an item name may contain `/`
///   (`github.com/myorg`) while the object vocabulary never does — the
///   opposite of `keychain:`, whose account is the part that may contain `/`.
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
    /// A Bitwarden item field (`bw:<item>/<field>`).
    Bitwarden {
        /// The item, as `bw get` takes it: an item id or a search string.
        item: String,
        /// The `bw get` object name (`password`, `username`, `totp`, …).
        field: String,
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

    /// Build a Bitwarden reference from its item and field.
    pub fn bitwarden(item: impl Into<String>, field: impl Into<String>) -> Self {
        Self::Bitwarden {
            item: item.into(),
            field: field.into(),
        }
    }
}

/// The textual form the reference was written in (`keychain:…` / `op://…` /
/// `cmd:…` / `bw:…`). The reference names *where* a secret lives, never the secret
/// itself, so displaying it is safe (error messages, doctor output).
///
/// For `cmd:` that safety is a rule, not a construction: the command string
/// is config text, and the standing rule that no plaintext secret goes into
/// config applies to it — `cmd:curl -H "Bearer xoxp-…"` violates it exactly
/// the way `token = "xoxp-…"` does. Fetch inline credentials via the command
/// itself (that is the scheme's whole point), never paste them into it.
impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keychain { service, account } => {
                write!(f, "{KEYCHAIN_PREFIX}{service}/{account}")
            }
            Self::OnePassword { uri } => f.write_str(uri),
            Self::Command { command } => write!(f, "{COMMAND_PREFIX}{command}"),
            Self::Bitwarden { item, field } => {
                write!(f, "{BITWARDEN_PREFIX}{item}/{field}")
            }
        }
    }
}

impl FromStr for SecretRef {
    type Err = SecretError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_scheme(s).unwrap_or_else(|| Err(SecretError::InvalidReference(s.to_string())))
    }
}

/// Parse `s` when it carries a known scheme prefix.
///
/// `None` means **no scheme matched** — the value is an `${ENV}` string or a
/// plain one, and belongs to the resolver's other branch. `Some(Err(_))` means
/// a scheme matched but its body was malformed, which must stay an error
/// rather than being silently treated as a plain string.
///
/// This is the **only** list of scheme prefixes. The resolver used to keep its
/// own copy of it, so adding a scheme meant updating two places and nothing
/// caught the omission (#699); [`is_secret_reference`] now derives the answer
/// from this function.
fn parse_scheme(s: &str) -> Option<Result<SecretRef, SecretError>> {
    let malformed = || Err(SecretError::InvalidReference(s.to_string()));
    if let Some(rest) = s.strip_prefix(KEYCHAIN_PREFIX) {
        let Some((service, account)) = rest.split_once('/') else {
            return Some(malformed());
        };
        if service.is_empty() || account.is_empty() {
            return Some(malformed());
        }
        return Some(Ok(SecretRef::keychain(service, account)));
    }
    if let Some(rest) = s.strip_prefix(ONEPASSWORD_PREFIX) {
        // `op read` needs at least `vault/item/field`; deeper validation
        // (existence, extra segments like `?attribute=…`) is `op`'s job.
        let segments: Vec<&str> = rest.split('/').collect();
        if segments.len() < 3 || segments.iter().take(3).any(|s| s.is_empty()) {
            return Some(malformed());
        }
        return Some(Ok(SecretRef::onepassword(s)));
    }
    if let Some(rest) = s.strip_prefix(COMMAND_PREFIX) {
        // Anything after the prefix is the command, verbatim. Only an
        // empty/blank command is rejected — the command's own validity is
        // the shell's job at resolve time.
        if rest.trim().is_empty() {
            return Some(malformed());
        }
        return Some(Ok(SecretRef::command(rest)));
    }
    if let Some(rest) = s.strip_prefix(BITWARDEN_PREFIX) {
        // Split at the **last** `/`: the item may contain one, the `bw get`
        // object name cannot. Splitting at the first `/` instead would make
        // `bw:github.com/myorg/password` unreachable forever.
        let Some((item, field)) = rest.rsplit_once('/') else {
            return Some(malformed());
        };
        if item.is_empty() || field.is_empty() {
            return Some(malformed());
        }
        // The `bw get` object vocabulary is deliberately not validated here:
        // hardcoding it would couple totsuka releases to Bitwarden's, and an
        // unknown object is reported by `bw` itself (ADR-0006's "existence is
        // the CLI's job", applied to the vocabulary).
        return Some(Ok(SecretRef::bitwarden(item, field)));
    }
    None
}

/// Whether `value` is written as a store-backed secret reference.
///
/// Used by the resolver to tell "fetch this from a backend" from "expand
/// `${VAR}` in this string". Derived from the same parser [`FromStr`] uses, so
/// a scheme added to the parser is recognised here without a second edit.
pub fn is_secret_reference(value: &str) -> bool {
    parse_scheme(value).is_some()
}

/// Errors from resolving a secret reference.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The reference string was not a well-formed `keychain:<service>/<account>`,
    /// `op://<vault>/<item>/<field>`, `cmd:<command>`, or `bw:<item>/<field>`.
    #[error("invalid secret reference: {0}")]
    InvalidReference(String),
    /// No secret exists for the reference.
    #[error("secret not found: {reference}")]
    NotFound { reference: String },
    /// The underlying secret backend failed.
    #[error("secret backend error: {0}")]
    Backend(String),
    /// The backend tool for this reference scheme is not installed.
    ///
    /// `install_hint` comes from the backend rather than being baked in here:
    /// §7 wants a next action, and only the backend knows its own. The hint
    /// used to be hardcoded to 1Password's, which reads as an outright wrong
    /// instruction the moment a second shell-out backend exists.
    #[error("secret backend `{backend}` is not available → {install_hint}")]
    BackendUnavailable {
        /// Display name of the missing tool (e.g. `1Password CLI (op)`).
        backend: String,
        /// How to install it, phrased as an imperative next action.
        install_hint: String,
    },
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
    fn parses_bitwarden_reference() {
        let r: SecretRef = "bw:totsuka-slack/password".parse().unwrap();
        assert_eq!(r, SecretRef::bitwarden("totsuka-slack", "password"));
        assert_eq!(r.to_string(), "bw:totsuka-slack/password");
    }

    /// The split is at the **last** `/`, not the first: a Bitwarden item name
    /// may contain `/` (`github.com/myorg`) while the `bw get` object
    /// vocabulary never does. This is the opposite of `keychain:`, where the
    /// *account* is the part allowed to contain `/` — splitting the same way
    /// would make such items permanently unreachable.
    #[test]
    fn bitwarden_item_may_contain_slashes() {
        let r: SecretRef = "bw:github.com/myorg/password".parse().unwrap();
        assert_eq!(r, SecretRef::bitwarden("github.com/myorg", "password"));
        // The reference round-trips, so error messages quote it as written.
        assert_eq!(r.to_string(), "bw:github.com/myorg/password");
    }

    /// The `bw get` object vocabulary is not validated here: hardcoding it
    /// would couple a totsuka release to Bitwarden's, and `bw` reports an
    /// unknown object itself.
    #[test]
    fn bitwarden_field_vocabulary_is_left_to_the_cli() {
        for field in ["password", "username", "totp", "uri", "some-future-object"] {
            let r: SecretRef = format!("bw:item/{field}").parse().unwrap();
            assert_eq!(r, SecretRef::bitwarden("item", field));
        }
    }

    /// `is_secret_reference` is derived from the parser, so the two cannot
    /// disagree about what carries a scheme — the resolver used to keep its
    /// own prefix list and would have missed `bw:` entirely (#699).
    #[test]
    fn every_scheme_is_recognised_as_a_reference() {
        for reference in [
            "keychain:totsuka/token",
            "op://Dev/Item/field",
            "cmd:gh auth token",
            "bw:totsuka-slack/password",
        ] {
            assert!(is_secret_reference(reference), "{reference}");
            assert!(reference.parse::<SecretRef>().is_ok(), "{reference}");
        }
        for plain in ["${TOTSUKA_TOKEN}", "plain-value", "https://example.com"] {
            assert!(!is_secret_reference(plain), "{plain}");
        }
    }

    /// A known prefix with a malformed body stays a *reference* — the
    /// resolver must report it rather than silently expanding the config's
    /// literal text as a plain string and handing that to an API.
    #[test]
    fn a_malformed_reference_is_still_a_reference() {
        for bad in ["op://only-vault", "bw:no-field", "keychain:noslash", "cmd:"] {
            assert!(is_secret_reference(bad), "{bad}");
            assert!(bad.parse::<SecretRef>().is_err(), "{bad}");
        }
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
            // bw: needs both an item and a field.
            "bw:",
            "bw:no-field",
            "bw:/password",
            "bw:item/",
        ] {
            assert!(
                bad.parse::<SecretRef>().is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
