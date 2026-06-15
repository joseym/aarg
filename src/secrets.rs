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

/// The keychain username a `(provider, label)` pair is filed under. Each
/// provider can hold several keys, one per label (e.g. `anthropic:work`),
/// so the user can keep more than one account on hand and switch between
/// them. The keychain can't be listed portably, so the *set* of labels is
/// tracked in config (`AnthropicConfig::keys`); this module just reads and
/// writes one secret at a time by its slot.
fn slot(provider: &str, label: &str) -> String {
    format!("{provider}:{label}")
}

/// Handle to the keychain entry filed under `username`.
fn entry(username: &str) -> Result<Entry, SecretsError> {
    Entry::new(SERVICE, username).map_err(SecretsError::Keychain)
}

/// Store `key` as the API key for `provider` under `label`, replacing any
/// existing one with that label.
pub async fn store_api_key(provider: &str, label: &str, key: &str) -> Result<(), SecretsError> {
    let username = slot(provider, label);
    let key = key.to_string();
    tokio::task::spawn_blocking(move || {
        entry(&username)?
            .set_password(&key)
            .map_err(SecretsError::Keychain)
    })
    .await
    .map_err(SecretsError::Interrupted)?
}

/// Fetch the API key for `provider` under `label`. `Ok(None)` means nothing
/// is stored for that label — an expected state, not an error.
pub async fn load_api_key(provider: &str, label: &str) -> Result<Option<String>, SecretsError> {
    load_by_username(slot(provider, label)).await
}

/// Read the key from the legacy bare-provider slot (`anthropic`, no label),
/// where versions before named keys stored the single key. Used as a
/// fallback so existing users keep working without re-running `aarg init`,
/// and by `init` to adopt that key under a label.
pub async fn load_legacy_key(provider: &str) -> Result<Option<String>, SecretsError> {
    load_by_username(provider.to_string()).await
}

/// Shared body of the two loads: read one entry, mapping `NoEntry` to
/// `Ok(None)`.
async fn load_by_username(username: String) -> Result<Option<String>, SecretsError> {
    tokio::task::spawn_blocking(move || match entry(&username)?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(SecretsError::Keychain(e)),
    })
    .await
    .map_err(SecretsError::Interrupted)?
}

/// Delete the API key for `provider` under `label`. Returns `Ok(false)`
/// when there was nothing to delete — an idempotent no-op, not an error.
pub async fn delete_api_key(provider: &str, label: &str) -> Result<bool, SecretsError> {
    let username = slot(provider, label);
    tokio::task::spawn_blocking(move || match entry(&username)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(SecretsError::Keychain(e)),
    })
    .await
    .map_err(SecretsError::Interrupted)?
}

/// Delete the legacy bare-provider key, if any. Used once a legacy key has
/// been migrated to a labeled slot so it isn't left lingering.
pub async fn delete_legacy_key(provider: &str) -> Result<bool, SecretsError> {
    let username = provider.to_string();
    tokio::task::spawn_blocking(move || match entry(&username)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(e) => Err(SecretsError::Keychain(e)),
    })
    .await
    .map_err(SecretsError::Interrupted)?
}

#[cfg(test)]
mod tests {
    use super::*;

    // The store/load/delete paths are thin pass-throughs to the OS keychain;
    // exercising those for real would read and write the developer's own
    // credential store, so the mock-backed command tests cover the callers
    // instead. Only the pure slot-naming scheme is unit-tested here, because
    // its exact format is the contract that lets several keys coexist.
    #[test]
    fn slot_namespaces_label_under_provider() {
        assert_eq!(slot("anthropic", "work"), "anthropic:work");
        assert_eq!(slot("anthropic", "default"), "anthropic:default");
    }
}
