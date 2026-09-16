//! Bitwarden-backed [`SecretStore`] (#699): resolves `bw:<item>/<field>`
//! references by shelling out to the official Bitwarden CLI
//! (`bw get <field> <item>`).
//!
//! A CLI shell-out for the same reasons as [`onepassword`](super::onepassword):
//! no SDK dependency, and `bw` ships for every platform, so this module carries
//! no `#[cfg]` gate.
//!
//! # Why the reference is `bw:` and not `bw://`
//!
//! `op://` is a **real URI that `op read` accepts**, which is why ADR-0006
//! adopted it verbatim. Bitwarden has no counterpart: `bw get` takes an object
//! name and an item, and there is no URI form at all. Writing `bw://` would be
//! inventing a URI that does not exist, which is the same objection ADR-0044
//! raised against `cmd://`. `<field>` is a `bw get` object name so the
//! reference reads the way the CLI is actually invoked.
//!
//! # The session is the hard part
//!
//! `op` keeps a session the desktop app can unlock; `bw` does not. `bw unlock`
//! prints a session key that must travel in `BW_SESSION` (or `--session`), and
//! **without one `bw` prompts for the master password on stdin**. `totsuka run`
//! is a long-lived process, so hitting that prompt is a silent hang, not a
//! visible error. This backend therefore checks `BW_SESSION` *before spawning*
//! and fails with the recovery step instead. `--nointeraction` is passed as
//! well, so a session that expired between the check and the spawn errors out
//! rather than blocking.
//!
//! Unattended operation (a session that survives without a shell) is out of
//! scope here exactly as service-account tokens are for 1Password — ADR-0006
//! drew the same line for `op`, and doing more for Bitwarden alone would be
//! asymmetric.
//!
//! Security invariants (F-65, §5.2), identical to the other backends: `bw`'s
//! **stdout is the plaintext secret** and is only ever wrapped into a
//! [`SecretString`] — never logged, never put in an error. Classification uses
//! stderr only.

use std::io;
use std::process::{Command, Output};
use std::sync::Arc;

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// Display name of the backend, used in [`SecretError::BackendUnavailable`].
pub(crate) const BACKEND_NAME: &str = "Bitwarden CLI (bw)";

/// What to do when `bw` is missing (§7 wants a next action).
pub(crate) const INSTALL_HINT: &str = "install it (macOS: `brew install bitwarden-cli`, \
     other platforms: https://bitwarden.com/help/cli/)";

/// The environment variable carrying the unlocked-vault session key.
pub(crate) const SESSION_ENV: &str = "BW_SESSION";

/// How `bw` is invoked — a seam so tests cover every outcome without the real
/// binary (CI has no `bw`, and the real one needs an account).
type BwRunner = dyn Fn(&str, &[&str]) -> io::Result<Output> + Send + Sync;

/// Whether an unlocked session is available — a seam over `BW_SESSION`.
type SessionProbe = dyn Fn() -> bool + Send + Sync;

/// [`SecretStore`] resolving `bw:` references via the Bitwarden CLI.
#[derive(Clone)]
pub struct BitwardenCli {
    binary: String,
    runner: Arc<BwRunner>,
    session: Arc<SessionProbe>,
}

impl Default for BitwardenCli {
    fn default() -> Self {
        Self {
            binary: "bw".to_string(),
            runner: Arc::new(|bin, args| Command::new(bin).args(args).output()),
            // Read at resolve time, not at construction: `totsuka run` builds
            // the store once and resolves later, and the operator may have
            // exported the session in between.
            session: Arc::new(|| std::env::var_os(SESSION_ENV).is_some_and(|v| !v.is_empty())),
        }
    }
}

impl BitwardenCli {
    /// A store with a custom command runner and an unlocked session (tests).
    pub fn with_runner(
        runner: impl Fn(&str, &[&str]) -> io::Result<Output> + Send + Sync + 'static,
    ) -> Self {
        Self {
            binary: "bw".to_string(),
            runner: Arc::new(runner),
            session: Arc::new(|| true),
        }
    }

    /// The same store, but reporting no unlocked session (tests).
    pub fn without_session(self) -> Self {
        Self {
            session: Arc::new(|| false),
            ..self
        }
    }
}

