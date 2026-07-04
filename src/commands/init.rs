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

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use inquire::{Password, PasswordDisplayMode, Select, Text};
use serde_json::Value;

use crate::commands::{CliError, validate_key_label};
use crate::config::{AuthKind, Config, DEFAULT_KEY_LABEL, Provider};
use crate::{secrets, style, workspace};

/// How long to wait when listing a local server's models during setup. The list
/// is a convenience, so a slow or absent server falls back to a typed name
/// rather than stalling `init`.
const MODELS_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn run(global: bool, dir: Option<PathBuf>) -> Result<(), CliError> {
    // Decide where this workspace's config lives, and whether it's the
    // shared home config (the only place a pre-labels key migration makes
    // sense, since that legacy slot is global).
    let (config_path, is_global) = target_config_path(global, dir)?;

    // Load from the explicit path rather than via discovery: the workspace
    // may not exist yet, so discovery would find the wrong one (or none).
    let mut config = Config::load_from(&config_path)?;

    // Choose the provider. Interactive only: a piped/CI run keeps whatever the
    // config already carries (Anthropic by default), so scripted setup that
    // pre-writes config.toml is unaffected.
    if std::io::stdin().is_terminal() {
        config.provider = prompt_provider(config.provider)?;
    }

    match config.provider {
        Provider::Anthropic => setup_anthropic(&mut config, is_global).await?,
        Provider::LmStudio | Provider::Ollama => setup_local(&mut config).await?,
    }

    config.save_to(&config_path)?;
    announce(&config_path, is_global);
    eprintln!(
        "{}",
        style::suggest("next: run `aarg llm ping` to verify the connection")
    );
    Ok(())
}

/// Ask which provider to use, starting the cursor on the one already set.
fn prompt_provider(current: Provider) -> Result<Provider, CliError> {
    const ANTHROPIC: &str = "Anthropic (hosted · API key or Claude plan)";
    const LMSTUDIO: &str = "LM Studio (local · OpenAI-compatible server)";
    const OLLAMA: &str = "Ollama (local)";
    let start = match current {
        Provider::Anthropic => 0,
        Provider::LmStudio => 1,
        Provider::Ollama => 2,
    };
    let choice = Select::new("Which provider?", vec![ANTHROPIC, LMSTUDIO, OLLAMA])
        .with_starting_cursor(start)
        .prompt()?;
    Ok(match choice {
        LMSTUDIO => Provider::LmStudio,
        OLLAMA => Provider::Ollama,
        _ => Provider::Anthropic,
    })
}

/// The Anthropic setup: migrate a legacy key if this is the global config, then
/// take or reuse a stored credential.
async fn setup_anthropic(config: &mut Config, is_global: bool) -> Result<(), CliError> {
    let provider = Provider::Anthropic;
    if is_global {
        // A key stored before named keys lives in a bare slot; adopt it
        // under the default label so upgrading users keep their key.
        migrate_legacy_key(config, provider).await?;
    }
    if config.anthropic.keys.is_empty() {
        // Nothing stored yet: take a single credential under the default label.
        let kind = prompt_auth_kind()?;
        add_key(config, provider, DEFAULT_KEY_LABEL, kind).await?;
    } else {
        // Keys already exist: reuse, switch, add, or replace.
        existing_key_flow(config, provider).await?;
    }
    Ok(())
}

/// The local-provider setup: no credential, just the base URL and a model. The
/// model list is offered from the running server when it's reachable, and falls
/// back to a typed name when it isn't.
async fn setup_local(config: &mut Config) -> Result<(), CliError> {
    let provider = config.provider;
    let default_base = config.active_base_url().unwrap_or_default().to_string();
    eprintln!(
        "{}",
        style::info(
            "A local provider needs no key. Confirm where its server listens, then pick a model."
        )
    );
    let base_url = Text::new("Server base URL:")
        .with_initial_value(&default_base)
        .prompt()?
        .trim()
        .to_string();

    let model = prompt_local_model(provider, &base_url).await?;

    match provider {
        Provider::LmStudio => {
            config.lmstudio.base_url = base_url;
            config.lmstudio.model = model;
        }
        Provider::Ollama => {
            config.ollama.base_url = base_url;
            config.ollama.model = model;
        }
        Provider::Anthropic => {}
    }
    Ok(())
}

/// Pick a model for a local provider: a menu of what the server currently has
/// when it's reachable, otherwise a typed name (with a nudge to start the server
/// for the list). An empty typed name is accepted (`aarg` reports the missing
/// model on first use), so setup never gets stuck here.
async fn prompt_local_model(provider: Provider, base_url: &str) -> Result<String, CliError> {
    let models = list_server_models(provider, base_url).await;
    if models.is_empty() {
        eprintln!(
            "{}",
            style::warn(format!(
                "couldn't list models from {base_url} · type a model name (start the server first to get the list)"
            ))
        );
        return Ok(Text::new("Model name:").prompt()?.trim().to_string());
    }
    Ok(Select::new("Model:", models).prompt()?)
}

