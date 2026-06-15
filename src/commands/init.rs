//! `aarg init` — first-run setup: store a provider API key in the OS
//! keychain and write a config file with defaults.
//!
//! Several keys can coexist, each under a label (e.g. `work`, `personal`);
//! `init` detects what's already stored and lets you reuse the active key,
//! switch the active one, add another, or replace one. The secrets live in
//! the OS keychain (the `secrets` module); config holds only the label
//! registry and which label is active.

use inquire::{Password, PasswordDisplayMode, Select, Text};

use crate::commands::{CliError, validate_key_label};
use crate::config::{Config, DEFAULT_KEY_LABEL, Provider};
use crate::secrets;

pub async fn run() -> Result<(), CliError> {
    let mut config = Config::load()?;
    let provider = config.provider;
    println!(
        "Provider: {} (the only provider in this build)",
        provider.name()
    );

    // A key stored before named keys lives in a bare slot; adopt it under
    // the default label so upgrading users keep their key and gain labels.
    migrate_legacy_key(&mut config, provider).await?;

    if config.anthropic.keys.is_empty() {
        // Nothing stored yet: take a single key under the default label.
        add_key(&mut config, provider, DEFAULT_KEY_LABEL).await?;
    } else {
        // Keys already exist: reuse, switch, add, or replace.
        existing_key_flow(&mut config, provider).await?;
    }

    config.save()?;
    println!("Config written to {}.", Config::path()?.display());
    println!("Try `aarg llm ping` to verify the connection.");
    Ok(())
}

/// Move a legacy bare-slot key into the labeled scheme (as `default`) when
/// no labeled keys exist yet. A no-op once labels are in use.
async fn migrate_legacy_key(config: &mut Config, provider: Provider) -> Result<(), CliError> {
    if !config.anthropic.keys.is_empty() {
        return Ok(());
    }
    if let Some(key) = secrets::load_legacy_key(provider.name()).await? {
        secrets::store_api_key(provider.name(), DEFAULT_KEY_LABEL, &key).await?;
        secrets::delete_legacy_key(provider.name()).await?;
        config.anthropic.register_key(DEFAULT_KEY_LABEL);
        config.anthropic.active_key = Some(DEFAULT_KEY_LABEL.to_string());
        println!("Adopted your existing key under the label `{DEFAULT_KEY_LABEL}`.");
    }
    Ok(())
}

/// Keys already exist: present the choices and act on the chosen one.
async fn existing_key_flow(config: &mut Config, provider: Provider) -> Result<(), CliError> {
    let active = config.anthropic.active_label().to_string();
    println!(
        "Existing keys: {} (active: {active})",
        config.anthropic.keys.join(", ")
    );

    // Static option strings; matched below. `_` covers "reuse" and any
    // unexpected value, so there's no panicking catch-all.
    const SWITCH: &str = "Switch the active key";
    const ADD: &str = "Add another key";
    const REPLACE: &str = "Replace a key";
    let reuse = format!("Keep using the active key ({active})");

    let mut actions = vec![reuse.as_str()];
    if config.anthropic.keys.len() > 1 {
        actions.push(SWITCH);
    }
    actions.push(ADD);
    actions.push(REPLACE);

    match Select::new("What would you like to do?", actions).prompt()? {
        SWITCH => {
            let label = Select::new("Use which key?", config.anthropic.keys.clone()).prompt()?;
            println!("Active key is now `{label}`.");
            config.anthropic.active_key = Some(label);
        }
        ADD => {
            let entered = Text::new("Label for the new key (e.g. work, personal):").prompt()?;
            let label = validate_key_label(&entered)?.to_string();
            add_key(config, provider, &label).await?;
        }
        REPLACE => {
            let label =
                Select::new("Replace which key?", config.anthropic.keys.clone()).prompt()?;
            add_key(config, provider, &label).await?;
        }
        _ => println!("Keeping `{active}`."),
    }
    Ok(())
}

/// Prompt for a masked key, store it under `label`, and make that label the
/// active one. The key is read masked, never echoed, and goes straight to
/// the OS keychain — never into the config file. When stdin is not a
/// terminal (scripts, CI), inquire fails with a typed error rather than
/// hanging on input that will never come.
async fn add_key(config: &mut Config, provider: Provider, label: &str) -> Result<(), CliError> {
    let key = Password::new(&format!("API key for `{label}`:"))
        .with_display_mode(PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()?;
    secrets::store_api_key(provider.name(), label, &key).await?;
    config.anthropic.register_key(label);
    config.anthropic.active_key = Some(label.to_string());
    println!("Key `{label}` stored in the OS keychain and set active.");
    Ok(())
}
