//! macOS Keychain-backed [`SecretStore`] (F-62, §5.6).
//!
//! Reads generic-password items from the login Keychain via the `keyring`
//! crate (`apple-native` backend). Only the Orchestrator holds this access;
//! resolved values are passed to plugins so plugins never touch the Keychain
//! (F-65).

use crate::ports::{SecretError, SecretRef, SecretStore, SecretString};

/// [`SecretStore`] reading from the macOS Keychain.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainSecretStore;

impl SecretStore for KeychainSecretStore {
    fn get(&self, reference: &SecretRef) -> Result<SecretString, SecretError> {
        let SecretRef::Keychain { service, account } = reference else {
            // The composite store routes by scheme; a non-keychain reference
            // reaching here is a wiring bug, not a user error.
            return Err(SecretError::InvalidReference(reference.to_string()));
        };
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| SecretError::Backend(e.to_string()))?;
        match entry.get_password() {
            Ok(value) => Ok(SecretString::new(value)),
            Err(keyring::Error::NoEntry) => Err(SecretError::NotFound {
                reference: reference.to_string(),
            }),
            Err(e) => Err(SecretError::Backend(e.to_string())),
        }
    }
}