/// The model ids a local server currently serves, best-effort. LM Studio speaks
/// the OpenAI `GET /v1/models` shape; Ollama lists pulled models at
/// `GET /api/tags`. Any failure (server down, odd shape) yields an empty list,
/// which the caller degrades to a typed name.
async fn list_server_models(provider: Provider, base_url: &str) -> Vec<String> {
    let Ok(http) = reqwest::Client::builder().timeout(MODELS_TIMEOUT).build() else {
        return Vec::new();
    };
    let (url, pointer, key) = match provider {
        Provider::LmStudio => (format!("{base_url}/v1/models"), "/data", "id"),
        Provider::Ollama => (format!("{base_url}/api/tags"), "/models", "name"),
        Provider::Anthropic => return Vec::new(),
    };
    let Ok(response) = http.get(url).send().await else {
        return Vec::new();
    };
    let Ok(body) = response.text().await else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&body) else {
        return Vec::new();
    };
    parsed
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get(key).and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
        eprintln!(
            "{}",
            style::success(format!(
                "global config written to {}",
                config_path.display()
            ))
        );
        return;
    }
    // The `.aarg/` directory is the config file's parent.
    let root = config_path.parent().unwrap_or(config_path);
    eprintln!(
        "{}",
        style::success(format!("local workspace ready at {}", root.display()))
    );
    eprintln!(
        "{}",
        style::info(
            "commands run here (or in any subdirectory) use it; elsewhere aarg falls back to your global setup"
        )
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
        // A pre-labels key is always a plain API key (OAuth came later).
        config
            .anthropic
            .register_key(DEFAULT_KEY_LABEL, AuthKind::ApiKey);
        config.anthropic.active_key = Some(DEFAULT_KEY_LABEL.to_string());
        eprintln!(
            "{}",
            style::success(format!(
                "adopted your existing key under the label `{DEFAULT_KEY_LABEL}`"
            ))
        );
    }
    Ok(())
}

/// Keys already exist: present the choices and act on the chosen one.
async fn existing_key_flow(config: &mut Config, provider: Provider) -> Result<(), CliError> {
    let active = config.anthropic.active_label().to_string();
    eprintln!(
        "{}",
        style::info(format!(
            "existing keys: {} (active: {active})",
            config.anthropic.keys.join(", ")
        ))
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
            eprintln!("{}", style::success(format!("active key is now `{label}`")));
            config.anthropic.active_key = Some(label);
        }
        ADD => {
            let entered = Text::new("Label for the new key (e.g. work, personal):").prompt()?;
            let label = validate_key_label(&entered)?.to_string();
            let kind = prompt_auth_kind()?;
            add_key(config, provider, &label, kind).await?;
        }
        REPLACE => {
            let label =
                Select::new("Replace which key?", config.anthropic.keys.clone()).prompt()?;
            let kind = prompt_auth_kind()?;
            add_key(config, provider, &label, kind).await?;
        }
        _ => eprintln!("{}", style::info(format!("keeping `{active}`"))),
    }
    Ok(())
}

/// Ask which kind of credential to add: a pay-as-you-go API key, a pasted
/// Claude-plan token, or a plan delegated to the official CLI. The `_` arm
/// covers the API-key option and any unexpected value, so there's no
/// panicking catch-all.
fn prompt_auth_kind() -> Result<AuthKind, CliError> {
    const API: &str = "API key (pay-as-you-go)";
    const SUB: &str = "Claude subscription / Pro or Max — paste a token (experimental)";
    const CLI: &str =
        "Claude subscription via the `ant` CLI — auto-refresh, no stored token (experimental)";
    match Select::new("Credential type:", vec![API, SUB, CLI]).prompt()? {
        SUB => Ok(AuthKind::Oauth),
        CLI => Ok(AuthKind::Cli),
        _ => Ok(AuthKind::ApiKey),
    }
}

/// Register a credential of `kind` under `label` and make it active. API-key
/// and OAuth kinds prompt for a masked secret (never echoed, straight to the
/// keychain, never to config); the CLI-delegated kind stores no secret — a
/// token is fetched from `ant` at request time. When stdin is not a terminal
/// (scripts, CI), inquire fails with a typed error rather than hanging.
async fn add_key(
    config: &mut Config,
    provider: Provider,
    label: &str,
    kind: AuthKind,
) -> Result<(), CliError> {
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
        let prompt = if kind == AuthKind::Oauth {
            // A plan token is generated by Claude Code, not pasted from the
            // console; point the user at the command and flag the caveat.
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
        secrets::store_api_key(provider.name(), label, &secret).await?;
    }
    config.anthropic.register_key(label, kind);
    config.anthropic.active_key = Some(label.to_string());
    match kind {
        AuthKind::ApiKey => eprintln!(
            "{}",
            style::success(format!(
                "key `{label}` stored in the OS keychain and set active"
            ))
        ),
        AuthKind::Oauth => eprintln!(
            "{}",
            style::success(format!(
                "subscription token `{label}` stored in the OS keychain and set active"
            ))
        ),
        AuthKind::Cli => eprintln!(
            "{}",
            style::success(format!(
                "CLI-delegated credential `{label}` set active (token fetched via `ant` each run)"
            ))
        ),
    }
    Ok(())
}
