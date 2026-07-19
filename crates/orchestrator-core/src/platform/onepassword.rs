//! 1Password-backed [`SecretStore`] (#156): resolves `op://<vault>/<item>/<field>`
//! references by shelling out to the 1Password CLI (`op read`).
//!
//! Deliberately a CLI shell-out (no SDK/Connect dependency): it matches the
//! `op inject`-style ecosystem users already keep their references in, and —
//! unlike the Keychain backend — works on **every** platform `op` ships for,
//! so this module carries no `#[cfg]` gate (on Linux it is the first working
//! secret backend). v1 assumes an interactively unlocked session (`op signin`
//! done); unattended service-account tokens are a follow-up.
//!
//! Security invariants (F-65, §5.2): `op`'s **stdout is the plaintext secret**
//! and is only ever wrapped into a [`SecretString`] — never logged, never put
//! in an error. Error classification uses stderr only, which carries the
//! reference and diagnostics, not the secret.

use std::io;
use std::process::{Command, Output};
use std::sync::Arc;

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// How `op` is invoked — a seam so tests cover every outcome without the real
/// binary (CI has no `op`, and the real one would prompt for biometrics).
type OpRunner = dyn Fn(&str, &[&str]) -> io::Result<Output> + Send + Sync;

/// [`SecretStore`] resolving `op://` references via the 1Password CLI.
#[derive(Clone)]
pub struct OnePasswordCli {
    binary: String,
    runner: Arc<OpRunner>,
}

impl Default for OnePasswordCli {
    fn default() -> Self {
        Self {
            binary: "op".to_string(),
            runner: Arc::new(|bin, args| Command::new(bin).args(args).output()),
        }
    }
}

impl OnePasswordCli {
    /// A store with a custom command runner (tests) — the runner receives the
    /// binary name and the full argv.
    pub fn with_runner(
        runner: impl Fn(&str, &[&str]) -> io::Result<Output> + Send + Sync + 'static,
    ) -> Self {
        Self {
            binary: "op".to_string(),
            runner: Arc::new(runner),
        }
    }
}

impl SecretStore for OnePasswordCli {
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
        let SecretRef::OnePassword { uri } = reference else {
            // The composite store routes by scheme; a non-op reference
            // reaching here is a wiring bug, not a user error.
            return Err(SecretError::InvalidReference(reference.to_string()));
        };
        // `--no-newline` keeps the value byte-exact (no trailing-\n trim that
        // could eat a legitimate final newline in the stored secret).
        let output = (self.runner)(&self.binary, &["read", "--no-newline", uri]).map_err(|e| {
            match e.kind() {
                io::ErrorKind::NotFound => SecretError::BackendUnavailable {
                    backend: "1Password CLI (op)".to_string(),
                },
                _ => SecretError::Backend(format!("could not run `op`: {e}")),
            }
        })?;
        if output.status.success() {
            // stdout is the plaintext: wrap it immediately, never log it.
            let value = String::from_utf8(output.stdout)
                .map_err(|_| SecretError::Backend("`op read` returned non-UTF-8 data".into()))?;
            return Ok(SecretString::new(value));
        }
        Err(classify_op_error(&output.stderr, uri))
    }
}

/// Map a failed `op read`'s stderr to an actionable [`SecretError`] (§7).
/// stderr carries the reference and diagnostics — never the secret — so
/// quoting it is safe.
fn classify_op_error(stderr: &[u8], uri: &str) -> SecretError {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_lowercase();
    // Session missing/expired (`op` phrases this a few ways).
    if lower.contains("not currently signed in")
        || lower.contains("no account found")
        || lower.contains("session expired")
        || lower.contains("not signed in")
    {
        return SecretError::Backend(format!(
            "not signed in to 1Password → run `op signin` and retry ({})",
            first_line(&text)
        ));
    }
    // Vault/item/field does not exist.
    if lower.contains("isn't a vault")
        || lower.contains("isn't an item")
        || lower.contains("isn't a field")
        || lower.contains("no item matching")
        || lower.contains("not found")
    {
        return SecretError::NotFound {
            reference: uri.to_string(),
        };
    }
    SecretError::Backend(format!("`op read {uri}` failed: {}", first_line(&text)))
}

/// The first non-empty stderr line, trimmed — enough diagnosis for one error
/// message without pasting a whole CLI dump.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no stderr)")
        .to_string()
}

// Unix-gated: building a fake `ExitStatus` needs `ExitStatusExt::from_raw`,
// which has no portable equivalent. The backend itself compiles everywhere.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::sync::Mutex;

    fn output(code: i32, stdout: &[u8], stderr: &[u8]) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.to_vec(),
        }
    }

    fn op_ref(uri: &str) -> SecretRef {
        SecretRef::onepassword(uri)
    }

    #[test]
    fn reads_a_secret_and_records_the_exact_argv() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let record = seen.clone();
        let store = OnePasswordCli::with_runner(move |bin, args| {
            let mut call = vec![bin.to_string()];
            call.extend(args.iter().map(|a| a.to_string()));
            *record.lock().unwrap() = call;
            Ok(output(0, b"sk-plain", b""))
        });
        let secret = store.get(&op_ref("op://Dev/Openrouter/api_key")).unwrap();
        assert_eq!(secret.expose(), "sk-plain");
        // `--no-newline` keeps the value byte-exact; the URI rides verbatim.
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["op", "read", "--no-newline", "op://Dev/Openrouter/api_key"]
        );
    }

    #[test]
    fn missing_binary_is_backend_unavailable() {
        let store = OnePasswordCli::with_runner(|_, _| {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such file"))
        });
        let err = store.get(&op_ref("op://Dev/X/y")).unwrap_err();
        assert!(matches!(err, SecretError::BackendUnavailable { .. }));
        // The message carries the install next-action (§7).
        assert!(err.to_string().contains("brew install 1password-cli"));
    }

    #[test]
    fn missing_item_is_not_found() {
        let store = OnePasswordCli::with_runner(|_, _| {
            Ok(output(
                1,
                b"",
                b"[ERROR] 2026/07/19 \"Openrouter\" isn't an item in the \"Dev\" vault",
            ))
        });
        let err = store
            .get(&op_ref("op://Dev/Openrouter/api_key"))
            .unwrap_err();
        match err {
            SecretError::NotFound { reference } => {
                assert_eq!(reference, "op://Dev/Openrouter/api_key")
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn signed_out_session_names_op_signin() {
        let store = OnePasswordCli::with_runner(|_, _| {
            Ok(output(
                1,
                b"",
                b"[ERROR] you are not currently signed in. Please run `op signin --help`",
            ))
        });
        let err = store.get(&op_ref("op://Dev/X/y")).unwrap_err();
        assert!(matches!(err, SecretError::Backend(_)));
        assert!(err.to_string().contains("op signin"), "got {err}");
    }

    #[test]
    fn other_failures_quote_stderr_never_stdout() {
        // stdout could be a partially-written secret; only stderr may appear
        // in the error (§5.2).
        let store = OnePasswordCli::with_runner(|_, _| {
            Ok(output(1, b"half-a-secret", b"[ERROR] connection reset"))
        });
        let err = store.get(&op_ref("op://Dev/X/y")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("connection reset"));
        assert!(!msg.contains("half-a-secret"));
    }

    #[test]
    fn keychain_reference_is_a_wiring_error() {
        let store = OnePasswordCli::with_runner(|_, _| Ok(output(0, b"", b"")));
        let err = store.get(&SecretRef::keychain("svc", "acct")).unwrap_err();
        assert!(matches!(err, SecretError::InvalidReference(_)));
    }
}
