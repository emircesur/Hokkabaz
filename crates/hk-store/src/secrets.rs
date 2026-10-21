//! Secrets API. Values are wrapped in [`Secret`] (redacted Debug) and live only in
//! a [`SecretStore`] backend: OS keyring on desktop, in-memory for tests.
//! Android Keystore backend lands in P6.2; daemon age-file backend later.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret backend: {0}")]
    Backend(String),
}

/// Secret value. Debug/Display never print contents; use [`Secret::expose`] deliberately.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(v: impl Into<String>) -> Self {
        Self(v.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Backend for secret values. Sync: keyring APIs are blocking.
pub trait SecretStore: Send + Sync + fmt::Debug {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError>;
    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError>;
    /// Deleting a missing secret is Ok.
    fn delete(&self, name: &str) -> Result<(), SecretError>;
}

/// Non-persistent backend for tests and headless fallbacks.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    map: Mutex<HashMap<String, Secret>>,
}

impl MemorySecretStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Secret>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.lock().get(name).cloned())
    }

    fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
        self.lock().insert(name.to_owned(), value.clone());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), SecretError> {
        self.lock().remove(name);
        Ok(())
    }
}

#[cfg(all(feature = "keyring", not(target_os = "android")))]
pub use keyring_backend::KeyringSecretStore;

#[cfg(all(feature = "keyring", not(target_os = "android")))]
mod keyring_backend {
    use super::{Secret, SecretError, SecretStore};

    /// OS keyring (Keychain / Credential Manager / Secret Service).
    #[derive(Debug, Clone)]
    pub struct KeyringSecretStore {
        service: String,
    }

    impl KeyringSecretStore {
        pub fn new(service: impl Into<String>) -> Self {
            Self { service: service.into() }
        }

        fn entry(&self, name: &str) -> Result<keyring::Entry, SecretError> {
            keyring::Entry::new(&self.service, name).map_err(|e| SecretError::Backend(e.to_string()))
        }
    }

    impl Default for KeyringSecretStore {
        fn default() -> Self {
            Self::new("hokkabaz")
        }
    }

    impl SecretStore for KeyringSecretStore {
        fn get(&self, name: &str) -> Result<Option<Secret>, SecretError> {
            match self.entry(name)?.get_password() {
                Ok(v) => Ok(Some(Secret::new(v))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(SecretError::Backend(e.to_string())),
            }
        }

        fn set(&self, name: &str, value: &Secret) -> Result<(), SecretError> {
            self.entry(name)?.set_password(value.expose()).map_err(|e| SecretError::Backend(e.to_string()))
        }

        fn delete(&self, name: &str) -> Result<(), SecretError> {
            match self.entry(name)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(SecretError::Backend(e.to_string())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_redacted() {
        let s = Secret::new("sk-topsecret");
        assert!(!format!("{s:?}").contains("topsecret"));
        assert!(!format!("{s}").contains("topsecret"));
        let be = MemorySecretStore::default();
        be.set("k", &s).unwrap();
        assert!(!format!("{be:?}").contains("topsecret"));
    }
}
