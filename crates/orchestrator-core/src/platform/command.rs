//! Command-backed [`SecretStore`] (#444): resolves `cmd:<command>` references
//! by running the command via `/bin/sh -c` and using its stdout as the secret.
//!
//! For credentials another tool already manages and **rotates**
//! (`cmd:gh auth token`): resolving re-runs the command, so there is no stored
//! copy to go stale — the failure mode that copying a rotating token into
//! 1Password or the Keychain cannot avoid.
//!
//! This grants the config no new power: a config already names plugin binaries
//! that the orchestrator executes, so whoever can write it can already run
//! commands. Execution happens only at *resolve* time (the `totsuka run`
//! startup path); parsing a config or printing it never runs anything, and
//! `doctor` skips `cmd:` probes the same way it skips `op://` (#289 — doctor
//! cannot know the command is prompt-free).
//!
//! Security invariants (F-65, §5.2), identical to
//! [`onepassword`](super::onepassword): **stdout is the plaintext secret** and
//! is only ever wrapped into a [`SecretString`] — never logged, never put in
//! an error. Error messages quote stderr only.

use std::io;
use std::process::{Command, Output};
use std::sync::Arc;

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// How the command is invoked — a seam so tests cover every outcome without
/// spawning real processes. The runner receives the command string.
type CmdRunner = dyn Fn(&str) -> io::Result<Output> + Send + Sync;

/// [`SecretStore`] resolving `cmd:` references via `/bin/sh -c`.
#[derive(Clone)]
pub struct CommandSecretStore {
    runner: Arc<CmdRunner>,
}

impl Default for CommandSecretStore {
    fn default() -> Self {
        Self {
            // `sh -c` inherits the orchestrator's environment, so the command
            // sees the same PATH the operator launched `totsuka run` with
            // (mise / homebrew tools included).
            runner: Arc::new(|command| Command::new("/bin/sh").args(["-c", command]).output()),
        }
    }
}

impl CommandSecretStore {
    /// A store with a custom runner (tests).
    pub fn with_runner(
        runner: impl Fn(&str) -> io::Result<Output> + Send + Sync + 'static,
    ) -> Self {
        Self {
            runner: Arc::new(runner),
        }
    }
}

impl SecretStore for CommandSecretStore {
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
        let SecretRef::Command { command } = reference else {
            // The composite store routes by scheme; a non-cmd reference
            // reaching here is a wiring bug, not a user error.
            return Err(SecretError::InvalidReference(reference.to_string()));
        };
        let output = (self.runner)(command)
            .map_err(|e| SecretError::Backend(format!("could not run `/bin/sh -c`: {e}")))?;
        if !output.status.success() {
            // stderr carries diagnostics, never the secret, so quoting it is
            // safe; stdout could be a partially-written secret (§5.2).
            return Err(SecretError::Backend(format!(
                "`{reference}` failed ({}): {}",
                output.status,
                first_line(&String::from_utf8_lossy(&output.stderr)),
            )));
        }
        let mut value = String::from_utf8(output.stdout)
            .map_err(|_| SecretError::Backend(format!("`{reference}` returned non-UTF-8 data")))?;
        // CLIs terminate their output with a newline that is line framing,
        // not part of the value (`gh auth token` does); left in place it
        // corrupts whatever header the secret ends up in. `op read` solves
        // the same problem with `--no-newline`; an arbitrary command has no
        // such flag, so the trim happens here.
        while value.ends_with('\n') {
            value.pop();
            if value.ends_with('\r') {
                value.pop();
            }
        }
        if value.is_empty() {
            // Fail loudly at resolve time rather than hand an empty token to
            // an API and surface as an unexplained 401 later.
            return Err(SecretError::Backend(format!(
                "`{reference}` succeeded but produced no output → the secret would be empty"
            )));
        }
        Ok(SecretString::new(value))
    }
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

// Unix-gated for the same reason as the `onepassword` tests: building a fake
// `ExitStatus` needs `ExitStatusExt::from_raw`. The backend compiles everywhere.
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

    fn cmd_ref(command: &str) -> SecretRef {
        SecretRef::command(command)
    }

    #[test]
    fn runs_the_command_and_trims_the_trailing_newline() {
        let seen: Arc<Mutex<String>> = Arc::default();
        let record = seen.clone();
        let store = CommandSecretStore::with_runner(move |command| {
            *record.lock().unwrap() = command.to_string();
            Ok(output(0, b"gho_tok3n\n", b""))
        });
        let secret = store.get(&cmd_ref("gh auth token")).unwrap();
        assert_eq!(secret.expose(), "gho_tok3n");
        assert_eq!(seen.lock().unwrap().as_str(), "gh auth token");
    }

    #[test]
    fn trims_every_trailing_newline_but_nothing_else() {
        let store =
            CommandSecretStore::with_runner(|_| Ok(output(0, b"  value with spaces \r\n\n", b"")));
        // Interior/leading whitespace is the value's own business; only the
        // line framing at the end is removed.
        assert_eq!(
            store.get(&cmd_ref("x")).unwrap().expose(),
            "  value with spaces "
        );
    }

    #[test]
    fn a_failing_command_quotes_stderr_never_stdout() {
        // stdout could be a partially-written secret; only stderr may appear
        // in the error (§5.2).
        let store = CommandSecretStore::with_runner(|_| {
            Ok(output(
                1,
                b"half-a-secret",
                b"gh: not logged in\nrun gh auth login",
            ))
        });
        let err = store.get(&cmd_ref("gh auth token")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not logged in"), "got {msg}");
        assert!(msg.contains("cmd:gh auth token"), "got {msg}");
        assert!(!msg.contains("half-a-secret"));
    }

    #[test]
    fn empty_output_is_an_error_not_an_empty_secret() {
        let store = CommandSecretStore::with_runner(|_| Ok(output(0, b"\n", b"")));
        let err = store.get(&cmd_ref("true")).unwrap_err();
        assert!(
            err.to_string().contains("produced no output"),
            "an empty token must fail at resolve time, not as a 401 later: {err}"
        );
    }

    #[test]
    fn a_missing_shell_is_a_backend_error() {
        let store = CommandSecretStore::with_runner(|_| {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such file"))
        });
        let err = store.get(&cmd_ref("x")).unwrap_err();
        assert!(matches!(err, SecretError::Backend(_)));
        assert!(err.to_string().contains("/bin/sh"));
    }

    #[test]
    fn an_op_reference_is_a_wiring_error() {
        let store = CommandSecretStore::with_runner(|_| Ok(output(0, b"", b"")));
        let err = store
            .get(&SecretRef::onepassword("op://Dev/X/y"))
            .unwrap_err();
        assert!(matches!(err, SecretError::InvalidReference(_)));
    }

    /// The one test through the REAL default runner: `/bin/sh -c` exists on
    /// every platform CI runs (ubuntu, macOS), and every other test faking the
    /// runner proves nothing about the argv the default actually builds.
    #[test]
    fn the_default_runner_really_runs_the_shell() {
        let store = CommandSecretStore::default();
        let secret = store
            .get(&SecretRef::command("printf 'real-value\\n'"))
            .unwrap();
        assert_eq!(secret.expose(), "real-value");
        // And a real failing command classifies as Backend with its stderr.
        let err = store
            .get(&SecretRef::command("echo oops >&2; exit 3"))
            .unwrap_err();
        assert!(err.to_string().contains("oops"), "got {err}");
    }
}
