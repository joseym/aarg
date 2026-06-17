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
use crate::config::{AuthKind, Config, DEFAULT_KEY_LABEL};
use crate::secrets;
use crate::style;

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
                eprintln!(
                    "{}",
                    style::info("One unlabeled key is stored (from before named keys).")
                );
                eprintln!(
                    "{}",
                    style::suggest("run `aarg init` to adopt it under a label")
                );
            }
            None => eprintln!(
                "{}",
                style::suggest("no keys stored · run `aarg init` or `aarg key add <label>`")
            ),
        }
        return Ok(());
    }

    let active = config.anthropic.active_label();
    eprintln!(
        "{}",
        style::section(format!("Stored keys for {}", provider.name()))
    );
    for label in &config.anthropic.keys {
        let marker = if label == active {
            style::dim(" (active)")
        } else {
            String::new()
        };
        eprintln!("  {}", style::bullet(format!("{label}{marker}")));
    }
    if let Ok(env_label) = std::env::var("AARG_KEY") {
        eprintln!(
            "{}",
            style::info(format!(
                "AARG_KEY overrides the active key to `{env_label}` in this shell."
            ))
        );
    }
    Ok(())
}

/// `aarg key add [label]` — add a credential under `label` (or `default`).
/// An API key (default) or `--oauth` token is read masked and stored in the
/// keychain; `--cli` records a delegation that stores no secret and fetches
/// a token from `ant` each run. Adding the first key makes it active; adding
/// a further one leaves the active key alone.
pub async fn add(label: Option<String>, oauth: bool, cli: bool) -> Result<(), CliError> {
    let mut config = Config::load()?;
    let provider = config.provider;
    let label = match &label {
        Some(label) => validate_key_label(label)?.to_string(),
        None => DEFAULT_KEY_LABEL.to_string(),
    };
    let replacing = config.anthropic.keys.iter().any(|stored| stored == &label);
    // `--cli` delegates to the official CLI (no stored secret); `--oauth`
    // stores a pasted plan token (bearer auth); the default is an API key.
    let kind = if cli {
        AuthKind::Cli
    } else if oauth {
        AuthKind::Oauth
    } else {
        AuthKind::ApiKey
    };

    if kind == AuthKind::Cli {
        eprintln!(
            "{}",
            style::warn(
                "Claude subscription auth is experimental: Anthropic scopes plan credit to the official SDK and Claude Code, not third-party tools."
            )
        );
        eprintln!(
            "{}",
            style::info(
                "AARG will fetch a token via `ant auth print-credentials` each run · make sure `ant auth login` is done. Nothing is stored in the keychain."
            )
        );
    } else {
        let prompt = if oauth {
            eprintln!(
                "{}",
                style::warn(
                    "Claude subscription auth is experimental: Anthropic scopes plan credit to the official SDK and Claude Code, not third-party tools."
                )
            );
            eprintln!(
                "{}",
                style::info("Generate a token with `claude setup-token`, then paste it below.")
            );
            format!("OAuth token for `{label}`:")
        } else {
            format!("API key for `{label}`:")
        };
        let secret = Password::new(&prompt)
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .prompt()?;
        secrets::store_api_key(provider.name(), &label, &secret).await?;
    }
    config.anthropic.register_key(&label, kind);

    // Make it active only when there's no clear active key yet (first key in,
    // or none chosen) — adding a second key shouldn't silently switch.
    let became_active = config.anthropic.active_key.is_none() || config.anthropic.keys.len() == 1;
    if became_active {
        config.anthropic.active_key = Some(label.clone());
    }
    config.save()?;

    let verb = if replacing { "replaced" } else { "added" };
    let what = match kind {
        AuthKind::ApiKey => format!("Key `{label}` {verb} in the OS keychain"),
        AuthKind::Oauth => format!("Subscription token `{label}` {verb} in the OS keychain"),
        AuthKind::Cli => {
            format!(
                "CLI-delegated credential `{label}` {verb} (token fetched via `ant` each run; nothing stored)"
            )
        }
    };
    eprintln!("{}", style::success(format!("{what}.")));
    if became_active {
        eprintln!("{}", style::info("It is now the active credential."));
    } else {
        eprintln!(
            "{}",
            style::info(format!(
                "active credential unchanged ({})",
                config.anthropic.active_label()
            ))
        );
        eprintln!(
            "{}",
            style::suggest(format!("switch with `aarg key use {label}`"))
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
    eprintln!("{}", style::success(format!("active key is now `{label}`")));
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

    eprintln!("{}", style::success(format!("removed key `{label}`")));
    if was_active {
        if config.anthropic.keys.is_empty() {
            eprintln!(
                "{}",
                style::suggest(
                    "no keys remain · run `aarg init` or `aarg key add <label>` to add one"
                )
            );
        } else {
            eprintln!(
                "{}",
                style::info(format!(
                    "active key cleared; it now resolves to `{}`",
                    config.anthropic.active_label()
                ))
            );
            eprintln!("{}", style::suggest("pin one with `aarg key use <label>`"));
        }
    }
    Ok(())
}