impl SecretStore for BitwardenCli {
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
        let SecretRef::Bitwarden { item, field } = reference else {
            // The composite store routes by scheme; a non-bw reference
            // reaching here is a wiring bug, not a user error.
            return Err(SecretError::InvalidReference(reference.to_string()));
        };
        if !(self.session)() {
            // Deliberately before the spawn: with no session `bw` reads the
            // master password from stdin, which hangs an unattended
            // `totsuka run` forever instead of failing.
            return Err(SecretError::Backend(format!(
                "no Bitwarden session ({SESSION_ENV} is not set) → run `bw unlock`, \
                 export the {SESSION_ENV} it prints, and start `totsuka run` from that shell"
            )));
        }
        // `--nointeraction` is not optional: it is what turns an expired
        // session into an error instead of a stdin prompt.
        let output = (self.runner)(&self.binary, &["get", field, item, "--nointeraction"])
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => SecretError::BackendUnavailable {
                    backend: BACKEND_NAME.to_string(),
                    install_hint: INSTALL_HINT.to_string(),
                },
                _ => SecretError::Backend(format!("could not run `bw`: {e}")),
            })?;
        if !output.status.success() {
            return Err(classify_bw_error(&output.stderr, reference));
        }
        let mut value = String::from_utf8(output.stdout)
            .map_err(|_| SecretError::Backend(format!("`{reference}` returned non-UTF-8 data")))?;
        // `bw` has no `--no-newline`, so the line framing is trimmed here the
        // way `cmd:` does it (ADR-0044); left in place it corrupts whatever
        // header the secret ends up in.
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

