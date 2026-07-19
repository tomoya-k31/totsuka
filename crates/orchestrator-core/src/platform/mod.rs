//! Platform-specific implementations of the OS-dependent [`ports`](crate::ports).
//!
//! Per §5.6, OS-specific code is isolated here so the rest of the crate stays
//! portable. The Keychain-backed secret store is macOS-only; a fallback that
//! reports [`SecretError::Unsupported`](crate::ports::SecretError::Unsupported)
//! keeps the crate compiling on other platforms (e.g. Linux CI) without
//! `#[cfg]` leaking into callers. The 1Password backend
//! ([`onepassword`]) shells out to the cross-platform `op` CLI and therefore
//! carries no `#[cfg]` gate at all — on non-macOS it is the first *working*
//! secret backend. Process liveness is POSIX-generic and lives in [`unix`].

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

#[cfg(unix)]
pub mod unix;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(not(target_os = "macos"))]
pub mod fallback;

pub mod onepassword;

/// The `keychain:` backend for the current platform (Keychain on macOS,
/// `Unsupported` elsewhere).
#[cfg(target_os = "macos")]
type KeychainBackend = macos::KeychainSecretStore;
#[cfg(not(target_os = "macos"))]
type KeychainBackend = fallback::UnsupportedSecretStore;

/// The [`SecretStore`](crate::ports::SecretStore) for the current platform:
/// a composite that routes each [`SecretRef`] to its scheme's backend —
/// `keychain:` to the OS Keychain (or the non-macOS fallback), `op://` to the
/// 1Password CLI ([`onepassword::OnePasswordCli`], every platform).
#[derive(Clone, Default)]
pub struct PlatformSecretStore {
    keychain: KeychainBackend,
    onepassword: onepassword::OnePasswordCli,
}

impl SecretStore for PlatformSecretStore {
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
        match reference {
            SecretRef::Keychain { .. } => self.keychain.get(reference),
            SecretRef::OnePassword { .. } => self.onepassword.get(reference),
        }
    }
}

/// The [`ProcessProbe`](crate::ports::ProcessProbe) implementation for the
/// current platform.
#[cfg(unix)]
pub type PlatformProcessProbe = unix::UnixProcessProbe;
