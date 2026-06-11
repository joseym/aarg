//! API keys live in the OS keychain (macOS Keychain, Windows Credential
//! Manager, the Secret Service on Linux) — never in config files, never in
//! environment variables we write down.
//!
//! The `keyring` crate talks to whichever store the OS provides. Its calls
//! are blocking, so this module wraps them for use from async code.

use keyring::Entry;

/// The service name every aarg keychain entry is filed under.
const SERVICE: &str = "aarg";

/// Everything that can go wrong while talking to the OS keychain.
#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("could not access the OS keychain")]
    Keychain(#[source] keyring::Error),

    #[error("the keychain task was interrupted before finishing")]
    Interrupted(#[source] tokio::task::JoinError),
}

/// Handle to the keychain entry for one provider's API key.
fn entry(provider: &str) -> Result<Entry, SecretsError> {
    Entry::new(SERVICE, provider).map_err(SecretsError::Keychain)
}

/// Store `key` as the API key for `provider`, replacing any existing one.
pub async fn store_api_key(provider: &str, key: &str) -> Result<(), SecretsError> {
    let provider = provider.to_string();
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        entry(&provider)?
            .set_password(&key)
            .map_err(SecretsError::Keychain)
    })
    .await
    .map_err(SecretsError::Interrupted)?
}

/// Fetch the API key for `provider`. `Ok(None)` means nothing is stored —
/// an expected state before `aarg init`, not an error.
pub async fn load_api_key(provider: &str) -> Result<Option<String>, SecretsError> {
    let provider = provider.to_string();
    tokio::task::spawn_blocking(move || match entry(&provider)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretsError::Keychain(e)),
    })
    .await
    .map_err(SecretsError::Interrupted)?
}

// No unit tests here: every code path is a thin pass-through to the OS
// keychain, and exercising that for real would read and write the
// developer's own credential store. The mock-backed command tests cover
// the callers instead.
