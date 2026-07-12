//! Platform-specific implementations of the OS-dependent [`ports`](crate::ports).
//!
//! Per §5.6, OS-specific code is isolated here so the rest of the crate stays
//! portable. The Keychain-backed secret store is macOS-only; a fallback that
//! reports [`SecretError::Unsupported`](crate::ports::SecretError::Unsupported)
//! keeps the crate compiling on other platforms (e.g. Linux CI) without
//! `#[cfg]` leaking into callers. Process liveness is POSIX-generic and lives
//! in [`unix`].

#[cfg(unix)]
pub mod unix;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(not(target_os = "macos"))]
pub mod fallback;

/// The [`SecretStore`](crate::ports::SecretStore) implementation for the
/// current platform.
#[cfg(target_os = "macos")]
pub type PlatformSecretStore = macos::KeychainSecretStore;
/// The [`SecretStore`](crate::ports::SecretStore) implementation for the
/// current platform.
#[cfg(not(target_os = "macos"))]
pub type PlatformSecretStore = fallback::UnsupportedSecretStore;

/// The [`ProcessProbe`](crate::ports::ProcessProbe) implementation for the
/// current platform.
#[cfg(unix)]
pub type PlatformProcessProbe = unix::UnixProcessProbe;
