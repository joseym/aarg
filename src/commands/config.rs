//! `aarg config` — show the effective configuration and where it comes
//! from. Read-only: the file is edited by hand or via `aarg init`.

use crate::agent::ModelTier;
use crate::commands::CliError;
use crate::config::{Config, Provider};
use crate::secrets;
use crate::style;

pub async fn run() -> Result<(), CliError> {
    let path = Config::path()?;
    let config = Config::load()?;
    let file_exists = path.exists();

    // Human status report on stderr (the stream the color helpers detect on);
    // this is a read-only display command with no machine mode.
    eprintln!("{}", style::section("Workspace"));
    // Width fits the longest label in this block ("config file") so the two
    // value columns line up.
    eprintln!(
        "{}",
        style::kv("workspace", crate::workspace::locate().describe(), 12)
    );
    eprintln!(
        "{}",
        style::kv(
            "config file",
            format!(
                "{}{}",
                path.display(),
                if file_exists {
                    String::new()
                } else {
                    style::dim(" (not created yet; showing defaults)")
                }
            ),
            12
        )
    );
    // Surface what is steering resolution, so a surprising location is
    // debuggable: the `AARG_DIR` env var and/or a `workspace` set in the
    // global config (the file-based equivalent of the env var).
    if let Some(env_dir) = std::env::var_os(crate::workspace::DIR_ENV)
        && !env_dir.is_empty()
    {
        eprintln!("{}", style::kv("AARG_DIR", env_dir.to_string_lossy(), 12));
    }
    if let Some(configured) = crate::workspace::configured_workspace() {
        eprintln!(
            "{}",
            style::kv(
                "configured",
                format!("{} (from global config)", configured.display()),
                12
            )
        );
    }

    eprintln!("{}", style::section("Provider"));
    eprintln!("{}", style::kv("provider", config.provider.name(), 9));
    match config.provider {
        Provider::Anthropic => show_anthropic(&config).await,
        Provider::LmStudio | Provider::Ollama => show_local(&config),
    }

    // The active resolver maps each agent's tier to a concrete model; a
    // representative agent id per tier is enough to show what resolves, since
    // the id only matters when a per-agent pin exists.
    let resolver = config.active_resolver();
    eprintln!("{}", style::section("Model tiers"));
    eprintln!(
        "{}",
        style::kv(
            "cheap",
            format!(
                "{} {}",
                tier_display(resolver.resolve("jd_parser_v1", ModelTier::Cheap)),
                style::dim("(parse/match)")
            ),
            7
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "mid",
            format!(
                "{} {}",
                tier_display(resolver.resolve("metric_interview_v1", ModelTier::Mid)),
                style::dim("(interview)")
            ),
            7
        )
    );
    eprintln!(
        "{}",
        style::kv(
            "smart",
            format!(
                "{} {}",
                tier_display(resolver.resolve("tailoring_v1", ModelTier::Smart)),
                style::dim("(tailor/review)")
            ),
            7
        )
    );
    show_agent_overrides(&config);

    let limits = &config.limits;
    eprintln!("{}", style::section("Limits"));
    eprintln!("{}", style::kv("revisions", limits.revisions, 21));
    eprintln!(
        "{}",
        style::kv(
            "acceptable score",
            format!("{:.2}", limits.acceptable_score),
            21
        )
    );
    eprintln!(
        "{}",
        style::kv("strengthen questions", limits.strengthen_questions, 21)
    );
    eprintln!(
        "{}",
        style::kv("strengthen revises", limits.strengthen_revises, 21)
    );
    eprintln!(
        "{}",
        style::kv(
            "budget",
            match limits.budget_usd {
                Some(b) => format!("${b:.2} per build"),
                None => "none".to_string(),
            },
            21
        )
    );

    eprintln!("{}", style::section("Export"));
    eprintln!(
        "{}",
        style::kv(
            "destination",
            match &config.export.dir {
                Some(dir) => dir.display().to_string(),
                None => "current directory (set `export.dir`, or pass `--to`)".to_string(),
            },
            11
        )
    );

    eprintln!("{}", style::section("Render"));
    eprintln!(
        "{}",
        style::kv(
            "typst",
            match &config.render.typst {
                Some(path) => path.clone(),
                None => "auto (PATH, then next to aarg)".to_string(),
            },
            7
        )
    );
    Ok(())
}

/// A resolved tier value for display: the model id, or a hint when a local
/// provider has none set (its tiers resolve to the empty string).
fn tier_display(model: &str) -> String {
    if model.is_empty() {
        style::warn("not set")
    } else {
        model.to_string()
    }
}

