//! `aarg key` — manage the API keys stored in the OS keychain.
//!
//! Several keys can coexist, one per label (e.g. `work`, `personal`), so
//! you can keep more than one account on hand and switch the active one
//! without re-entering a secret. The secrets live in the keychain (the
//! `secrets` module); config holds only the label registry and which label
//! is active. A one-off `AARG_KEY=<label>` env var overrides the active
//! label for a single invocation (see `commands::configured_client`).

use inquire::{Password, PasswordDisplayMode};

use crate::commands::{CliError, validate_key_label};
use crate::config::{Config, DEFAULT_KEY_LABEL};
use crate::secrets;

/// `aarg key list` — show the stored labels, marking the active one. Never
/// prints a secret; labels only.
pub async fn list() -> Result<(), CliError> {
    let config = Config::load()?;
    let provider = config.provider;

    if config.anthropic.keys.is_empty() {
        // No labeled keys — but a pre-labels key may still be in the bare
        // slot, in which case it's what requests actually use.
        match secrets::load_legacy_key(provider.name()).await? {
            Some(_) => {
                println!("One unlabeled key is stored (from before named keys).");
                println!("Run `aarg init` to adopt it under a label.");
            }
            None => println!("No keys stored. Run `aarg init` or `aarg key add <label>`."),
        }
        return Ok(());
    }

    let active = config.anthropic.active_label();
    println!("Stored keys for {}:", provider.name());
    for label in &config.anthropic.keys {
        let marker = if label == active { " (active)" } else { "" };
        println!("  {label}{marker}");
    }
    if let Ok(env_label) = std::env::var("AARG_KEY") {
        println!("note: AARG_KEY overrides the active key to `{env_label}` in this shell.");
    }
    Ok(())
}

/// `aarg key add [label]` — prompt for a key and store it under `label`
/// (or `default` when omitted). The key is read masked, never echoed, and
/// goes straight to the keychain. Adding the first key makes it active;
/// adding a further one leaves the active key alone.
pub async fn add(label: Option<String>) -> Result<(), CliError> {
    let mut config = Config::load()?;
    let provider = config.provider;
    let label = match &label {
        Some(label) => validate_key_label(label)?.to_string(),
        None => DEFAULT_KEY_LABEL.to_string(),
    };
    let replacing = config.anthropic.keys.iter().any(|stored| stored == &label);

    let key = Password::new(&format!("API key for `{label}`:"))
        .with_display_mode(PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()?;
    secrets::store_api_key(provider.name(), &label, &key).await?;
    config.anthropic.register_key(&label);

    // Make it active only when there's no clear active key yet (first key in,
    // or none chosen) — adding a second key shouldn't silently switch.
    let became_active = config.anthropic.active_key.is_none() || config.anthropic.keys.len() == 1;
    if became_active {
        config.anthropic.active_key = Some(label.clone());
    }
    config.save()?;

    let verb = if replacing { "replaced" } else { "stored" };
    println!("Key `{label}` {verb} in the OS keychain.");
    if became_active {
        println!("It is now the active key.");
    } else {
        println!(
            "Active key unchanged ({}). Switch with `aarg key use {label}`.",
            config.anthropic.active_label()
        );
    }
    Ok(())
}

/// `aarg key use <label>` — make a stored key the active one.
pub async fn use_key(label: String) -> Result<(), CliError> {
    let mut config = Config::load()?;
    if !config.anthropic.keys.iter().any(|stored| stored == &label) {
        return Err(CliError::NoSuchKey { label });
    }
    config.anthropic.active_key = Some(label.clone());
    config.save()?;
    println!("Active key is now `{label}`.");
    Ok(())
}

/// `aarg key remove <label>` — delete a stored key from both the keychain
/// and the config registry. Clearing the active key reports what now
/// resolves in its place.
pub async fn remove(label: String) -> Result<(), CliError> {
    let mut config = Config::load()?;
    let provider = config.provider;
    if !config.anthropic.keys.iter().any(|stored| stored == &label) {
        return Err(CliError::NoSuchKey { label });
    }

    secrets::delete_api_key(provider.name(), &label).await?;
    let was_active = config.anthropic.active_key.as_deref() == Some(label.as_str());
    config.anthropic.forget_key(&label);
    config.save()?;

    println!("Removed key `{label}`.");
    if was_active {
        if config.anthropic.keys.is_empty() {
            println!("No keys remain — run `aarg init` or `aarg key add <label>` to add one.");
        } else {
            println!(
                "Active key cleared; it now resolves to `{}`. Pin one with `aarg key use <label>`.",
                config.anthropic.active_label()
            );
        }
    }
    Ok(())
}