/// Map a failed `bw get`'s stderr to an actionable [`SecretError`] (§7).
///
/// stderr carries the reference and diagnostics — never the secret — so
/// quoting it is safe.
fn classify_bw_error(stderr: &[u8], reference: &SecretRef) -> SecretError {
    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_lowercase();
    // The vault locked (or the session expired) between the pre-spawn check
    // and here. Same recovery as a missing session.
    //
    // Matched on the whole phrase, never a bare `"locked"`: **"unlocked"
    // contains "locked"**, so the loose test would report a message about a
    // perfectly healthy vault as a locked one.
    if lower.contains("vault is locked") {
        return SecretError::Backend(format!(
            "the Bitwarden vault is locked → run `bw unlock`, export the {SESSION_ENV} it \
             prints, and retry ({})",
            first_line(&text)
        ));
    }
    if lower.contains("not logged in") {
        return SecretError::Backend(format!(
            "not logged in to Bitwarden → run `bw login`, then `bw unlock` ({})",
            first_line(&text)
        ));
    }
    // `bw get` can only return one object, so an ambiguous search string is a
    // configuration problem with a concrete fix: name the item by its id.
    if lower.contains("more than one result") {
        return SecretError::Backend(format!(
            "`{reference}` matches more than one Bitwarden item → use the item's id \
             (`bw list items --search <name>` prints it) instead of a name"
        ));
    }
    // Deliberately **not** a bare `"not found"` search, for the reason recorded
    // in [`classify_op_error`](super::onepassword): it misclassifies unrelated
    // failures — "server not found" from a DNS error naming a self-hosted
    // host, say — as a missing item. That trade is worse than it looks, because
    // `NotFound` carries only the reference: the stderr diagnosis and the next
    // action (§7) are both dropped, so the operator is told the item is absent
    // when the truth is that the server is unreachable.
    //
    // `bw get` prints exactly `Not found.`, so the first line is *compared*
    // rather than searched. Anything else stays `Backend`, which quotes stderr.
    if matches!(
        first_line(&text).to_lowercase().trim(),
        "not found." | "not found"
    ) {
        return SecretError::NotFound {
            reference: reference.to_string(),
        };
    }
    SecretError::Backend(format!("`{reference}` failed: {}", first_line(&text)))
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

    fn bw_ref(item: &str, field: &str) -> SecretRef {
        SecretRef::bitwarden(item, field)
    }

    #[test]
    fn reads_a_secret_and_records_the_exact_argv() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let record = seen.clone();
        let store = BitwardenCli::with_runner(move |bin, args| {
            let mut call = vec![bin.to_string()];
            call.extend(args.iter().map(|a| a.to_string()));
            *record.lock().unwrap() = call;
            Ok(output(0, b"sk-plain\n", b""))
        });
        let secret = store.get(&bw_ref("totsuka-slack", "password")).unwrap();
        // The trailing newline is line framing, not part of the value.
        assert_eq!(secret.expose(), "sk-plain");
        // Field before item, exactly as `bw get <object> <id>` is invoked, and
        // `--nointeraction` so an expired session cannot prompt on stdin.
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["bw", "get", "password", "totsuka-slack", "--nointeraction"]
        );
    }

    /// The item may contain `/`; the object name never does. Parsing splits at
    /// the last `/`, and the backend must pass the pieces through unchanged.
    #[test]
    fn an_item_name_containing_a_slash_survives() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let record = seen.clone();
        let store = BitwardenCli::with_runner(move |_, args| {
            *record.lock().unwrap() = args.iter().map(|a| a.to_string()).collect();
            Ok(output(0, b"v", b""))
        });
        let reference: SecretRef = "bw:github.com/myorg/password".parse().unwrap();
        store.get(&reference).unwrap();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["get", "password", "github.com/myorg", "--nointeraction"]
        );
    }

    /// Without a session `bw` reads the master password from stdin, which
    /// hangs an unattended `totsuka run`. The check must happen *before* the
    /// spawn, so no process is started at all.
    #[test]
    fn a_missing_session_fails_without_spawning() {
        let spawned = Arc::new(Mutex::new(false));
        let flag = spawned.clone();
        let store = BitwardenCli::with_runner(move |_, _| {
            *flag.lock().unwrap() = true;
            Ok(output(0, b"never", b""))
        })
        .without_session();
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        assert!(!*spawned.lock().unwrap(), "bw must not be spawned at all");
        let msg = err.to_string();
        assert!(msg.contains("BW_SESSION"), "{msg}");
        assert!(msg.contains("bw unlock"), "{msg}");
    }

    #[test]
    fn missing_binary_is_backend_unavailable_with_the_right_install_hint() {
        let store = BitwardenCli::with_runner(|_, _| {
            Err(io::Error::new(io::ErrorKind::NotFound, "no such file"))
        });
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        assert!(matches!(err, SecretError::BackendUnavailable { .. }));
        let msg = err.to_string();
        // The hint names *this* backend's CLI — the bug #699 fixed was that
        // every backend told the operator to install 1Password's.
        assert!(msg.contains("brew install bitwarden-cli"), "{msg}");
        assert!(!msg.contains("1password"), "{msg}");
    }

    #[test]
    fn a_locked_vault_names_bw_unlock() {
        let store = BitwardenCli::with_runner(|_, _| Ok(output(1, b"", b"Vault is locked.")));
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        assert!(err.to_string().contains("bw unlock"), "{err}");
    }

    #[test]
    fn being_logged_out_names_bw_login() {
        let store = BitwardenCli::with_runner(|_, _| Ok(output(1, b"", b"You are not logged in.")));
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        assert!(err.to_string().contains("bw login"), "{err}");
    }

    /// `bw get` can only return one object, so an ambiguous name is a config
    /// problem — the error has to say how to disambiguate.
    #[test]
    fn an_ambiguous_item_says_to_use_the_id() {
        let store = BitwardenCli::with_runner(|_, _| {
            Ok(output(1, b"", b"More than one result was found."))
        });
        let err = store.get(&bw_ref("shared", "password")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("item's id"), "{msg}");
        assert!(msg.contains("bw:shared/password"), "{msg}");
    }

    #[test]
    fn a_missing_item_is_not_found() {
        let store = BitwardenCli::with_runner(|_, _| Ok(output(1, b"", b"Not found.")));
        let err = store.get(&bw_ref("gone", "password")).unwrap_err();
        match err {
            SecretError::NotFound { reference } => assert_eq!(reference, "bw:gone/password"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    /// A bare `"not found"` search would call a DNS or connection failure a
    /// missing item — and `NotFound` carries only the reference, so the stderr
    /// diagnosis and the next action would both be lost. `classify_op_error`
    /// records the same decision.
    #[test]
    fn an_unreachable_server_is_not_a_missing_item() {
        for stderr in [
            &b"getaddrinfo ENOTFOUND vault.example.com: server not found"[..],
            &b"Error: connect ECONNREFUSED 127.0.0.1:8080"[..],
        ] {
            let store = BitwardenCli::with_runner(move |_, _| Ok(output(1, b"", stderr)));
            let err = store.get(&bw_ref("x", "password")).unwrap_err();
            assert!(
                !matches!(err, SecretError::NotFound { .. }),
                "{err} must stay a Backend error so the diagnosis survives"
            );
            // The diagnosis is what makes it actionable.
            assert!(
                err.to_string().to_lowercase().contains("not found")
                    || err.to_string().contains("ECONNREFUSED"),
                "{err}"
            );
        }
    }

    /// `"unlocked"` contains `"locked"`, so a bare substring test would report
    /// a healthy vault as a locked one.
    #[test]
    fn an_unlocked_vault_is_not_reported_as_locked() {
        let store = BitwardenCli::with_runner(|_, _| {
            Ok(output(
                1,
                b"",
                b"Vault is unlocked but the item could not be read",
            ))
        });
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        assert!(!err.to_string().contains("bw unlock"), "{err}");
    }

    /// An empty value would surface later as an unexplained 401; fail now.
    #[test]
    fn empty_output_is_an_error() {
        let store = BitwardenCli::with_runner(|_, _| Ok(output(0, b"\n", b"")));
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        assert!(err.to_string().contains("no output"), "{err}");
    }

    /// §5.2: stdout may be a partially written secret, so only stderr is
    /// quoted.
    #[test]
    fn other_failures_quote_stderr_never_stdout() {
        let store =
            BitwardenCli::with_runner(|_, _| Ok(output(1, b"half-a-secret", b"connection reset")));
        let err = store.get(&bw_ref("x", "password")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("connection reset"), "{msg}");
        assert!(!msg.contains("half-a-secret"), "{msg}");
    }

    #[test]
    fn a_non_bitwarden_reference_is_a_wiring_error() {
        let store = BitwardenCli::with_runner(|_, _| Ok(output(0, b"", b"")));
        let err = store.get(&SecretRef::keychain("svc", "acct")).unwrap_err();
        assert!(matches!(err, SecretError::InvalidReference(_)));
    }
}
