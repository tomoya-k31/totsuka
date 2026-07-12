//! Fallback [`SecretStore`] for non-macOS platforms.
//!
//! v1 only ships a Keychain backend (macOS). On other platforms every lookup
//! reports [`SecretError::Unsupported`] rather than failing to compile, so the
//! crate stays portable (e.g. Linux CI) until a native backend is added.

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// A [`SecretStore`] whose lookups always fail with [`SecretError::Unsupported`].
#[derive(Debug, Default, Clone, Copy)]
pub struct UnsupportedSecretStore;

impl SecretStore for UnsupportedSecretStore {
    fn get(&self, _reference: &SecretRef) -> Result<SecretString, SecretError> {
        Err(SecretError::Unsupported)
    }
}
