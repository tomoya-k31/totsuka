//! spec §11.13: refuse to spawn Claude with secret-like CLI flags. Secrets go
//! via env vars on the herdr `agent.start` payload.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub offending: String,
}

fn pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^--.*(?:token|secret|password|key)").unwrap())
}

/// Reject the entire spawn if any argv element matches the secret-like regex.
/// First match wins (we report only one offender so the user can fix and retry).
pub fn check_argv(argv: &[String]) -> Result<(), Violation> {
    for a in argv {
        if pattern().is_match(a) {
            return Err(Violation {
                offending: a.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_token_flag() {
        let err = check_argv(&["claude".into(), "--api-token".into(), "tk_x".into()]).unwrap_err();
        assert_eq!(err.offending, "--api-token");
    }

    #[test]
    fn rejects_secret_flag_case_insensitive() {
        let err = check_argv(&["claude".into(), "--MY-SECRET-FLAG".into()]).unwrap_err();
        assert_eq!(err.offending, "--MY-SECRET-FLAG");
    }

    #[test]
    fn rejects_password_flag() {
        assert!(check_argv(&["claude".into(), "--password=x".into()]).is_err());
    }

    #[test]
    fn rejects_key_flag() {
        assert!(check_argv(&["--ssh-key".into()]).is_err());
    }

    #[test]
    fn allows_benign_flags() {
        assert!(check_argv(&[
            "claude".into(),
            "--model".into(),
            "claude-sonnet-4-6".into(),
            "--prompt-file".into(),
            "spec.md".into(),
        ])
        .is_ok());
    }
}
