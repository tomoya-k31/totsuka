//! Secret redaction rules (§5.2): the last line of defence against secrets
//! reaching logs, complementing the type-level [`SecretString`](crate::ports::SecretString).
//!
//! Two independent mechanisms:
//!
//! - **Field denylist**: a field whose *name* looks secret (e.g. `api_key`,
//!   `authorization`, `*_token`) has its whole value replaced with `***`.
//! - **Value patterns**: token-shaped substrings (`Bearer …`, `ghp_…`,
//!   `sk-…`, …) are redacted even in otherwise-innocent fields (e.g. a
//!   `message` that interpolated a token).

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// The replacement written in place of a secret.
pub const REDACTED: &str = "***";

/// Whether a field *name* marks its value as secret (case-insensitive).
///
/// Uses an exact set plus secret-y suffixes, chosen to avoid redacting benign
/// numeric fields such as `max_tokens` or `token_count`.
pub fn is_secret_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const EXACT: &[&str] = &[
        "api_key",
        "apikey",
        "token",
        "authorization",
        "secret",
        "password",
        "passwd",
        "credential",
        "credentials",
        "access_token",
        "refresh_token",
        "client_secret",
        "private_key",
    ];
    if EXACT.contains(&n.as_str()) {
        return true;
    }
    const SUFFIX: &[&str] = &[
        "_token",
        "_secret",
        "_key",
        "_password",
        "_apikey",
        "_credential",
    ];
    SUFFIX.iter().any(|s| n.ends_with(s))
}

/// Whether a field carries prompt/RPC-payload content, which is only logged at
/// debug+ and can be disabled via `[log] log_prompts = false` (§5.2).
pub fn is_prompt_field(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "prompt" | "rpc_payload" | "payload" | "request_body" | "response_body"
    )
}

/// Compiled value-pattern regexes (built once).
fn value_patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Authorization scheme + credential -> keep the scheme, drop token.
            (
                Regex::new(r"(?i)\b(Bearer|Basic|Token)\s+[A-Za-z0-9._~+/=\-]+").unwrap(),
                "$1 ***",
            ),
            // Provider token shapes -> fully redacted.
            (
                Regex::new(
                    r"\b(?:gh[posur]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9]{16,}|xox[baprs]-[A-Za-z0-9-]{10,}|secret_[A-Za-z0-9]{8,}|AKIA[0-9A-Z]{16})\b",
                )
                .unwrap(),
                REDACTED,
            ),
        ]
    })
}

/// Redact token-shaped substrings from an arbitrary value.
pub fn redact_value(value: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(value);
    for (re, replacement) in value_patterns() {
        if re.is_match(&out) {
            out = Cow::Owned(re.replace_all(&out, *replacement).into_owned());
        }
    }
    out
}

/// Redact a `(name, value)` field: secret-named fields are fully replaced;
/// others have token-shaped substrings redacted.
pub fn redact_field<'a>(name: &str, value: &'a str) -> Cow<'a, str> {
    if is_secret_field(name) {
        Cow::Borrowed(REDACTED)
    } else {
        redact_value(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_named_fields_are_fully_redacted() {
        for name in [
            "api_key",
            "API_KEY",
            "Authorization",
            "token",
            "access_token",
            "client_secret",
            "db_password",
            "ssh_private_key",
        ] {
            assert_eq!(
                redact_field(name, "super-secret-value"),
                REDACTED,
                "field {name} should be redacted"
            );
        }
    }

    #[test]
    fn benign_numeric_fields_are_not_redacted() {
        // Observability fields that merely contain "token" must survive.
        for name in ["max_tokens", "token_count", "tokens_used", "prompt_tokens"] {
            assert_eq!(redact_field(name, "1234"), "1234", "field {name}");
        }
    }

    #[test]
    fn value_patterns_redacted_in_free_text() {
        let cases = [
            (
                "call failed: Bearer abc123DEF.ghi",
                "call failed: Bearer ***",
            ),
            ("token ghp_0123456789abcdefghijABCD used", "token *** used"),
            ("key sk-abcdefghij0123456789 here", "key *** here"),
            ("slack xoxb-123456789-abcdefg rotated", "slack *** rotated"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_value(input), expected, "input: {input}");
        }
    }

    #[test]
    fn plain_text_is_left_alone() {
        assert_eq!(redact_value("nothing secret here"), "nothing secret here");
        assert_eq!(redact_field("repo", "totsuka"), "totsuka");
    }

    #[test]
    fn prompt_fields_are_recognized() {
        assert!(is_prompt_field("prompt"));
        assert!(is_prompt_field("RPC_PAYLOAD"));
        assert!(!is_prompt_field("repo"));
    }
}