/// The Anthropic provider block: fallback model, the credential's keychain
/// status, the stored labels, and any headless env override in effect.
async fn show_anthropic(config: &Config) {
    eprintln!(
        "{}",
        style::kv(
            "model",
            format!(
                "{} {}",
                config.anthropic.model,
                style::dim("(fallback for unpinned tiers)")
            ),
            9
        )
    );
    eprintln!(
        "{}",
        style::kv("api key", anthropic_key_status(config).await, 9)
    );
    if !config.anthropic.keys.is_empty() {
        // List the labels (never the secrets), marking the active one and
        // tagging non-API-key kinds.
        let active = config.anthropic.active_label();
        let labels: Vec<String> = config
            .anthropic
            .keys
            .iter()
            .map(|label| {
                let kind_tag = match config.anthropic.kind_for(label) {
                    crate::config::AuthKind::ApiKey => "",
                    crate::config::AuthKind::Oauth => " (oauth)",
                    crate::config::AuthKind::Cli => " (cli)",
                };
                let active_marker = if label == active { " (active)" } else { "" };
                format!("{label}{kind_tag}{active_marker}")
            })
            .collect();
        eprintln!("{}", style::kv("keys", labels.join(", "), 9));
    }
    // The headless path overrides everything above; say so if it's in effect.
    // The var names are configurable, so report the names actually checked.
    let auth_token_env = config.anthropic.auth_token_env();
    let api_key_env = config.anthropic.api_key_env();
    if std::env::var_os(auth_token_env).is_some() {
        eprintln!(
            "{}",
            style::info(format!(
                "{auth_token_env} is set · requests use that OAuth token, not the keychain."
            ))
        );
    } else if std::env::var_os(api_key_env).is_some() {
        eprintln!(
            "{}",
            style::info(format!(
                "{api_key_env} is set · requests use that key, not the keychain."
            ))
        );
    }
}

/// The keychain status of the active Anthropic label: presence only, never the
/// secret. Kept from failing the read-only command when the keychain is
/// unreachable (a headless Linux box with no Secret Service daemon).
async fn anthropic_key_status(config: &Config) -> String {
    let label = config.anthropic.active_label();
    let kind = config.anthropic.kind_for(label);
    let kind_str = match kind {
        crate::config::AuthKind::ApiKey => "API key",
        crate::config::AuthKind::Oauth => "OAuth / subscription",
        crate::config::AuthKind::Cli => "CLI-delegated",
    };
    // A CLI-delegated credential has no stored secret; its token is fetched
    // by running the configured command at request time, so don't probe the
    // keychain for it. Show the actual command (the default `ant …` or a
    // per-label override) rather than assuming `ant`.
    if kind == crate::config::AuthKind::Cli {
        let command = config.anthropic.credential_command(label).join(" ");
        return style::success(format!(
            "delegated to `{command}` {}",
            style::dim(format!("(label: {label}, {kind_str})"))
        ));
    }
    match secrets::load_api_key(config.provider.name(), label).await {
        Ok(Some(_)) => style::success(format!(
            "stored in the OS keychain {}",
            style::dim(format!("(label: {label}, {kind_str})"))
        )),
        // Nothing under the active label; a legacy bare-slot key may still
        // be in play for users who haven't re-run init.
        Ok(None) if config.anthropic.keys.is_empty() => {
            match secrets::load_legacy_key(config.provider.name()).await {
                Ok(Some(_)) => style::warn(
                    "stored in the OS keychain (legacy slot; run `aarg init` to label it)",
                ),
                Ok(None) => style::suggest("not set · run `aarg init`"),
                Err(error) => style::warn(format!("unknown ({error})")),
            }
        }
        Ok(None) => style::suggest(format!(
            "not set for label `{label}` · run `aarg key add {label}`"
        )),
        Err(error) => style::warn(format!("unknown ({error})")),
    }
}

/// The local-provider block: base URL, the fallback model (with a nudge when it
/// isn't set yet), and, for Ollama, the context floor and keep-alive. No
/// credential is shown: a local server needs none. The live loaded window is a
/// server-side runtime fact, so point at `aarg llm ping` rather than probe it
/// from this read-only command.
fn show_local(config: &Config) {
    let (base_url, model) = match config.provider {
        Provider::LmStudio => (
            config.lmstudio.base_url.as_str(),
            config.lmstudio.model.as_str(),
        ),
        Provider::Ollama => (
            config.ollama.base_url.as_str(),
            config.ollama.model.as_str(),
        ),
        Provider::Anthropic => return,
    };
    eprintln!("{}", style::kv("base url", base_url, 9));
    let model_line = if model.is_empty() {
        style::warn(format!(
            "not set · add `model = \"…\"` under [{}] in config.toml",
            config.provider.name()
        ))
    } else {
        format!("{model} {}", style::dim("(fallback for unpinned tiers)"))
    };
    eprintln!("{}", style::kv("model", model_line, 9));
    if config.provider == Provider::Ollama {
        let ctx = match config.ollama.num_ctx {
            Some(n) => format!("{n} tokens"),
            None => "8192 tokens (default)".to_string(),
        };
        eprintln!("{}", style::kv("context", ctx, 9));
        if let Some(keep_alive) = &config.ollama.keep_alive {
            eprintln!("{}", style::kv("keep alive", keep_alive, 9));
        }
    }
    eprintln!(
        "{}",
        style::info(
            "no key needed for a local provider · run `aarg llm ping` to verify the server and its loaded context window"
        )
    );
}

/// The per-agent model overrides for the active provider, if any.
fn show_agent_overrides(config: &Config) {
    let agents = match config.provider {
        Provider::Anthropic => &config.anthropic.agents,
        Provider::LmStudio => &config.lmstudio.agents,
        Provider::Ollama => &config.ollama.agents,
    };
    if !agents.is_empty() {
        eprintln!("{}", style::section("Per-agent overrides"));
        for (agent_id, model) in agents {
            eprintln!("  {}", style::bullet(format!("{agent_id}: {model}")));
        }
    }
}
