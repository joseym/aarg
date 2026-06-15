//! `aarg init` — set up a workspace and store a provider API key.
//!
//! By default aarg works out of a local `.aarg/` workspace in the current
//! directory (config, dataset, builds, traces, cache). `--global` targets
//! the per-user home config instead, and `--dir <path>` puts the workspace
//! at another project directory. Whichever is chosen, the config file is
//! written there directly — not through the usual discovery, which couldn't
//! yet see a workspace being created.
//!
//! Several keys can coexist, each under a label (e.g. `work`, `personal`);
//! `init` detects what's already stored and lets you reuse the active key,
//! switch the active one, add another, or replace one. The secrets live in
//! the OS keychain (the `secrets` module) and are shared across workspaces;
//! config holds only the label registry and which label is active.

use std::path::PathBuf;

use inquire::{Password, PasswordDisplayMode, Select, Text};

use crate::commands::{CliError, validate_key_label};
use crate::config::{Config, DEFAULT_KEY_LABEL, Provider};
use crate::{secrets, workspace};

pub async fn run(global: bool, dir: Option<PathBuf>) -> Result<(), CliError> {
    // Decide where this workspace's config lives, and whether it's the
    // shared home config (the only place a pre-labels key migration makes
    // sense, since that legacy slot is global).
    let (config_path, is_global) = target_config_path(global, dir)?;

    // Load from the explicit path rather than via discovery: the workspace
    // may not exist yet, so discovery would find the wrong one (or none).
    let mut config = Config::load_from(&config_path)?;
    let provider = config.provider;
    println!(
        "Provider: {} (the only provider in this build)",
        provider.name()
    );

    if is_global {
        // A key stored before named keys lives in a bare slot; adopt it
        // under the default label so upgrading users keep their key.
        migrate_legacy_key(&mut config, provider).await?;
    }

    if config.anthropic.keys.is_empty() {
        // Nothing stored yet: take a single key under the default label.
        add_key(&mut config, provider, DEFAULT_KEY_LABEL).await?;
    } else {
        // Keys already exist: reuse, switch, add, or replace.
        existing_key_flow(&mut config, provider).await?;
    }

    config.save_to(&config_path)?;
    announce(&config_path, is_global);
    println!("Try `aarg llm ping` to verify the connection.");
    Ok(())
}

/// Resolve `config.toml`'s path from the flags, returning whether it's the
/// shared home config. `--global` wins; `--dir <p>` makes a workspace at
/// `<p>/.aarg`; otherwise the current directory's `.aarg`.
fn target_config_path(global: bool, dir: Option<PathBuf>) -> Result<(PathBuf, bool), CliError> {
    if global {
        let dir = workspace::global_config_dir()
            .ok_or(CliError::Config(crate::config::ConfigError::NoHomeDir))?;
        return Ok((dir.join("config.toml"), true));
    }
    let project = match dir {
        Some(path) => path,
        None => std::env::current_dir().map_err(CliError::CurrentDir)?,
    };
    Ok((workspace::local_root(&project).join("config.toml"), false))
}

/// Tell the user where the workspace landed and what it means.
fn announce(config_path: &std::path::Path, is_global: bool) {
    if is_global {
        println!("Global config written to {}.", config_path.display());
        return;
    }
    // The `.aarg/` directory is the config file's parent.
    let root = config_path.parent().unwrap_or(config_path);
    println!("Local workspace ready at {}.", root.display());
    println!(
        "Commands run here (or in any subdirectory) use it; elsewhere aarg falls back to your global setup."
    );
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
