//! `aarg init` — first-run setup: store the provider API key in the OS
//! keychain and write a config file with defaults.

use inquire::{Password, PasswordDisplayMode};

use crate::commands::CliError;
use crate::config::Config;
use crate::secrets;

pub async fn run() -> Result<(), CliError> {
    let config = Config::load()?;
    let provider = config.provider;
    println!(
        "Provider: {} (the only provider in this build)",
        provider.name()
    );

    // The key is read masked and never echoed; it goes straight to the
    // OS keychain, never into the config file. When stdin is not a
    // terminal (scripts, CI), inquire fails with a typed NotTTY error
    // instead of hanging on input that will never come.
    let key = Password::new("API key:")
        .with_display_mode(PasswordDisplayMode::Masked)
        .without_confirmation()
        .prompt()?;
    secrets::store_api_key(provider.name(), &key).await?;

    config.save()?;
    println!("Key stored in the OS keychain.");
    println!("Config written to {}.", Config::path()?.display());
    println!("Try `aarg llm ping` to verify the connection.");
    Ok(())
}
