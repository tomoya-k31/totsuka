//! Fallback for the **`keychain:` scheme** on non-macOS platforms.
//!
//! Only macOS ships a Keychain backend; here every `keychain:` lookup reports
//! [`SecretError::Unsupported`] rather than failing to compile, so the crate
//! stays portable (e.g. Linux CI) until a native backend is added. This covers
//! the `keychain:` scheme only — `op://` references resolve on every platform
//! via [`onepassword`](super::onepassword) (#156), which the composite
//! [`PlatformSecretStore`](super::PlatformSecretStore) routes to first.

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// A [`SecretStore`] whose lookups always fail with [`SecretError::Unsupported`].
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSecretStore;

impl SecretStore for UnsupportedSecretStore {
    fn get(&self, _reference: &SecretRef) -> Result<SecretString, SecretError> {
        Err(SecretError::Unsupported)
    }
}
